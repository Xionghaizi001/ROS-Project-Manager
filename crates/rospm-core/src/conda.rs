//! Existing environments are checked only; managed environments live in the
//! project and are prepared by apply. All subprocesses and writes happen when
//! the plan executes, never while constructing or rendering a preview.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::lock::sha256_hex;
use crate::manifest::{CondaBuild, ManagedConda};
use crate::msg;
use crate::plan::{shell_quote, Command};

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

    pub fn command(&self) -> Command {
        let cmd = Command::new("conda").args(["run", "--no-capture-output"]);
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

/// One plan step. `prepare` is false for build: it may only check readiness.
#[derive(Debug, Clone)]
pub struct CondaStep {
    selection: Selection,
    managed: Option<Preparation>,
    prepare: bool,
}

impl CondaStep {
    pub fn new(config: &CondaBuild, root: &Path, prepare: bool, locked: bool) -> Result<Self> {
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
        Ok(Self {
            selection,
            managed,
            prepare,
        })
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
        format!(
            "{}\n{}\n{}\n{}",
            msg!(
                "conda.prepare",
                prefix = managed.prefix.display(),
                source = source
            ),
            msg!("conda.definition", path = managed.definition.display()),
            managed.contents.trim_end(),
            creation_command(managed).render()
        )
    }

    pub fn perform(&self, out: &mut dyn Write) -> Result<()> {
        self.perform_with(out, query, run)
    }

    fn perform_with(
        &self,
        out: &mut dyn Write,
        query: impl Fn(&[&str]) -> Result<String>,
        run: impl Fn(&Command) -> Result<()>,
    ) -> Result<()> {
        let text = query(&["env", "list", "--json"])?;
        let envs: EnvironmentList = serde_json::from_str(&text)
            .map_err(|e| Error::Host(msg!("conda.query_invalid", error = e)))?;
        if let Some(managed) = &self.managed {
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
            let command = creation_command(managed);
            run(&command)?;
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

        let exists = match &self.selection {
            Selection::Prefix(prefix) => prefix.join("conda-meta/history").is_file(),
            Selection::Name(name) if name == "base" => {
                let output = query(&["info", "--base"])?;
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

fn creation_command(managed: &Preparation) -> Command {
    Command::new("conda")
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

fn query(args: &[&str]) -> Result<String> {
    let output = process::Command::new("conda")
        .args(args)
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

fn run(command: &Command) -> Result<()> {
    let status = process::Command::new(&command.program)
        .args(&command.args)
        .current_dir(
            command
                .cwd
                .as_ref()
                .expect("managed command has project cwd"),
        )
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
    use std::cell::Cell;
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
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn empty_envs(_: &[&str]) -> Result<String> {
        Ok(r#"{"envs":[]}"#.into())
    }

    fn create(command: &Command) -> Result<()> {
        assert_eq!(command.program, "conda");
        assert_eq!(command.args[..4], ["env", "create", "--yes", "--prefix"]);
        let prefix = Path::new(&command.args[4]);
        fs::create_dir_all(prefix.join("conda-meta")).unwrap();
        fs::write(prefix.join("conda-meta/history"), "created").unwrap();
        Ok(())
    }

    #[test]
    fn managed_creation_reuses_only_a_completed_matching_definition() {
        let project = Project::new();
        let config = project.managed();
        let step = CondaStep::new(&config, &project.0, true, false).unwrap();
        assert!(step.render().contains("python=3.12"));
        assert!(!project.0.join(".rospm").exists());
        let creates = Cell::new(0);
        let run = |command: &Command| {
            creates.set(creates.get() + 1);
            assert_eq!(command.cwd.as_deref(), Some(project.0.as_path()));
            create(command)
        };
        step.perform_with(&mut Vec::new(), empty_envs, run).unwrap();
        assert!(project.0.join(READY).is_file());
        step.perform_with(&mut Vec::new(), empty_envs, run).unwrap();
        assert_eq!(creates.get(), 1);
        let build = CondaStep::new(&config, &project.0, false, false).unwrap();
        build
            .perform_with(&mut Vec::new(), empty_envs, |_| {
                panic!("build must not create")
            })
            .unwrap();
        let mut changed = config;
        changed.managed.as_mut().unwrap().python = "3.11".into();
        let step = CondaStep::new(&changed, &project.0, true, false).unwrap();
        let error = step
            .perform_with(&mut Vec::new(), empty_envs, |_| {
                panic!("must not overwrite")
            })
            .unwrap_err();
        assert!(error.to_string().contains("conda.managed_conflict"));
    }

    #[test]
    fn failed_create_and_unprepared_build_never_mark_ready_or_adopt_directories() {
        let project = Project::new();
        let config = project.managed();
        let check = CondaStep::new(&config, &project.0, false, false).unwrap();
        assert!(check
            .perform_with(&mut Vec::new(), empty_envs, |_| panic!(
                "build must not create"
            ))
            .is_err());
        assert!(!project.0.join(".rospm").exists());
        let step = CondaStep::new(&config, &project.0, true, false).unwrap();
        let result = step.perform_with(&mut Vec::new(), empty_envs, |command| {
            create(command)?;
            Err(Error::CommandFailed {
                command: command.render(),
                status: "19".into(),
            })
        });
        assert!(matches!(result, Err(Error::CommandFailed { .. })));
        assert!(!project.0.join(READY).exists());
        assert!(step
            .perform_with(&mut Vec::new(), empty_envs, |_| panic!(
                "partial environment must not be adopted"
            ))
            .is_err());
        assert!(!project.0.join(READY).exists());
    }

    #[test]
    fn external_environments_are_checked_without_writes_and_missing_ones_fail() {
        let project = Project::new();
        let prefix = project.0.join("envs/robot");
        fs::create_dir_all(prefix.join("conda-meta")).unwrap();
        fs::write(prefix.join("conda-meta/history"), "external").unwrap();
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
            let step = CondaStep::new(&config, &project.0, true, false).unwrap();
            let query = |_: &[&str]| Ok(serde_json::json!({"envs": [prefix]}).to_string());
            step.perform_with(&mut Vec::new(), query, |_| {
                panic!("must not modify external environment")
            })
            .unwrap();
            fs::remove_file(prefix.join("conda-meta/history")).unwrap();
            assert!(step
                .perform_with(&mut Vec::new(), query, |_| panic!(
                    "must not create external environment"
                ))
                .unwrap_err()
                .to_string()
                .contains("conda.existing_missing"));
            fs::write(prefix.join("conda-meta/history"), "external").unwrap();
        }
        assert_eq!(
            fs::read_to_string(prefix.join("conda-meta/history")).unwrap(),
            "external"
        );
        assert!(!project.0.join(".rospm").exists());
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
        let step = CondaStep::new(&config, &project.0, true, false).unwrap();
        let rendered = step.render();
        assert!(rendered.contains(file.to_str().unwrap()));
        assert!(!rendered.contains("/external/must-not-touch"));
        assert!(!rendered.contains("shared-name"));
        assert!(rendered.contains("numpy") && rendered.contains("python=3.12"));
        assert!(rendered.contains("nodefaults"));
        let other =
            CondaStep::new(&other_project.managed(), &other_project.0, true, false).unwrap();
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
            assert!(CondaStep::new(&config, &project.0, true, false).is_err());
        }
        assert!(!project.0.join(".rospm").exists());
    }

    #[test]
    fn managed_locks_are_refused_before_writes_or_solving() {
        let project = Project::new();
        let mut config = project.managed();
        assert!(CondaStep::new(&config, &project.0, true, true).is_err());
        config.managed.as_mut().unwrap().lock_file = Some(PathBuf::from("locks/conda.lock"));
        let error = CondaStep::new(&config, &project.0, true, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains(project.0.join("locks/conda.lock").to_str().unwrap()));
        assert!(!project.0.join(".rospm").exists());
    }
}
