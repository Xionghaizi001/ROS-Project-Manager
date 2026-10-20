//! Real Miniforge bootstrap: downloads ~125 MB and creates an environment.
//! Run on a disposable machine without Conda, as a regular user:
//!
//! ```sh
//! cargo test -p rospm-core --test conda_bootstrap -- --ignored --nocapture
//! ```
//!
//! Optional: `ROSPM_TEST_CONDA_CHANNEL` replaces `conda-forge` with a mirror
//! URL; `ROSPM_TEST_CACHE_DIR` keeps the downloaded installer between runs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rospm_core::conda::CondaStep;
use rospm_core::manifest::{CondaBuild, ManagedConda};
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
        on_path: None,
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
    let step = |prepare| CondaStep::new(&config, &root, &host, platform, prepare, false).unwrap();

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
