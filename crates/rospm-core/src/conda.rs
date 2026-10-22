//! Existing environments are checked only; managed environments live in the
//! project and are prepared by apply, which installs Conda itself when the
//! host has no compatible one. All subprocesses, downloads and writes happen
//! when the plan executes, never while constructing or rendering a preview.
//!
//! `lock` records a managed environment as an explicit lock per platform
//! (`conda list --explicit --sha256`); `apply --locked` replays it with
//! `conda create --file`, which installs exactly those packages without
//! solving. PyPI packages are outside explicit locks, so they must be exact
//! pins in the definition and are installed afterwards with `--no-deps`.

mod bootstrap;
mod definition;
mod explicit;
mod system;
#[cfg(test)]
mod tests;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::lock::{sha256_hex, Lock};
use crate::manifest::CondaBuild;
use crate::msg;
use crate::pixi::conda_platform;
use crate::plan::{shell_quote, Command};
use crate::platform::Platform;
use crate::probe::CondaHost;

use bootstrap::{compatible, miniforge, Bootstrap};
use definition::environment_yaml;
use explicit::replay;
use system::{Real, System};

pub use definition::definition_sha256;
pub use explicit::{lock_path, resolve_lock};

const MANAGED_PREFIX: &str = ".rospm/conda/env";
const DEFINITION: &str = ".rospm/conda/environment.yml";
const READY: &str = ".rospm/conda/ready.sha256";
const PENDING: &str = "pending\n";

#[derive(Debug, Clone)]
pub enum Selection {
    Name(String),
    Prefix(PathBuf),
}

impl Selection {
    pub fn from_config(config: &CondaBuild, root: &Path) -> Result<Self> {
        config.validate()?;
        Ok(if let Some(name) = &config.name {
            Self::Name(name.clone())
        } else {
            Self::Prefix(
                root.join(
                    config
                        .prefix
                        .as_deref()
                        .unwrap_or(Path::new(MANAGED_PREFIX)),
                ),
            )
        })
    }

    /// `conda run` through `program`, which is `conda` on PATH or the
    /// bootstrapped executable.
    pub fn command(&self, program: &str) -> Command {
        let cmd = Command::new(program).args(["run", "--no-capture-output"]);
        match self {
            Self::Name(name) => cmd.args(["-n", name]),
            Self::Prefix(prefix) => cmd.arg("-p").arg(prefix.to_string_lossy()),
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Name(name) => format!("-n {}", shell_quote(name)),
            Self::Prefix(prefix) => format!("-p {}", shell_quote(&prefix.to_string_lossy())),
        }
    }
}

#[derive(Debug, Clone)]
struct Preparation {
    root: PathBuf,
    prefix: PathBuf,
    definition: PathBuf,
    ready: PathBuf,
    contents: String,
    fingerprint: String,
    platform: Platform,
    source: Option<PathBuf>,
    variables: IndexMap<String, String>,
    /// `apply --locked`: install from the lock instead of solving.
    replay: Option<Replay>,
}

#[derive(Debug, Clone)]
struct Replay {
    lock: PathBuf,
    lock_sha256: String,
    /// `name==version`, installed with `--no-deps` after the Conda packages.
    pip: Vec<String>,
}

impl Preparation {
    /// Completion record: the definition digest, followed by the lock digest
    /// when the environment was replayed from a lock.
    fn stamp(&self) -> String {
        let head = stamp_head(&self.fingerprint, self.platform);
        match &self.replay {
            Some(replay) => format!("{head}\n{}", replay.lock_sha256),
            None => head,
        }
    }

    /// A replayed environment also satisfies its definition, so build and
    /// plain apply reuse it; a solved one never satisfies a lock.
    fn is_ready(&self) -> bool {
        let Ok(ready) = fs::read_to_string(&self.ready) else {
            return false;
        };
        self.prefix.join("conda-meta/history").is_file()
            && match &self.replay {
                Some(_) => ready == self.stamp(),
                None => ready.lines().next() == Some(&stamp_head(&self.fingerprint, self.platform)),
            }
    }
}

/// First line of the completion record: the definition digest and the Conda
/// subdir, so a prefix copied from another architecture is never reused.
fn stamp_head(fingerprint: &str, platform: Platform) -> String {
    format!("{fingerprint} {}", conda_platform(platform))
}

/// One plan step. `prepare` is false for build: it may only check readiness.
#[derive(Debug, Clone)]
pub struct CondaStep {
    selection: Selection,
    managed: Option<Preparation>,
    /// Managed environments only; `None` on platforms without an installer.
    bootstrap: Option<Bootstrap>,
    /// Chosen while planning from filesystem facts: a completed bootstrap
    /// wins, so apply and later builds keep using the Conda that created the
    /// environment; otherwise `conda` from PATH, checked when executed.
    program: String,
    prepare: bool,
}

