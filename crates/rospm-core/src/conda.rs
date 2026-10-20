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

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{self, Stdio};
use std::time::Duration;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::lock::{sha256_hex, Lock, LockedConda};
use crate::manifest::{CondaBuild, ManagedConda};
use crate::message::Message;
use crate::msg;
use crate::pixi::conda_platform;
use crate::plan::{shell_quote, Command};
use crate::platform::Platform;
use crate::probe::{pip_requirement_name, CondaHost};

use bootstrap::{compatible, miniforge, Bootstrap};

const MANAGED_PREFIX: &str = ".rospm/conda/env";
const DEFINITION: &str = ".rospm/conda/environment.yml";
const READY: &str = ".rospm/conda/ready.sha256";
const DEFAULT_LOCK: &str = "conda-{platform}.lock";

/// The explicit lock of `platform`, relative to the manifest directory.
pub fn lock_path(managed: &ManagedConda, platform: Platform) -> String {
    managed
        .lock_file
        .as_deref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| DEFAULT_LOCK.into())
        .replace("{platform}", conda_platform(platform))
}

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
        match &self.replay {
            Some(replay) => format!("{}\n{}", self.fingerprint, replay.lock_sha256),
            None => self.fingerprint.clone(),
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
                None => ready.lines().next() == Some(self.fingerprint.as_str()),
            }
    }
}

/// Subprocesses and downloads, replaced by fakes in tests.
pub(crate) trait System {
    /// Run `program args` and return its stdout.
    fn query(&self, program: &str, args: &[&str]) -> Result<String>;
    fn run(&self, command: &Command) -> Result<()>;
    /// Download `url` to `dest`; the caller verifies the contents.
    fn download(&self, url: &str, dest: &Path) -> Result<()>;
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
            // Never adopt or overwrite a directory just because it has the
            // right name. A failed create also remains visibly incomplete.
            if managed.prefix.exists() {
                return Err(Error::Host(msg!(
                    "conda.managed_conflict",
                    prefix = managed.prefix.display()
                )));
            }
            // Invalidate any stale completion stamp before starting creation.
            if managed.ready.exists() {
                fs::remove_file(&managed.ready).map_err(|e| Error::io(&managed.ready, e))?;
            }
            let parent = managed
                .definition
                .parent()
                .expect("project definition has a parent");
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
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

/// Everything `apply --locked` needs, checked while planning so a missing,
/// stale or incomplete record fails before anything is written. Never falls
/// back to solving the definition.
fn replay(
    managed: &ManagedConda,
    definition: &Definition,
    fingerprint: &str,
    lock: &Lock,
    root: &Path,
    platform: Platform,
) -> Result<Replay> {
    let Some(record) = lock.conda_for(platform) else {
        return Err(Error::Host(msg!(
            "conda.lock_record_missing",
            platform = platform,
            path = root.join(lock_path(managed, platform)).display()
        )));
    };
    if record.definition_sha256 != fingerprint {
        return Err(Error::Host(msg!(
            "conda.lock_definition_stale",
            platform = platform
        )));
    }
    let path = root.join(&record.lock);
    let text = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
    let actual = sha256_hex(&text);
    if actual != record.lock_sha256 {
        return Err(Error::Host(msg!(
            "conda.lock_digest_mismatch",
            path = path.display(),
            expected = record.lock_sha256,
            actual = actual
        )));
    }
    let invalid = |error: Message| {
        Error::Host(msg!(
            "conda.lock_invalid",
            path = path.display(),
            error = error
        ))
    };
    let packages = parse_explicit(&text, platform).map_err(invalid)?;
    // The record matched the definition when it was written; these checks
    // catch a lock edited or generated elsewhere.
    let mut missing: Vec<String> = definition
        .conda
        .iter()
        .map(|spec| spec_name(spec).to_string())
        .filter(|name| !packages.contains_key(name))
        .collect();
    let python = packages.get("python");
    if !python
        .is_some_and(|v| v == &managed.python || v.starts_with(&format!("{}.", managed.python)))
    {
        missing.push(format!("python={}", managed.python));
    }
    if !definition.pip.is_empty() && !packages.contains_key("pip") {
        missing.push("pip".into());
    }
    missing.dedup();
    if !missing.is_empty() {
        return Err(Error::Host(msg!(
            "conda.lock_incomplete",
            path = path.display(),
            packages = missing.join(", ")
        )));
    }
    let pip = exact_pins(&definition.pip)
        .map_err(|loose| Error::Host(msg!("conda.pip_unpinned", packages = loose.join(", "))))?;
    Ok(Replay {
        lock: path,
        lock_sha256: record.lock_sha256.clone(),
        pip: pip
            .into_iter()
            .map(|(name, version)| format!("{name}=={version}"))
            .collect(),
    })
}

/// Package name → version of a `conda list --explicit` file for `platform`.
/// Every package needs a digest so that Conda verifies what it downloads.
fn parse_explicit(
    text: &str,
    platform: Platform,
) -> std::result::Result<IndexMap<String, String>, Message> {
    let expected = conda_platform(platform);
    let mut declared = None;
    let mut explicit = false;
    let mut packages = IndexMap::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(comment) = line.strip_prefix('#') {
            if let Some(found) = comment.trim().strip_prefix("platform:") {
                declared = Some(found.trim().to_string());
            }
            continue;
        }
        if line == "@EXPLICIT" {
            explicit = true;
            continue;
        }
        if !explicit {
            return Err(msg!("conda.explicit_marker_missing"));
        }
        let (url, _digest) = line
            .rsplit_once('#')
            .filter(|(_, d)| matches!(d.len(), 32 | 64) && d.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| msg!("conda.explicit_digest_missing", line = line))?;
        let file = url.rsplit('/').next().unwrap_or_default();
        let mut parts = file
            .strip_suffix(".conda")
            .or_else(|| file.strip_suffix(".tar.bz2"))
            .unwrap_or_default()
            .rsplitn(3, '-');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(_build), Some(version), Some(name)) if !name.is_empty() => {
                packages.insert(name.to_string(), version.to_string());
            }
            _ => return Err(msg!("conda.explicit_not_package", line = line)),
        }
    }
    match declared {
        Some(found) if found == expected => {}
        found => {
            return Err(msg!(
                "conda.explicit_platform",
                found = found.unwrap_or_else(|| "-".into()),
                expected = expected
            ))
        }
    }
    if packages.is_empty() {
        return Err(msg!("conda.explicit_empty"));
    }
    Ok(packages)
}

