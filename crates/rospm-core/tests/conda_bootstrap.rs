//! Real Miniforge bootstrap: downloads ~125 MB and creates an environment.
//! Run on a disposable machine without Conda, as a regular user:
//!
//! ```sh
//! cargo test -p rospm-core --test conda_bootstrap -- --ignored --nocapture
//! ```
//!
//! Optional: `ROSPM_TEST_CONDA_CHANNEL` replaces `conda-forge` with a mirror
//! URL; `ROSPM_TEST_CACHE_DIR` keeps the downloaded installer between runs.
//!
//! `locks_and_replays_a_real_environment` instead needs a compatible `conda`
//! on PATH (`PIP_INDEX_URL` may select a PyPI mirror):
//!
//! ```sh
//! cargo test -p rospm-core --test conda_bootstrap locks -- --ignored --nocapture
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rospm_core::conda::{resolve_lock, CondaStep};
use rospm_core::lock::{sha256_hex, Lock, LockedProvider, SUPPORTED_SCHEMA_VERSION};
use rospm_core::manifest::{CondaBuild, ManagedConda, ProviderKind};
use rospm_core::platform::Platform;
use rospm_core::probe::CondaHost;

fn startup_files(home: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    [".bashrc", ".bash_profile", ".profile", ".zshrc", ".condarc"]
        .iter()
        .map(|f| home.join(f))
        .map(|p| {
            let contents = fs::read(&p).ok();
            (p, contents)
        })
        .collect()
}

