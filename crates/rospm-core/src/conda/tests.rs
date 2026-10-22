use super::explicit::{exact_pins, resolve_lock_with};
use super::*;
use crate::lock::LockedConda;
use crate::manifest::ManagedConda;
use crate::message::Message;
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
    /// `sys.executable` reported inside the environment; default `<prefix>/bin/python`.
    interpreter: Option<PathBuf>,
    /// `command -v colcon` fails inside the environment.
    no_colcon: bool,
    /// `command -v colcon` finds the system's colcon.
    system_colcon: bool,
    /// Output of the import check with ROS sourced.
    imports: &'static str,
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
            interpreter: None,
            no_colcon: false,
            system_colcon: false,
            imports: r#"{"python": "3.12", "ros": ["3.12"], "failed": {}}"#,
        }
    }
}

impl System for Fake {
    fn query(&self, program: &str, args: &[&str]) -> Result<String> {
        if let ["run", "--no-capture-output", flag, target, rest @ ..] = args {
            let prefix = match *flag {
                "-p" => PathBuf::from(target),
                _ => self
                    .envs
                    .iter()
                    .find(|p| p.ends_with(target))
                    .cloned()
                    .unwrap(),
            };
            return match rest {
                ["python", "-c", _] => Ok(format!(
                    "{}\n",
                    self.interpreter
                        .clone()
                        .unwrap_or(prefix.join("bin/python"))
                        .display()
                )),
                ["bash", "--noprofile", "--norc", "-c", "command -v colcon"] if !self.no_colcon => {
                    Ok(match self.system_colcon {
                        true => "/usr/bin/colcon\n".into(),
                        false => format!("{}\n", prefix.join("bin/colcon").display()),
                    })
                }
                ["bash", "--noprofile", "--norc", "-c", script] if script.contains("rclpy") => {
                    assert!(script.starts_with("set -e\nsource /opt/ros/jazzy/setup.bash\n"));
                    Ok(format!("{}\n", self.imports))
                }
                _ => Err(Error::Host(msg!(
                    "conda.query_failed",
                    status = 1,
                    error = ""
                ))),
            };
        }
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
    // The same prefix (e.g. copied to a Jetson) is not ready for aarch64.
    let arm = CondaStep::new(
        &config,
        &project.0,
        &project.host(),
        Platform::LinuxAarch64,
        false,
        None,
    )
    .unwrap();
    assert!(!arm.managed.as_ref().unwrap().is_ready());
    // A compatible conda on PATH is used as is; nothing is installed.
    assert_eq!(*fake.creates.borrow(), ["conda"]);
    assert_eq!(fake.downloads.get() + fake.installs.get(), 0);
    assert!(!project.0.join("user").exists());
    project
        .step(&config, false)
        .perform_with(&mut Vec::new(), &fake)
        .unwrap();
    // A changed definition recreates the environment rospm created.
    let mut changed = config;
    changed.managed.as_mut().unwrap().python = "3.11".into();
    let step = project.step(&changed, true);
    assert!(step.render().contains("conda.recreate("));
    step.perform_with(&mut Vec::new(), &fake).unwrap();
    assert_eq!(fake.creates.borrow().len(), 2);
    assert!(step.managed.as_ref().unwrap().is_ready());
    assert!(step.render().contains("conda.reused("));
}

#[test]
fn failed_create_is_redone_but_foreign_directories_are_never_adopted() {
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
    // The partial environment is never ready, not even for build.
    assert!(!step.managed.as_ref().unwrap().is_ready());
    assert!(project
        .step(&config, false)
        .perform_with(&mut Vec::new(), &fake)
        .is_err());
    // Rerunning apply recreates it from scratch.
    assert!(step.render().contains("conda.recreate("));
    fs::write(project.0.join(MANAGED_PREFIX).join("partial"), "").unwrap();
    step.perform_with(&mut Vec::new(), &Fake::default())
        .unwrap();
    assert!(!project.0.join(MANAGED_PREFIX).join("partial").exists());
    assert!(step.managed.as_ref().unwrap().is_ready());

    // A prefix without rospm's record is someone else's: left untouched.
    let foreign = Project::new();
    fs::create_dir_all(foreign.0.join(MANAGED_PREFIX).join("conda-meta")).unwrap();
    fs::write(
        foreign.0.join(MANAGED_PREFIX).join("conda-meta/history"),
        "theirs",
    )
    .unwrap();
    let step = foreign.step(&config, true);
    assert!(step.render().contains("conda.managed_conflict("));
    let error = step.perform_with(&mut Vec::new(), &fake).unwrap_err();
    assert!(error.to_string().contains("conda.managed_conflict"));
    assert_eq!(
        fs::read_to_string(foreign.0.join(MANAGED_PREFIX).join("conda-meta/history")).unwrap(),
        "theirs"
    );
    assert!(!foreign.0.join(READY).exists());
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
fn build_requires_the_interpreter_and_tools_inside_the_environment() {
    let project = Project::new();
    let prefix = project.0.join("envs/robot");
    fs::create_dir_all(prefix.join("conda-meta")).unwrap();
    fs::write(prefix.join("conda-meta/history"), "external").unwrap();
    let envs = vec![prefix.clone()];
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
        let step = project.step(&config, false);
        assert!(step.render().contains("conda.build_checks("));
        let fake = Fake {
            envs: envs.clone(),
            ..Fake::default()
        };
        let mut out = Vec::new();
        step.perform_with(&mut out, &fake).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.contains(prefix.join("bin/python").to_str().unwrap()),
            "{out}"
        );

        // A system interpreter or a missing colcon stops the build.
        let system = Fake {
            envs: envs.clone(),
            interpreter: Some("/usr/bin/python3".into()),
            ..Fake::default()
        };
        let error = step.perform_with(&mut Vec::new(), &system).unwrap_err();
        assert!(
            error.to_string().contains("conda.interpreter_outside("),
            "{error}"
        );
        let no_colcon = Fake {
            envs: envs.clone(),
            no_colcon: true,
            ..Fake::default()
        };
        let error = step.perform_with(&mut Vec::new(), &no_colcon).unwrap_err();
        assert!(
            error.to_string().contains("conda.build_tool_missing("),
            "{error}"
        );
    }
    // A colcon from the system would build with the system Python.
    let system = Fake {
        envs: envs.clone(),
        system_colcon: true,
        ..Fake::default()
    };
    let step = project.step(
        &CondaBuild {
            name: Some("robot".into()),
            ..Default::default()
        },
        false,
    );
    let error = step.perform_with(&mut Vec::new(), &system).unwrap_err();
    assert!(
        error.to_string().contains("conda.build_tool_outside("),
        "{error}"
    );
    // apply only prepares; the checks belong to build.
    let fake = Fake {
        envs,
        no_colcon: true,
        ..Fake::default()
    };
    project
        .step(
            &CondaBuild {
                name: Some("robot".into()),
                ..Default::default()
            },
            true,
        )
        .perform_with(&mut Vec::new(), &fake)
        .unwrap();
    // A ready managed environment is checked at its project prefix.
    let config = project.managed();
    project
        .step(&config, true)
        .perform_with(&mut Vec::new(), &Fake::default())
        .unwrap();
    let step = project.step(&config, false);
    assert!(step.render().contains(
        project
            .0
            .join(MANAGED_PREFIX)
            .join("bin/python")
            .to_str()
            .unwrap()
    ));
    step.perform_with(&mut Vec::new(), &Fake::default())
        .unwrap();
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
    assert!(preview
        .contains("pip install --no-deps --no-input --disable-pip-version-check requests==2.32.3"));
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
        format!(
            "{} linux-64\n{}",
            record.definition_sha256, record.lock_sha256
        )
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
    // apply --locked replaces it with the replay instead.
    let step = locked_step(&solved, &config, &lock_with(vec![other])).unwrap();
    assert!(step.render().contains("conda.recreate("));
    let replayer = Fake::default();
    step.perform_with(&mut Vec::new(), &replayer).unwrap();
    assert!(replayer.creates.borrow().is_empty());
    assert!(step.managed.as_ref().unwrap().is_ready());
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
    assert_eq!(
        exact_pins(&["requests == 2.32.3".into()]).unwrap(),
        [("requests".to_string(), "2.32.3".to_string())]
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
    assert!(old.creates.borrow()[0].ends_with("miniforge3-26.7.2-0-linux-64/bin/conda"));
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

#[test]
fn numeric_and_boolean_variables_are_read_as_text() {
    let project = Project::new();
    let config = pinned(&project, "rich==13.9.4");
    let file = project.0.join("environment.yml");
    let managed = config.managed.as_ref().unwrap();
    fs::write(
        &file,
        "dependencies: [numpy]\nvariables:\n  ROS_DOMAIN_ID: 0\n  USE_SIM: true\n  SCALE: 1.5\n  ROBOT: one\n",
    )
    .unwrap();
    let definition = environment_yaml(managed, &project.0).unwrap();
    let pairs: Vec<_> = definition
        .variables
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    assert_eq!(
        pairs,
        ["ROS_DOMAIN_ID=0", "USE_SIM=true", "SCALE=1.5", "ROBOT=one"]
    );

    fs::write(
        &file,
        "dependencies: [numpy]\nvariables:\n  PATHS: [a, b]\n",
    )
    .unwrap();
    let err = environment_yaml(managed, &project.0)
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("variables.PATHS"), "{err}");
}

#[test]
fn build_diagnoses_imports_and_another_interpreters_cmake_cache() {
    let project = Project::new();
    let prefix = project.0.join("envs/robot");
    fs::create_dir_all(prefix.join("conda-meta")).unwrap();
    fs::write(prefix.join("conda-meta/history"), "external").unwrap();
    let build = project.0.join("ws/build");
    let step = project
        .step(
            &CondaBuild {
                prefix: Some(PathBuf::from("envs/robot")),
                ..Default::default()
            },
            false,
        )
        .for_build(
            "source /opt/ros/jazzy/setup.bash".into(),
            vec![build.clone()],
        );
    let check = |imports| {
        step.perform_with(
            &mut Vec::new(),
            &Fake {
                imports,
                ..Fake::default()
            },
        )
    };
    check(r#"{"python": "3.12", "ros": ["3.12"], "failed": {}}"#).unwrap();

    // Another CPython than ROS's: the extension cannot load.
    let error = check(
        r#"{"python": "3.11", "ros": ["3.12"], "failed": {"rclpy._rclpy_pybind11": "ModuleNotFoundError: x"}}"#,
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("conda.python_abi(") && error.contains("3.11"),
        "{error}"
    );
    let error =
        check(r#"{"python": "3.12", "ros": ["3.12"], "failed": {"em": "ModuleNotFoundError: x"}}"#)
            .unwrap_err()
            .to_string();
    assert!(
        error.contains("conda.python_modules(") && error.contains("em ("),
        "{error}"
    );

    // Caches naming this interpreter (or none) pass; another one is reported.
    fs::create_dir_all(build.join("ok")).unwrap();
    fs::write(
        build.join("ok/CMakeCache.txt"),
        format!(
            "PYTHON_EXECUTABLE:FILEPATH=PYTHON_EXECUTABLE-NOTFOUND\nPython3_EXECUTABLE:FILEPATH={}\n",
            prefix.join("bin/python").display()
        ),
    )
    .unwrap();
    fs::create_dir_all(build.join("plain")).unwrap();
    check(r#"{"python": "3.12", "ros": ["3.12"], "failed": {}}"#).unwrap();
    fs::create_dir_all(build.join("old")).unwrap();
    fs::write(
        build.join("old/CMakeCache.txt"),
        "Python3_EXECUTABLE:FILEPATH=/usr/bin/python3\n",
    )
    .unwrap();
    let error = check(r#"{"python": "3.12", "ros": ["3.12"], "failed": {}}"#)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("conda.stale_cache(")
            && error.contains("/usr/bin/python3")
            && !error.contains("ok ("),
        "{error}"
    );
}