/// The package name of a Conda match spec, without channel.
fn spec_name(spec: &str) -> &str {
    let package = spec
        .trim()
        .split(['=', '<', '>', '!', '~', ' ', '['])
        .next()
        .unwrap_or("");
    package.rsplit("::").next().unwrap_or(package)
}

/// `(normalized name, version)` for each `name==version`; otherwise every
/// requirement that is not such a pin. Pins are installed without
/// dependencies, so loose ranges, URLs and markers cannot be replayed.
fn exact_pins(pip: &[String]) -> std::result::Result<Vec<(String, String)>, Vec<String>> {
    let mut pins = Vec::new();
    let mut loose = Vec::new();
    for requirement in pip {
        let pinned = requirement
            .trim()
            .split_once("==")
            .filter(|(name, version)| {
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                    && !version.is_empty()
                    && version
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._+!-".contains(&b))
            });
        match pinned {
            Some((name, version)) => pins.push((pip_requirement_name(name), version.to_string())),
            None => loose.push(requirement.clone()),
        }
    }
    if loose.is_empty() {
        Ok(pins)
    } else {
        Err(loose)
    }
}

/// `rospm lock` for the Conda environment: write the explicit lock of a
/// ready managed environment and return its record. Whatever cannot be
/// recorded is a note; `apply --locked` then fails on this platform.
pub fn resolve_lock(
    config: &CondaBuild,
    root: &Path,
    host: &CondaHost,
    platform: Platform,
    notes: &mut Vec<Message>,
) -> Result<Option<LockedConda>> {
    resolve_lock_with(config, root, host, platform, notes, &Real)
}

fn resolve_lock_with(
    config: &CondaBuild,
    root: &Path,
    host: &CondaHost,
    platform: Platform,
    notes: &mut Vec<Message>,
    system: &dyn System,
) -> Result<Option<LockedConda>> {
    let selection = Selection::from_config(config, root)?;
    let Some(managed) = &config.managed else {
        notes.push(msg!("conda.lock_external", selection = selection.label()));
        return Ok(None);
    };
    let definition = environment_yaml(managed, root)?;
    let fingerprint = sha256_hex(&definition.contents);
    let prefix = root.join(MANAGED_PREFIX);
    let skipped = |notes: &mut Vec<Message>, reason: Message| {
        notes.push(msg!("conda.lock_skipped", reason = reason));
        Ok(None)
    };
    let ready = fs::read_to_string(root.join(READY)).unwrap_or_default();
    if !prefix.join("conda-meta/history").is_file()
        || ready.lines().next() != Some(fingerprint.as_str())
    {
        return skipped(
            notes,
            msg!("conda.managed_not_ready", prefix = prefix.display()),
        );
    }
    let pins = match exact_pins(&definition.pip) {
        Ok(pins) => pins,
        Err(loose) => {
            return skipped(
                notes,
                msg!("conda.pip_unpinned", packages = loose.join(", ")),
            )
        }
    };
    let program = match Bootstrap::new(host, platform) {
        Some(b) if b.is_ready() => b.conda().to_string_lossy().into_owned(),
        _ => "conda".to_string(),
    };
    let prefix_arg = prefix.to_string_lossy();
    let listed = system
        .query(&program, &["list", "--json", "--prefix", &prefix_arg])
        .and_then(|text| {
            serde_json::from_str::<Vec<Listed>>(&text)
                .map_err(|e| Error::Host(msg!("conda.query_invalid", error = e)))
        });
    let listed = match listed {
        Ok(listed) => listed,
        Err(error) => return skipped(notes, error.message()),
    };
    // Explicit locks hold Conda packages only: every PyPI package in the
    // environment must be one of the pins, at the pinned version.
    let installed: Vec<(String, String)> = listed
        .into_iter()
        .filter(|p| p.channel == "pypi")
        .map(|p| (pip_requirement_name(&p.name), p.version))
        .collect();
    let mut differences: Vec<String> = installed
        .iter()
        .filter(|p| !pins.contains(p))
        .map(|(name, version)| format!("{name}=={version}"))
        .collect();
    differences.extend(
        pins.iter()
            .filter(|p| !installed.contains(p))
            .map(|(name, version)| format!("{name}=={version} (-)")),
    );
    if !differences.is_empty() {
        return skipped(
            notes,
            msg!("conda.pip_differs", packages = differences.join(", ")),
        );
    }
    let text = match system.query(
        &program,
        &["list", "--explicit", "--sha256", "--prefix", &prefix_arg],
    ) {
        Ok(text) => text,
        Err(error) => return skipped(notes, error.message()),
    };
    let packages = match parse_explicit(&text, platform) {
        Ok(packages) => packages,
        Err(error) => return skipped(notes, error),
    };
    let relative = lock_path(managed, platform);
    let path = root.join(&relative);
    if fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        fs::write(&path, &text).map_err(|e| Error::io(&path, e))?;
    }
    // The environment is what the lock describes, so `apply --locked` on
    // this host reuses it instead of refusing to touch the prefix.
    let lock_sha256 = sha256_hex(&text);
    let stamp = format!("{fingerprint}\n{lock_sha256}");
    if ready != stamp {
        let ready = root.join(READY);
        fs::write(&ready, stamp).map_err(|e| Error::io(&ready, e))?;
    }
    notes.push(msg!(
        "conda.lock_recorded",
        path = path.display(),
        count = packages.len(),
        pip = pins.len()
    ));
    Ok(Some(LockedConda {
        platform,
        lock: relative,
        lock_sha256,
        definition_sha256: fingerprint,
    }))
}