#[test]
#[ignore = "downloads and installs the real Miniforge"]
fn installs_pinned_miniforge_without_conda_and_reuses_it() {
    assert!(
        which::which("conda").is_err(),
        "run on a host without conda on PATH"
    );
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let before = startup_files(&home);
    let root = std::env::temp_dir().join(format!("rospm-bootstrap-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("config")).unwrap();
    fs::write(
        root.join("config/environment.yml"),
        "dependencies: [numpy]\n",
    )
    .unwrap();
    let host = CondaHost {
        data_dir: root.join("user/data"),
        cache_dir: std::env::var_os("ROSPM_TEST_CACHE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("user/cache")),
    };
    let config = CondaBuild {
        managed: Some(ManagedConda {
            python: "3.12".into(),
            channels: vec![
                std::env::var("ROSPM_TEST_CONDA_CHANNEL").unwrap_or_else(|_| "conda-forge".into())
            ],
            environment_file: Some("config/environment.yml".into()),
            lock_file: None,
        }),
        ..Default::default()
    };
    let platform = Platform::current().expect("supported platform");
    let step = |prepare| CondaStep::new(&config, &root, &host, platform, prepare, None).unwrap();

    let first = step(true);
    println!("{}", first.render());
    let mut out = Vec::new();
    first.perform(&mut out).unwrap();
    let log = String::from_utf8(out).unwrap();
    println!("{log}");
    assert!(log.contains("conda.installed("));

    // Planning now selects the installed tool, and build runs through it.
    let build = step(false);
    assert!(build.render().starts_with("conda.check("));
    let command = build.command();
    assert!(
        command.program.ends_with("/bin/conda"),
        "{}",
        command.program
    );
    let conda = PathBuf::from(&command.program);
    assert!(conda.starts_with(root.join("user/data/conda")));
    build.perform(&mut Vec::new()).unwrap();
    let probe = Command::new(&command.program)
        .args(&command.args)
        .args([
            "python",
            "-c",
            "import sys, numpy; print(sys.executable); print(numpy.__version__)",
        ])
        .output()
        .unwrap();
    assert!(probe.status.success(), "{probe:?}");
    let stdout = String::from_utf8(probe.stdout).unwrap();
    assert!(
        stdout.starts_with(root.join(".rospm/conda/env/bin/python").to_str().unwrap()),
        "{stdout}"
    );

    // An interrupted reinstall resumes from the verified cache.
    let ready = conda
        .parent()
        .and_then(Path::parent)
        .map(|r| PathBuf::from(format!("{}.ready", r.display())))
        .unwrap();
    assert!(ready.is_file(), "{}", ready.display());
    fs::remove_file(&ready).unwrap();
    fs::remove_file(&conda).unwrap();
    let again = step(true);
    assert!(again.render().starts_with("conda.bootstrap("));
    let mut out = Vec::new();
    again.perform(&mut out).unwrap();
    let log = String::from_utf8(out).unwrap();
    println!("{log}");
    assert!(log.contains("conda.installer_cached(") && !log.contains("conda.downloading("));
    assert!(log.contains("conda.reused("));

    assert_eq!(startup_files(&home), before, "shell startup files changed");
    fs::remove_dir_all(&root).unwrap();
}

fn explicit(prefix: &Path) -> String {
    let out = Command::new("conda")
        .args(["list", "--explicit", "--sha256", "--prefix"])
        .arg(prefix)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout).unwrap()
}

#[test]
#[ignore = "creates real Conda environments from the network"]
fn locks_and_replays_a_real_environment() {
    which::which("conda").expect("a compatible conda on PATH");
    let base = std::env::temp_dir().join(format!("rospm-conda-lock-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let (machine, clean) = (base.join("machine"), base.join("clean"));
    for root in [&machine, &clean] {
        fs::create_dir_all(root).unwrap();
        fs::write(
            root.join("environment.yml"),
            "dependencies:\n  - pip\n  - pip: ['six==1.16.0']\nvariables:\n  ROSPM_ROBOT: replayed\n",
        )
        .unwrap();
    }
    let host = CondaHost {
        data_dir: base.join("user/data"),
        cache_dir: base.join("user/cache"),
    };
    let config = CondaBuild {
        managed: Some(ManagedConda {
            python: "3.12".into(),
            channels: vec![
                std::env::var("ROSPM_TEST_CONDA_CHANNEL").unwrap_or_else(|_| "conda-forge".into())
            ],
            environment_file: Some("environment.yml".into()),
            lock_file: Some("locks/conda-{platform}.lock".into()),
        }),
        ..Default::default()
    };
    let platform = Platform::current().expect("supported platform");

    // Solve and create on the first machine, then lock it.
    CondaStep::new(&config, &machine, &host, platform, true, None)
        .unwrap()
        .perform(&mut Vec::new())
        .unwrap();
    let mut notes = Vec::new();
    let record = resolve_lock(&config, &machine, &host, platform, &mut notes)
        .unwrap()
        .unwrap_or_else(|| panic!("{notes:?}"));
    println!("{notes:?}\n{record:?}");
    let text = fs::read_to_string(machine.join(&record.lock)).unwrap();
    assert_eq!(text, explicit(&machine.join(".rospm/conda/env")));
    assert!(text.contains("/python-3.12"), "{text}");
    let lock = Lock {
        schema_version: SUPPORTED_SCHEMA_VERSION,
        generated_at: "2026-10-20T00:00:00Z".into(),
        manifest_sha256: sha256_hex("manifest"),
        provider: LockedProvider {
            kind: ProviderKind::Native,
            native: None,
            pixi: None,
            docker: None,
        },
        sources: Vec::new(),
        apt: Vec::new(),
        pip: Vec::new(),
        conda: vec![record.clone()],
    };
    // Same host: the locked environment is reused as is.
    let mut out = Vec::new();
    CondaStep::new(&config, &machine, &host, platform, true, Some(&lock))
        .unwrap()
        .perform(&mut out)
        .unwrap();
    assert!(String::from_utf8(out).unwrap().contains("conda.reused("));

    // A clean project with only the files: replay without solving.
    fs::create_dir_all(clean.join("locks")).unwrap();
    fs::copy(machine.join(&record.lock), clean.join(&record.lock)).unwrap();
    let step = CondaStep::new(&config, &clean, &host, platform, true, Some(&lock)).unwrap();
    println!("{}", step.render());
    step.perform(&mut Vec::new()).unwrap();
    let prefix = clean.join(".rospm/conda/env");
    assert_eq!(explicit(&prefix), text, "replay installed other packages");
    let probe = step
        .command()
        .args([
            "python",
            "-c",
            "import os, six; print(six.__version__, os.environ['ROSPM_ROBOT'])",
        ])
        .render();
    let probe = Command::new("bash").args(["-c", &probe]).output().unwrap();
    assert!(probe.status.success(), "{probe:?}");
    assert_eq!(
        String::from_utf8(probe.stdout).unwrap().trim(),
        "1.16.0 replayed"
    );
    // The replayed environment locks to the same record.
    let mut notes = Vec::new();
    assert_eq!(
        resolve_lock(&config, &clean, &host, platform, &mut notes).unwrap(),
        Some(record)
    );
    fs::remove_dir_all(&base).unwrap();
}