impl CondaStep {
    /// `lock` is the lock of `apply --locked`; a managed environment is then
    /// replayed from its record for `platform` or the plan fails.
    pub fn new(
        config: &CondaBuild,
        root: &Path,
        host: &CondaHost,
        platform: Platform,
        prepare: bool,
        lock: Option<&Lock>,
    ) -> Result<Self> {
        let selection = Selection::from_config(config, root)?;
        let managed = if let Some(managed) = &config.managed {
            let definition = environment_yaml(managed, root)?;
            let fingerprint = sha256_hex(&definition.contents);
            let replay = match lock.filter(|_| prepare) {
                Some(lock) => Some(replay(
                    managed,
                    &definition,
                    &fingerprint,
                    lock,
                    root,
                    platform,
                )?),
                None => None,
            };
            Some(Preparation {
                root: root.to_path_buf(),
                prefix: root.join(MANAGED_PREFIX),
                definition: root.join(DEFINITION),
                ready: root.join(READY),
                fingerprint,
                platform,
                contents: definition.contents,
                source: managed.environment_file.as_ref().map(|p| root.join(p)),
                variables: definition.variables,
                replay,
            })
        } else {
            None
        };
        let bootstrap = managed
            .as_ref()
            .and_then(|_| Bootstrap::new(host, platform));
        let program = match &bootstrap {
            Some(b) if b.is_ready() => b.conda().to_string_lossy().into_owned(),
            _ => "conda".to_string(),
        };
        Ok(Self {
            selection,
            managed,
            bootstrap,
            program,
            prepare,
        })
    }

    /// The `conda run` prefix every build command goes through.
    pub fn command(&self) -> Command {
        self.selection.command(&self.program)
    }

    /// Preview includes the normalized definition and exact creation command.
    pub fn render(&self) -> String {
        let check = msg!("conda.check", selection = self.selection.label());
        let Some(managed) = &self.managed else {
            return check.to_string();
        };
        if !self.prepare {
            return format!(
                "{check}; {}",
                msg!("conda.require_ready", path = managed.ready.display())
            );
        }
        let source = managed
            .source
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "build.conda.managed".to_string());
        let tool = match &self.bootstrap {
            _ if self.program != "conda" => msg!("conda.tool_ready", conda = self.program),
            Some(bootstrap) => crate::Message::text(bootstrap.render()),
            None => msg!("conda.tool_path_only", minimum = miniforge().minimum_conda),
        };
        let prefix = managed.prefix.display();
        if managed.is_ready() {
            return format!("{tool}\n{}", msg!("conda.reused", prefix = prefix));
        }
        // What perform will do with a prefix that is already there.
        let tool = match (
            fs::symlink_metadata(&managed.prefix).is_ok(),
            managed.ready.exists(),
        ) {
            (false, _) => tool.to_string(),
            (true, true) => format!("{tool}\n{}", msg!("conda.recreate", prefix = prefix)),
            (true, false) => format!(
                "{tool}\n{}",
                msg!("conda.managed_conflict", prefix = prefix)
            ),
        };
        if let Some(replay) = &managed.replay {
            let mut lines = vec![
                tool.to_string(),
                msg!(
                    "conda.replay",
                    prefix = managed.prefix.display(),
                    lock = replay.lock.display(),
                    sha256 = replay.lock_sha256
                )
                .to_string(),
            ];
            lines.extend(
                replay_commands(managed, replay, &self.selection, &self.program)
                    .iter()
                    .map(Command::render),
            );
            return lines.join("\n");
        }
        format!(
            "{tool}\n{}\n{}\n{}\n{}",
            msg!(
                "conda.prepare",
                prefix = managed.prefix.display(),
                source = source
            ),
            msg!("conda.definition", path = managed.definition.display()),
            managed.contents.trim_end(),
            creation_command(managed, &self.program).render()
        )
    }

    pub fn perform(&self, out: &mut dyn Write) -> Result<()> {
        self.perform_with(out, &Real)
    }

    /// The Conda that prepares or runs the managed environment. Only apply
    /// may install one; build reports what is missing instead.
    fn managed_conda(&self, out: &mut dyn Write, system: &dyn System) -> Result<String> {
        if self.program != "conda" {
            return Ok(self.program.clone());
        }
        let problem = match system.query("conda", &["--version"]) {
            Ok(found) if compatible(&found) => return Ok(self.program.clone()),
            Ok(found) => msg!(
                "conda.tool_incompatible",
                found = found.trim(),
                minimum = miniforge().minimum_conda
            ),
            Err(error) => error.message(),
        };
        let Some(bootstrap) = self.bootstrap.as_ref().filter(|_| self.prepare) else {
            return Err(Error::Host(msg!(
                "conda.tool_unavailable",
                problem = problem,
                hint = if self.prepare {
                    msg!("conda.no_installer", minimum = miniforge().minimum_conda)
                } else {
                    msg!("conda.run_apply")
                }
            )));
        };
        writeln!(out, "{problem}").map_err(|e| Error::io("<stdout>", e))?;
        bootstrap.install(out, system)?;
        Ok(bootstrap.conda().to_string_lossy().into_owned())
    }

    fn perform_with(&self, out: &mut dyn Write, system: &dyn System) -> Result<()> {
        if let Some(managed) = &self.managed {
            let conda = self.managed_conda(out, system)?;
            if managed.is_ready() {
                writeln!(
                    out,
                    "{}",
                    msg!("conda.reused", prefix = managed.prefix.display())
                )
                .map_err(|e| Error::io("<stdout>", e))?;
                return Ok(());
            }
            if !self.prepare {
                return Err(Error::Host(msg!(
                    "conda.managed_not_ready",
                    prefix = managed.prefix.display()
                )));
            }
            // A completion record, even a pending or outdated one, means rospm
            // created the prefix: an interrupted or outdated environment is
            // recreated. Never adopt or overwrite a directory without one.
            if fs::symlink_metadata(&managed.prefix).is_ok() {
                if !managed.ready.exists() {
                    return Err(Error::Host(msg!(
                        "conda.managed_conflict",
                        prefix = managed.prefix.display()
                    )));
                }
                writeln!(
                    out,
                    "{}",
                    msg!("conda.recreate", prefix = managed.prefix.display())
                )
                .map_err(|e| Error::io("<stdout>", e))?;
                // A half-removed prefix must never match its old record.
                fs::write(&managed.ready, PENDING).map_err(|e| Error::io(&managed.ready, e))?;
                fs::remove_dir_all(&managed.prefix).map_err(|e| Error::io(&managed.prefix, e))?;
            }
            let parent = managed
                .definition
                .parent()
                .expect("project definition has a parent");
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
            // Claims the prefix before creation; never a valid stamp.
            fs::write(&managed.ready, PENDING).map_err(|e| Error::io(&managed.ready, e))?;
            fs::write(&managed.definition, &managed.contents)
                .map_err(|e| Error::io(&managed.definition, e))?;
            match &managed.replay {
                Some(replay) => {
                    for command in replay_commands(managed, replay, &self.selection, &conda) {
                        system.run(&command)?;
                    }
                }
                None => system.run(&creation_command(managed, &conda))?,
            }
            if !managed.prefix.join("conda-meta/history").is_file() {
                return Err(Error::Host(msg!(
                    "conda.managed_not_ready",
                    prefix = managed.prefix.display()
                )));
            }
            fs::write(&managed.ready, managed.stamp()).map_err(|e| Error::io(&managed.ready, e))?;
            return Ok(());
        }

        let text = system.query("conda", &["env", "list", "--json"])?;
        let envs: EnvironmentList = serde_json::from_str(&text)
            .map_err(|e| Error::Host(msg!("conda.query_invalid", error = e)))?;
        let exists = match &self.selection {
            Selection::Prefix(prefix) => prefix.join("conda-meta/history").is_file(),
            Selection::Name(name) if name == "base" => {
                let output = system.query("conda", &["info", "--base"])?;
                Path::new(output.trim())
                    .join("conda-meta/history")
                    .is_file()
            }
            Selection::Name(name) => envs.envs.iter().any(|p| {
                p.file_name().is_some_and(|n| n == name.as_str())
                    && p.join("conda-meta/history").is_file()
            }),
        };
        if !exists {
            return Err(Error::Host(msg!(
                "conda.existing_missing",
                selection = self.selection.label()
            )));
        }
        Ok(())
    }
}