#[derive(Deserialize)]
struct Listed {
    name: String,
    version: String,
    #[serde(default)]
    channel: String,
}

#[derive(Deserialize)]
struct EnvironmentList {
    envs: Vec<PathBuf>,
}

struct Real;

impl System for Real {
    fn query(&self, program: &str, args: &[&str]) -> Result<String> {
        let output = process::Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| Error::Host(msg!("conda.tool_missing", error = e)))?;
        if !output.status.success() {
            return Err(Error::Host(msg!(
                "conda.query_failed",
                status = output.status,
                error = String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn run(&self, command: &Command) -> Result<()> {
        let status = process::Command::new(&command.program)
            .args(&command.args)
            .current_dir(command.cwd.as_ref().expect("Conda command has a cwd"))
            .stdin(Stdio::null())
            .status()
            .map_err(|e| Error::io(&command.program, e))?;
        if !status.success() {
            return Err(Error::CommandFailed {
                command: command.render(),
                status: status.to_string(),
            });
        }
        Ok(())
    }

    /// Streams to `dest` with bounded connect / response waits and retries
    /// transient failures; HTTP errors other than 5xx fail at once.
    fn download(&self, url: &str, dest: &Path) -> Result<()> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .timeout_global(Some(Duration::from_secs(30 * 60)))
            .max_redirects(5)
            .user_agent(concat!("rospm/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        let mut last = String::new();
        for attempt in 1..=3 {
            if attempt > 1 {
                std::thread::sleep(Duration::from_secs(2 * attempt));
            }
            // (message, worth retrying)
            let result = agent
                .get(url)
                .call()
                .map_err(|e| {
                    let retry = !matches!(e, ureq::Error::StatusCode(s) if s < 500);
                    (e.to_string(), retry)
                })
                .and_then(|mut resp| {
                    let mut file = fs::File::create(dest).map_err(|e| (e.to_string(), false))?;
                    std::io::copy(&mut resp.body_mut().as_reader(), &mut file)
                        .map_err(|e| (e.to_string(), true))?;
                    file.sync_all().map_err(|e| (e.to_string(), false))
                });
            match result {
                Ok(()) => return Ok(()),
                Err((error, retry)) => {
                    last = error;
                    if !retry {
                        break;
                    }
                }
            }
        }
        Err(Error::Host(msg!(
            "conda.download_failed",
            url = url,
            error = last,
            path = dest.with_extension("").display()
        )))
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentFile {
    // Input names and prefixes never decide where a managed environment lives.
    #[serde(default, rename = "name")]
    _name: Option<String>,
    #[serde(default, rename = "prefix")]
    _prefix: Option<PathBuf>,
    #[serde(default)]
    channels: Vec<String>,
    #[serde(default)]
    dependencies: Vec<serde_yaml::Value>,
    #[serde(default)]
    variables: IndexMap<String, String>,
}

#[derive(Serialize)]
struct GeneratedEnvironment {
    channels: Vec<String>,
    dependencies: Vec<serde_yaml::Value>,
    #[serde(skip_serializing_if = "IndexMap::is_empty")]
    variables: IndexMap<String, String>,
}

/// Digest of the normalized definition, as recorded in a lock record.
pub fn definition_sha256(managed: &ManagedConda, root: &Path) -> Result<String> {
    Ok(sha256_hex(&environment_yaml(managed, root)?.contents))
}

/// The normalized definition and what lock replay needs to know about it.
struct Definition {
    contents: String,
    /// Conda match specs; `python` included.
    conda: Vec<String>,
    /// Requirements of the `pip:` entry.
    pip: Vec<String>,
    variables: IndexMap<String, String>,
}

fn environment_yaml(managed: &ManagedConda, root: &Path) -> Result<Definition> {
    let mut input = if let Some(file) = &managed.environment_file {
        let path = root.join(file);
        let text = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        serde_yaml::from_str::<EnvironmentFile>(&text).map_err(|e| {
            Error::Host(msg!(
                "conda.definition_invalid",
                path = path.display(),
                error = e
            ))
        })?
    } else {
        EnvironmentFile::default()
    };
    let channels: Vec<_> = managed
        .channels
        .iter()
        .filter(|c| *c != "nodefaults")
        .cloned()
        .collect();
    let file_channels: Vec<_> = input
        .channels
        .iter()
        .filter(|c| *c != "nodefaults")
        .cloned()
        .collect();
    if !file_channels.is_empty() && file_channels != channels {
        return Err(Error::Host(msg!("conda.channels_conflict")));
    }
    let python = format!("python={}", managed.python);
    for dependency in &mut input.dependencies {
        if let Some(spec) = dependency.as_str() {
            if spec_name(spec) == "python" {
                if spec != "python" && spec != python {
                    return Err(Error::Host(msg!(
                        "conda.python_conflict",
                        expected = python,
                        found = spec
                    )));
                }
                *dependency = serde_yaml::Value::String(python.clone());
            }
        }
    }
    if !input
        .dependencies
        .iter()
        .any(|d| d.as_str() == Some(&python))
    {
        input
            .dependencies
            .insert(0, serde_yaml::Value::String(python));
    }
    let mut channels = channels;
    channels.push("nodefaults".into());
    let conda = input
        .dependencies
        .iter()
        .filter_map(|d| d.as_str().map(str::to_string))
        .collect();
    let pip = input
        .dependencies
        .iter()
        .filter_map(|d| d.get("pip")?.as_sequence())
        .flatten()
        .filter_map(|r| r.as_str().map(str::to_string))
        .collect();
    let contents = serde_yaml::to_string(&GeneratedEnvironment {
        channels,
        dependencies: input.dependencies,
        variables: input.variables.clone(),
    })
    .map_err(|e| {
        Error::Host(msg!(
            "conda.definition_invalid",
            path = DEFINITION,
            error = e
        ))
    })?;
    Ok(Definition {
        contents,
        conda,
        pip,
        variables: input.variables,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Project(PathBuf);

    impl Project {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "rospm-conda-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }

        fn managed(&self) -> CondaBuild {
            CondaBuild {
                managed: Some(ManagedConda {
                    python: "3.12".into(),
                    channels: vec!["conda-forge".into()],
                    environment_file: None,
                    lock_file: None,
                }),
                ..Default::default()
            }
        }

        /// rospm's per-user directories, inside the temporary project.
        fn host(&self) -> CondaHost {
            CondaHost {
                on_path: None,
                data_dir: self.0.join("user/data"),
                cache_dir: self.0.join("user/cache"),
            }
        }

        fn step(&self, config: &CondaBuild, prepare: bool) -> CondaStep {
            let mut step = CondaStep::new(
                config,
                &self.0,
                &self.host(),
                Platform::LinuxX86_64,
                prepare,
                None,
            )
            .unwrap();
            // Fake installers cannot match the bundled digest of the real one.
            // Repeat the planning-time readiness check with the fake digest.
            if let Some(bootstrap) = &mut step.bootstrap {
                bootstrap.sha256 = Box::leak(digest(INSTALLER).into_boxed_str());
                if bootstrap.is_ready() {
                    step.program = bootstrap.conda().to_string_lossy().into_owned();
                }
            }
            step
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const INSTALLER: &[u8] = b"#!/bin/sh\n# fake Miniforge\n";

    fn digest(bytes: &[u8]) -> String {
        use sha2::Digest;
        format!("{:x}", sha2::Sha256::digest(bytes))
    }

    /// Records calls and imitates conda, the installer and the network.
    struct Fake {
        /// `conda --version` output of the conda on PATH; `None`: not found.
        path_conda: Option<&'static str>,
        envs: Vec<PathBuf>,
        /// Served by `download`; `None` imitates being offline.
        installer: Option<Vec<u8>>,
        fail_install: Cell<bool>,
        fail_create: bool,
        downloads: Cell<u32>,
        installs: Cell<u32>,
        /// Program of each `env create`.
        creates: RefCell<Vec<String>>,
        /// `conda list --json` and `--explicit` output for `lock`.
        listed: String,
        explicit: String,
        /// Arguments of every replay command.
        replays: RefCell<Vec<Vec<String>>>,
    }

    impl Default for Fake {
        fn default() -> Self {
            Self {
                path_conda: Some("conda 24.11.3"),
                envs: Vec::new(),
                installer: Some(INSTALLER.to_vec()),
                fail_install: Cell::new(false),
                fail_create: false,
                downloads: Cell::new(0),
                installs: Cell::new(0),
                creates: RefCell::new(Vec::new()),
                listed: "[]".into(),
                explicit: String::new(),
                replays: RefCell::new(Vec::new()),
            }
        }
    }

    impl System for Fake {
        fn query(&self, program: &str, args: &[&str]) -> Result<String> {
            if let ["list", kind, .., prefix] = args {
                assert!(Path::new(prefix).join("conda-meta/history").is_file());
                return Ok(match *kind {
                    "--json" => self.listed.clone(),
                    _ => self.explicit.clone(),
                });
            }
            if program != "conda" {
                assert!(Path::new(program).is_file(), "{program} was not installed");
                assert_eq!(args, ["--version"]);
                return Ok("conda 26.7.2\n".into());
            }
            let Some(version) = self.path_conda else {
                return Err(Error::Host(msg!("conda.tool_missing", error = "not found")));
            };
            Ok(match args {
                ["--version"] => version.to_string(),
                ["env", "list", "--json"] => serde_json::json!({ "envs": self.envs }).to_string(),
                other => panic!("unexpected conda query {other:?}"),
            })
        }

        fn run(&self, command: &Command) -> Result<()> {
            let failed = || {
                Err(Error::CommandFailed {
                    command: command.render(),
                    status: "19".into(),
                })
            };
            if command.program == "bash" {
                self.installs.set(self.installs.get() + 1);
                assert_eq!(command.args[..2], ["--noprofile", "--norc"]);
                assert_eq!(fs::read(&command.args[2]).unwrap(), INSTALLER);
                assert_eq!(command.args[3..5], ["-b", "-p"]);
                let root = Path::new(&command.args[5]);
                assert!(!root.exists(), "the installer requires a fresh prefix");
                fs::create_dir_all(root.join("bin")).unwrap();
                if self.fail_install.replace(false) {
                    return failed();
                }
                fs::write(root.join("bin/conda"), "conda").unwrap();
                return Ok(());
            }
            match command.args[0].as_str() {
                "create" => {
                    assert_eq!(command.args[..3], ["create", "--yes", "--prefix"]);
                    assert_eq!(command.args[4], "--file");
                    let prefix = Path::new(&command.args[3]);
                    fs::create_dir_all(prefix.join("conda-meta")).unwrap();
                    fs::write(prefix.join("conda-meta/history"), "replayed").unwrap();
                }
                "env" if command.args[1] == "config" => {}
                "run" => assert!(command.args.contains(&"--no-deps".to_string())),
                _ => {}
            }
            if command.args[..2] != ["env", "create"] {
                self.replays.borrow_mut().push(command.args.clone());
                return Ok(());
            }
            assert_eq!(command.args[..4], ["env", "create", "--yes", "--prefix"]);
            self.creates.borrow_mut().push(command.program.clone());
            let prefix = Path::new(&command.args[4]);
            fs::create_dir_all(prefix.join("conda-meta")).unwrap();
            fs::write(prefix.join("conda-meta/history"), "created").unwrap();
            if self.fail_create {
                return failed();
            }
            Ok(())
        }

        fn download(&self, url: &str, dest: &Path) -> Result<()> {
            assert!(url.starts_with("https://github.com/conda-forge/miniforge/"));
            self.downloads.set(self.downloads.get() + 1);
            match &self.installer {
                Some(bytes) => {
                    fs::write(dest, bytes).unwrap();
                    Ok(())
                }
                None => Err(Error::Host(msg!(
                    "conda.download_failed",
                    url = url,
                    error = "offline",
                    path = dest.display()
                ))),
            }
        }
    }

    #[test]
    fn managed_creation_reuses_only_a_completed_matching_definition() {
        let project = Project::new();
        let config = project.managed();
        let step = project.step(&config, true);
        assert!(step.render().contains("python=3.12"));
        assert!(!project.0.join(".rospm").exists());
        let fake = Fake::default();
        step.perform_with(&mut Vec::new(), &fake).unwrap();
        assert!(project.0.join(READY).is_file());
        step.perform_with(&mut Vec::new(), &fake).unwrap();
        // A compatible conda on PATH is used as is; nothing is installed.
        assert_eq!(*fake.creates.borrow(), ["conda"]);
        assert_eq!(fake.downloads.get() + fake.installs.get(), 0);
        assert!(!project.0.join("user").exists());
        project
            .step(&config, false)
            .perform_with(&mut Vec::new(), &fake)
            .unwrap();
        let mut changed = config;
        changed.managed.as_mut().unwrap().python = "3.11".into();
        let error = project
            .step(&changed, true)
            .perform_with(&mut Vec::new(), &fake)
            .unwrap_err();
        assert!(error.to_string().contains("conda.managed_conflict"));
        assert_eq!(fake.creates.borrow().len(), 1);
    }

    #[test]
    fn failed_create_and_unprepared_build_never_mark_ready_or_adopt_directories() {
        let project = Project::new();
        let config = project.managed();
        let fake = Fake {
            fail_create: true,
            ..Fake::default()
        };
        assert!(project
            .step(&config, false)
            .perform_with(&mut Vec::new(), &fake)
            .is_err());
        assert!(!project.0.join(".rospm").exists());
        let step = project.step(&config, true);
        let result = step.perform_with(&mut Vec::new(), &fake);
        assert!(matches!(result, Err(Error::CommandFailed { .. })));
        assert!(!project.0.join(READY).exists());
        // The partial environment is neither adopted nor recreated.
        assert!(step.perform_with(&mut Vec::new(), &fake).is_err());
        assert_eq!(fake.creates.borrow().len(), 1);
        assert!(!project.0.join(READY).exists());
    }

    #[test]
    fn external_environments_are_checked_without_writes_and_missing_ones_fail() {
        let project = Project::new();
        let prefix = project.0.join("envs/robot");
        fs::create_dir_all(prefix.join("conda-meta")).unwrap();
        fs::write(prefix.join("conda-meta/history"), "external").unwrap();
        let fake = Fake {
            envs: vec![prefix.clone()],
            ..Fake::default()
        };
        for config in [
            CondaBuild {
                name: Some("robot".into()),
                ..Default::default()
            },
            CondaBuild {
                prefix: Some(PathBuf::from("envs/robot")),
                ..Default::default()
            },
        ] {
            let step = project.step(&config, true);
            assert!(step.bootstrap.is_none());
            step.perform_with(&mut Vec::new(), &fake).unwrap();
            fs::remove_file(prefix.join("conda-meta/history")).unwrap();
            assert!(step
                .perform_with(&mut Vec::new(), &fake)
                .unwrap_err()
                .to_string()
                .contains("conda.existing_missing"));
            fs::write(prefix.join("conda-meta/history"), "external").unwrap();
        }
        // External environments never trigger a Conda installation.
        let missing = Fake {
            path_conda: None,
            ..Fake::default()
        };
        let named = CondaBuild {
            name: Some("robot".into()),
            ..Default::default()
        };
        assert!(project
            .step(&named, true)
            .perform_with(&mut Vec::new(), &missing)
            .unwrap_err()
            .to_string()
            .contains("conda.tool_missing"));
        assert!(fake.creates.borrow().is_empty() && missing.downloads.get() == 0);
        assert_eq!(
            fs::read_to_string(prefix.join("conda-meta/history")).unwrap(),
            "external"
        );
        assert!(!project.0.join(".rospm").exists() && !project.0.join("user").exists());
    }

    #[test]
    fn definition_paths_are_project_relative_and_cannot_select_an_external_prefix() {
        let project = Project::new();
        let other_project = Project::new();
        let mut config = project.managed();
        fs::create_dir_all(project.0.join("config")).unwrap();
        let file = project.0.join("config/environment.yml");
        fs::write(&file, "name: shared-name\nprefix: /external/must-not-touch\nchannels: [conda-forge]\ndependencies: [python, numpy]\n").unwrap();
        config.managed.as_mut().unwrap().environment_file =
            Some(PathBuf::from("config/environment.yml"));
        let step = project.step(&config, true);
        let rendered = step.render();
        assert!(rendered.contains(file.to_str().unwrap()));
        assert!(!rendered.contains("/external/must-not-touch"));
        assert!(!rendered.contains("shared-name"));
        assert!(rendered.contains("numpy") && rendered.contains("python=3.12"));
        assert!(rendered.contains("nodefaults"));
        let other = other_project.step(&other_project.managed(), true);
        assert_ne!(
            step.managed.as_ref().unwrap().prefix,
            other.managed.as_ref().unwrap().prefix
        );
        for definition in [
            "channels: [defaults]\n",
            "dependencies: ['python=3.11']\n",
            "not: [a supported field]\n",
        ] {
            fs::write(&file, definition).unwrap();
            assert!(CondaStep::new(
                &config,
                &project.0,
                &project.host(),
                Platform::LinuxX86_64,
                true,
                None
            )
            .is_err());
        }
        assert!(!project.0.join(".rospm").exists());
    }

    /// A definition with Conda packages, a PyPI pin and a variable.
    fn pinned(project: &Project, pip: &str) -> CondaBuild {
        let mut config = project.managed();
        fs::write(
            project.0.join("environment.yml"),
            format!(
                "dependencies:\n  - numpy\n  - pip\n  - pip: ['{pip}']\nvariables:\n  ROBOT: one\n"
            ),
        )
        .unwrap();
        config.managed.as_mut().unwrap().environment_file = Some(PathBuf::from("environment.yml"));
        config
    }

    fn explicit(platform: &str, packages: &[&str]) -> String {
        let mut text = format!("# This file may be used to create an environment using:\n# platform: {platform}\n@EXPLICIT\n");
        for package in packages {
            text += &format!(
                "https://conda.anaconda.org/conda-forge/{platform}/{package}.conda#{}\n",
                digest(package.as_bytes())
            );
        }
        text
    }

    const PACKAGES: [&str; 3] = [
        "python-3.12.7-hc5c86c4_0_cpython",
        "numpy-2.1.2-py312h58c1407_0",
        "pip-24.2-pyh8b19718_1",
    ];

    fn recorder(listed: &str) -> Fake {
        Fake {
            listed: listed.into(),
            explicit: explicit("linux-64", &PACKAGES),
            ..Fake::default()
        }
    }

    const PYPI: &str = r#"[{"name": "numpy", "version": "2.1.2", "channel": "conda-forge"},
        {"name": "Requests", "version": "2.32.3", "channel": "pypi"}]"#;

    fn resolve(
        project: &Project,
        config: &CondaBuild,
        fake: &Fake,
    ) -> (Option<LockedConda>, Vec<Message>) {
        let mut notes = Vec::new();
        let record = resolve_lock_with(
            config,
            &project.0,
            &project.host(),
            Platform::LinuxX86_64,
            &mut notes,
            fake,
        )
        .unwrap();
        (record, notes)
    }

    fn lock_with(records: Vec<LockedConda>) -> Lock {
        Lock {
            schema_version: crate::lock::SUPPORTED_SCHEMA_VERSION,
            generated_at: "2026-10-20T00:00:00Z".into(),
            manifest_sha256: sha256_hex("m"),
            provider: crate::lock::LockedProvider {
                kind: crate::manifest::ProviderKind::Native,
                native: None,
                pixi: None,
                docker: None,
            },
            sources: Vec::new(),
            apt: Vec::new(),
            pip: Vec::new(),
            conda: records,
        }
    }

    fn locked_step(project: &Project, config: &CondaBuild, lock: &Lock) -> Result<CondaStep> {
        CondaStep::new(
            config,
            &project.0,
            &project.host(),
            Platform::LinuxX86_64,
            true,
            Some(lock),
        )
    }

    #[test]
    fn lock_records_a_ready_environment_and_apply_locked_replays_it_without_solving() {
        let machine = Project::new();
        let config = pinned(&machine, "requests==2.32.3");
        let fake = recorder(PYPI);
        // Nothing to record before apply prepared the environment.
        let (record, notes) = resolve(&machine, &config, &fake);
        assert!(record.is_none() && notes[0].to_string().contains("conda.lock_skipped("));
        assert!(!machine.0.join("conda-linux-64.lock").exists());

        machine
            .step(&config, true)
            .perform_with(&mut Vec::new(), &fake)
            .unwrap();
        let (record, notes) = resolve(&machine, &config, &fake);
        let record = record.unwrap();
        assert!(
            notes[0].to_string().contains("conda.lock_recorded("),
            "{notes:?}"
        );
        assert_eq!(record.lock, "conda-linux-64.lock");
        let file = machine.0.join(&record.lock);
        assert_eq!(fs::read_to_string(&file).unwrap(), fake.explicit);
        assert_eq!(record.lock_sha256, sha256_hex(&fake.explicit));
        assert_eq!(resolve(&machine, &config, &fake).0.unwrap(), record);
        let lock = lock_with(vec![record.clone()]);
        // The recorded environment satisfies the lock on the same host.
        locked_step(&machine, &config, &lock)
            .unwrap()
            .perform_with(&mut Vec::new(), &fake)
            .unwrap();
        assert!(fake.replays.borrow().is_empty());

        // Another machine: only the project files.
        let clean = Project::new();
        for name in ["environment.yml", "conda-linux-64.lock"] {
            fs::copy(machine.0.join(name), clean.0.join(name)).unwrap();
        }
        let replayer = Fake::default();
        let step = locked_step(&clean, &config, &lock).unwrap();
        let preview = step.render();
        assert!(preview.contains("conda.replay("), "{preview}");
        assert!(preview.contains(&format!(
            "conda create --yes --prefix {}",
            clean.0.join(MANAGED_PREFIX).display()
        )));
        assert!(preview.contains(
            "pip install --no-deps --no-input --disable-pip-version-check requests==2.32.3"
        ));
        assert!(preview.contains("env config vars set --prefix") && preview.contains("ROBOT=one"));
        assert!(!preview.contains("env create"));
        assert!(!clean.0.join(".rospm").exists());
        step.perform_with(&mut Vec::new(), &replayer).unwrap();
        let replays = replayer.replays.borrow().clone();
        assert_eq!(replays.len(), 3);
        assert_eq!(
            replays[0][5],
            clean.0.join("conda-linux-64.lock").to_str().unwrap()
        );
        assert!(replayer.creates.borrow().is_empty(), "replay never solves");
        assert_eq!(
            fs::read_to_string(clean.0.join(READY)).unwrap(),
            format!("{}\n{}", record.definition_sha256, record.lock_sha256)
        );
        // Replayed: reused by locked apply, plain apply and build alike.
        locked_step(&clean, &config, &lock)
            .unwrap()
            .perform_with(&mut Vec::new(), &replayer)
            .unwrap();
        clean
            .step(&config, true)
            .perform_with(&mut Vec::new(), &replayer)
            .unwrap();
        clean
            .step(&config, false)
            .perform_with(&mut Vec::new(), &replayer)
            .unwrap();
        assert_eq!(replayer.replays.borrow().len(), 3);
        assert!(replayer.creates.borrow().is_empty());

        // A solved environment never counts as the replay of a lock.
        let solved = Project::new();
        let config = solved.managed();
        solved
            .step(&config, true)
            .perform_with(&mut Vec::new(), &fake)
            .unwrap();
        let mut other = record;
        other.definition_sha256 = sha256_hex(
            &environment_yaml(config.managed.as_ref().unwrap(), &solved.0)
                .unwrap()
                .contents,
        );
        other.lock = "conda-linux-64.lock".into();
        let text = explicit("linux-64", &PACKAGES[..1]);
        fs::write(solved.0.join(&other.lock), &text).unwrap();
        other.lock_sha256 = sha256_hex(&text);
        let error = locked_step(&solved, &config, &lock_with(vec![other]))
            .unwrap()
            .perform_with(&mut Vec::new(), &fake)
            .unwrap_err();
        assert!(
            error.to_string().contains("conda.managed_conflict"),
            "{error}"
        );
    }

    #[test]
    fn apply_locked_refuses_missing_stale_or_incomplete_records_before_any_write() {
        let project = Project::new();
        let config = pinned(&project, "requests==2.32.3");
        let definition = environment_yaml(config.managed.as_ref().unwrap(), &project.0).unwrap();
        let record = |text: &str| {
            fs::write(project.0.join("conda-linux-64.lock"), text).unwrap();
            LockedConda {
                platform: Platform::LinuxX86_64,
                lock: "conda-linux-64.lock".into(),
                lock_sha256: sha256_hex(text),
                definition_sha256: sha256_hex(&definition.contents),
            }
        };
        let refused = |lock: Lock, config: &CondaBuild| {
            locked_step(&project, config, &lock)
                .unwrap_err()
                .to_string()
        };
        let good = explicit("linux-64", &PACKAGES);

        let mut arm = record(&explicit("linux-aarch64", &PACKAGES));
        arm.platform = Platform::LinuxAarch64;
        let error = refused(lock_with(vec![arm]), &config);
        assert!(
            error.contains("conda.lock_record_missing(") && error.contains("linux-x86_64"),
            "{error}"
        );

        let mut stale = record(&good);
        stale.definition_sha256 = sha256_hex("older definition");
        assert!(refused(lock_with(vec![stale]), &config).contains("conda.lock_definition_stale("));

        let tampered = record(&good);
        fs::write(
            project.0.join("conda-linux-64.lock"),
            good.replace("numpy-2.1.2", "numpy-2.1.3"),
        )
        .unwrap();
        assert!(refused(lock_with(vec![tampered]), &config).contains("conda.lock_digest_mismatch("));

        for (text, expected) in [
            (
                explicit("linux-aarch64", &PACKAGES),
                "conda.explicit_platform(",
            ),
            (
                good.replace("@EXPLICIT\n", ""),
                "conda.explicit_marker_missing",
            ),
            (
                good.replacen(&format!("#{}", digest(PACKAGES[0].as_bytes())), "", 1),
                "conda.explicit_digest_missing(",
            ),
            (
                explicit("linux-64", &PACKAGES[..1]),
                "conda.lock_incomplete(",
            ),
            (
                explicit("linux-64", &[PACKAGES[1], PACKAGES[2], "python-3.11.9-h_0"]),
                "python=3.12",
            ),
        ] {
            let error = refused(lock_with(vec![record(&text)]), &config);
            assert!(error.contains(expected), "{expected}: {error}");
        }

        let loose = pinned(&project, "requests>=2");
        let definition = environment_yaml(loose.managed.as_ref().unwrap(), &project.0).unwrap();
        let mut entry = record(&good);
        entry.definition_sha256 = sha256_hex(&definition.contents);
        let error = refused(lock_with(vec![entry]), &loose);
        assert!(
            error.contains("conda.pip_unpinned(") && error.contains("requests>=2"),
            "{error}"
        );
        assert!(!project.0.join(".rospm").exists());
    }

    #[test]
    fn lock_does_not_record_loose_drifted_or_external_environments() {
        let project = Project::new();
        let loose = pinned(&project, "requests");
        let fake = recorder(PYPI);
        project
            .step(&loose, true)
            .perform_with(&mut Vec::new(), &fake)
            .unwrap();
        let (record, notes) = resolve(&project, &loose, &fake);
        assert!(record.is_none());
        assert!(
            notes[0].to_string().contains("conda.pip_unpinned("),
            "{notes:?}"
        );

        // PyPI packages outside the pins, or at other versions, are drift.
        let project = Project::new();
        let config = pinned(&project, "requests==2.32.3");
        project
            .step(&config, true)
            .perform_with(&mut Vec::new(), &fake)
            .unwrap();
        for listed in [
            r#"[{"name": "requests", "version": "2.32.3", "channel": "pypi"}, {"name": "idna", "version": "3.10", "channel": "pypi"}]"#,
            r#"[{"name": "requests", "version": "2.31.0", "channel": "pypi"}]"#,
        ] {
            let (record, notes) = resolve(&project, &config, &recorder(listed));
            assert!(record.is_none());
            assert!(
                notes[0].to_string().contains("conda.pip_differs("),
                "{notes:?}"
            );
        }
        let (record, notes) = resolve(
            &project,
            &config,
            &Fake {
                explicit: explicit("linux-aarch64", &PACKAGES),
                listed: PYPI.into(),
                ..Fake::default()
            },
        );
        assert!(record.is_none() && notes[0].to_string().contains("conda.explicit_platform("));
        assert!(!project.0.join("conda-linux-64.lock").exists());

        let external = CondaBuild {
            name: Some("robot".into()),
            ..Default::default()
        };
        let (record, notes) = resolve(&project, &external, &fake);
        assert!(record.is_none() && notes[0].to_string().contains("conda.lock_external("));
    }

    #[test]
    fn lock_paths_expand_the_conda_platform() {
        let project = Project::new();
        let mut managed = project.managed().managed.unwrap();
        assert_eq!(
            lock_path(&managed, Platform::LinuxAarch64),
            "conda-linux-aarch64.lock"
        );
        managed.lock_file = Some(PathBuf::from("locks/{platform}/env.lock"));
        assert_eq!(
            lock_path(&managed, Platform::LinuxX86_64),
            "locks/linux-64/env.lock"
        );
        assert_eq!(
            exact_pins(&["Foo_Bar==1.0.post1".into()]).unwrap(),
            [("foo-bar".to_string(), "1.0.post1".to_string())]
        );
        for loose in [
            "foo",
            "foo>=1",
            "foo==1.*",
            "foo==1; python_version>'3'",
            "git+https://x/y",
            "foo[extra]==1",
        ] {
            assert!(exact_pins(&[loose.into()]).is_err(), "{loose}");
        }
    }

    #[test]
    fn missing_conda_is_bootstrapped_once_and_then_used_by_apply_and_build() {
        let project = Project::new();
        let config = project.managed();
        let step = project.step(&config, true);
        let preview = step.render();
        let bootstrap = step.bootstrap.clone().unwrap();
        for part in [
            "conda.bootstrap(",
            "version=26.7.2-0",
            "Miniforge3-26.7.2-0-Linux-x86_64.sh",
            bootstrap.root.to_str().unwrap(),
            bootstrap.cache.to_str().unwrap(),
        ] {
            assert!(preview.contains(part), "{part}: {preview}");
        }
        assert!(!project.0.join("user").exists());

        let fake = Fake {
            path_conda: None,
            ..Fake::default()
        };
        let mut out = Vec::new();
        step.perform_with(&mut out, &fake).unwrap();
        let conda = bootstrap.conda().to_string_lossy().into_owned();
        assert_eq!(*fake.creates.borrow(), std::slice::from_ref(&conda));
        assert_eq!((fake.downloads.get(), fake.installs.get()), (1, 1));
        assert!(bootstrap.is_ready() && project.0.join(READY).is_file());
        assert!(String::from_utf8(out).unwrap().contains("conda.installed("));

        // Later plans pick the installed tool without any subprocess, and
        // its `conda run` drives the build.
        let again = project.step(&config, true);
        assert!(again.render().starts_with("conda.tool_ready("));
        again.perform_with(&mut Vec::new(), &fake).unwrap();
        let build = project.step(&config, false);
        build.perform_with(&mut Vec::new(), &fake).unwrap();
        assert_eq!(build.command().program, conda);
        assert_eq!((fake.downloads.get(), fake.installs.get()), (1, 1));
        assert_eq!(fake.creates.borrow().len(), 1);
    }

    #[test]
    fn incompatible_conda_bootstraps_for_apply_but_build_only_reports() {
        let project = Project::new();
        let config = project.managed();
        let old = Fake {
            path_conda: Some("conda 4.12.0"),
            ..Fake::default()
        };
        let error = project
            .step(&config, false)
            .perform_with(&mut Vec::new(), &old)
            .unwrap_err()
            .to_string();
        assert!(error.contains("conda.tool_incompatible(") && error.contains("conda.run_apply"));
        assert!(!project.0.join("user").exists() && !project.0.join(".rospm").exists());
        project
            .step(&config, true)
            .perform_with(&mut Vec::new(), &old)
            .unwrap();
        assert_eq!(old.installs.get(), 1);
        assert!(old.creates.borrow()[0].ends_with("miniforge3-26.7.2-0/bin/conda"));
    }

    #[test]
    fn digest_mismatch_is_refused_and_bad_cache_is_replaced() {
        let project = Project::new();
        let config = project.managed();
        let step = project.step(&config, true);
        let bootstrap = step.bootstrap.clone().unwrap();
        let tampered = Fake {
            path_conda: None,
            installer: Some(b"#!/bin/sh\nrm -rf ~\n".to_vec()),
            ..Fake::default()
        };
        let error = step
            .perform_with(&mut Vec::new(), &tampered)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("conda.installer_digest_mismatch("),
            "{error}"
        );
        assert!(error.contains(&digest(INSTALLER)));
        assert_eq!(tampered.installs.get(), 0);
        let downloads = bootstrap.cache.parent().unwrap();
        assert_eq!(fs::read_dir(downloads).unwrap().count(), 0);
        assert!(!bootstrap.root.exists() && !project.0.join(".rospm").exists());

        // A cached file with another digest is downloaded again, not run.
        fs::write(&bootstrap.cache, "truncated").unwrap();
        let fake = Fake {
            path_conda: None,
            ..Fake::default()
        };
        step.perform_with(&mut Vec::new(), &fake).unwrap();
        assert_eq!((fake.downloads.get(), fake.installs.get()), (1, 1));
        assert_eq!(fs::read(&bootstrap.cache).unwrap(), INSTALLER);
    }

    #[test]
    fn interrupted_install_is_redone_from_the_verified_cache_even_offline() {
        let project = Project::new();
        let config = project.managed();
        let step = project.step(&config, true);
        let bootstrap = step.bootstrap.clone().unwrap();
        let offline = Fake {
            path_conda: None,
            installer: None,
            ..Fake::default()
        };
        let error = step
            .perform_with(&mut Vec::new(), &offline)
            .unwrap_err()
            .to_string();
        assert!(error.contains("conda.download_failed("), "{error}");
        assert_eq!(offline.installs.get(), 0);

        let flaky = Fake {
            path_conda: None,
            fail_install: Cell::new(true),
            ..Fake::default()
        };
        assert!(matches!(
            step.perform_with(&mut Vec::new(), &flaky),
            Err(Error::CommandFailed { .. })
        ));
        assert!(bootstrap.root.exists() && !bootstrap.is_ready());
        // Not marked ready, so planning still offers the bootstrap.
        assert!(project
            .step(&config, true)
            .render()
            .starts_with("conda.bootstrap("));
        step.perform_with(&mut Vec::new(), &offline).unwrap();
        assert_eq!(
            offline.downloads.get(),
            1,
            "only the first, failed download"
        );
        assert_eq!(offline.installs.get(), 1);
        assert!(bootstrap.is_ready());
        assert!(!bootstrap.root.join("leftover").exists());
    }
}
