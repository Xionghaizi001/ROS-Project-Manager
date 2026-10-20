//! Existing environments are checked only; managed environments live in the
//! project and are prepared by apply, which installs Conda itself when the
//! host has no compatible one. All subprocesses, downloads and writes happen
//! when the plan executes, never while constructing or rendering a preview.

mod bootstrap;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{self, Stdio};
use std::time::Duration;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::lock::sha256_hex;
use crate::manifest::{CondaBuild, ManagedConda};
use crate::msg;
use crate::plan::{shell_quote, Command};
use crate::platform::Platform;
use crate::probe::CondaHost;

use bootstrap::{compatible, miniforge, Bootstrap};

const MANAGED_PREFIX: &str = ".rospm/conda/env";
const DEFINITION: &str = ".rospm/conda/environment.yml";
const READY: &str = ".rospm/conda/ready.sha256";

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
    pub fn new(
        config: &CondaBuild,
        root: &Path,
        host: &CondaHost,
        platform: Platform,
        prepare: bool,
        locked: bool,
    ) -> Result<Self> {
        let selection = Selection::from_config(config, root)?;
        let managed = if let Some(managed) = &config.managed {
            // Do not re-solve an input that claims to be locked. Reference
            // format, digests and platform records are a separate task.
            if prepare && (locked || managed.lock_file.is_some()) {
                return Err(Error::Host(msg!(
                    "conda.lock_replay_pending",
                    path = managed
                        .lock_file
                        .as_ref()
                        .map(|p| root.join(p).display().to_string())
                        .unwrap_or_else(|| "build.conda.managed.lock_file".into())
                )));
            }
            let contents = environment_yaml(managed, root)?;
            Some(Preparation {
                root: root.to_path_buf(),
                prefix: root.join(MANAGED_PREFIX),
                definition: root.join(DEFINITION),
                ready: root.join(READY),
                fingerprint: sha256_hex(&contents),
                contents,
                source: managed.environment_file.as_ref().map(|p| root.join(p)),
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
            let exists = managed.prefix.join("conda-meta/history").is_file();
            let ready = fs::read_to_string(&managed.ready).ok();
            if exists && ready.as_deref() == Some(&managed.fingerprint) {
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
            system.run(&creation_command(managed, &conda))?;
            if !managed.prefix.join("conda-meta/history").is_file() {
                return Err(Error::Host(msg!(
                    "conda.managed_not_ready",
                    prefix = managed.prefix.display()
                )));
            }
            fs::write(&managed.ready, &managed.fingerprint)
                .map_err(|e| Error::io(&managed.ready, e))?;
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

fn environment_yaml(managed: &ManagedConda, root: &Path) -> Result<String> {
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
            let package = spec
                .split(['=', '<', '>', '!', '~', ' ', '['])
                .next()
                .unwrap_or("");
            if package.rsplit("::").next() == Some("python") {
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
    serde_yaml::to_string(&GeneratedEnvironment {
        channels,
        dependencies: input.dependencies,
        variables: input.variables,
    })
    .map_err(|e| {
        Error::Host(msg!(
            "conda.definition_invalid",
            path = DEFINITION,
            error = e
        ))
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
                false,
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
            }
        }
    }

    impl System for Fake {
        fn query(&self, program: &str, args: &[&str]) -> Result<String> {
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
                false
            )
            .is_err());
        }
        assert!(!project.0.join(".rospm").exists());
    }

    #[test]
    fn managed_locks_are_refused_before_writes_or_solving() {
        let project = Project::new();
        let mut config = project.managed();
        let host = project.host();
        let new = |config: &CondaBuild, locked| {
            CondaStep::new(
                config,
                &project.0,
                &host,
                Platform::LinuxX86_64,
                true,
                locked,
            )
        };
        assert!(new(&config, true).is_err());
        config.managed.as_mut().unwrap().lock_file = Some(PathBuf::from("locks/conda.lock"));
        let error = new(&config, false).unwrap_err().to_string();
        assert!(error.contains(project.0.join("locks/conda.lock").to_str().unwrap()));
        assert!(!project.0.join(".rospm").exists());
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