fn creation_command(managed: &Preparation, conda: &str) -> Command {
    Command::new(conda)
        .args(["env", "create", "--yes", "--prefix"])
        .arg(managed.prefix.to_string_lossy())
        .arg("--file")
        .arg(managed.definition.to_string_lossy())
        .cwd(&managed.root)
}

/// No solving: `conda create --file` installs exactly the explicit lock and
/// verifies each package digest. Definition variables and PyPI pins, which an
/// explicit lock cannot hold, follow.
fn replay_commands(
    managed: &Preparation,
    replay: &Replay,
    selection: &Selection,
    conda: &str,
) -> Vec<Command> {
    let mut commands = vec![Command::new(conda)
        .args(["create", "--yes", "--prefix"])
        .arg(managed.prefix.to_string_lossy())
        .arg("--file")
        .arg(replay.lock.to_string_lossy())
        .cwd(&managed.root)];
    if !managed.variables.is_empty() {
        commands.push(
            Command::new(conda)
                .args(["env", "config", "vars", "set", "--prefix"])
                .arg(managed.prefix.to_string_lossy())
                .args(managed.variables.iter().map(|(k, v)| format!("{k}={v}")))
                .cwd(&managed.root),
        );
    }
    if !replay.pip.is_empty() {
        commands.push(
            selection
                .command(conda)
                .args(["python", "-m", "pip", "install", "--no-deps", "--no-input"])
                .arg("--disable-pip-version-check")
                .args(replay.pip.iter().cloned())
                .cwd(&managed.root),
        );
    }
    commands
}

#[derive(Deserialize)]
struct EnvironmentList {
    envs: Vec<PathBuf>,
}
