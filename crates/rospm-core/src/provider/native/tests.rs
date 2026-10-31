//! Unit tests for native provider helpers.

/// Runs the deactivation line with a fake active Conda whose `python3`
/// exits with `python_status`; returns (success, stdout, stderr).
fn run_deactivation(name: &str, python_status: u8, stale: bool) -> (bool, String, String) {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("rospm-deactivate-{name}-{}", std::process::id()));
    let conda = dir.join("conda root");
    std::fs::create_dir_all(conda.join("bin")).unwrap();
    std::fs::create_dir_all(conda.join("conda-meta")).unwrap();
    let python = conda.join("bin/python3");
    std::fs::write(&python, format!("#!/bin/sh\nexit {python_status}\n")).unwrap();
    std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
    if stale {
        std::fs::create_dir_all(dir.join("build/pkg")).unwrap();
        std::fs::write(
            dir.join("build/pkg/CMakeCache.txt"),
            format!(
                "Python3_EXECUTABLE:FILEPATH={}/bin/python3\n",
                conda.display()
            ),
        )
        .unwrap();
    }
    let script = format!(
        "set -e\n{}\necho \"PATH=$PATH\"; echo \"PREFIX=${{CONDA_PREFIX:-none}}\"",
        super::deactivate_unusable_conda("NOTE", "STALE", Some(&dir.join("build")))
    );
    let out = std::process::Command::new("bash")
        .args(["--noprofile", "--norc", "-c", &script])
        .current_dir(&dir)
        .env("PATH", format!("{}/bin:/usr/bin:/bin", conda.display()))
        .env("CONDA_PREFIX", &conda)
        .env("CONDA_EXE", conda.join("bin/conda"))
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn an_active_conda_that_cannot_build_ros_is_deactivated() {
    let (ok, out, err) = run_deactivation("unusable", 1, false);
    assert!(ok, "{err}");
    assert!(out.contains("PATH=/usr/bin:/bin\n"), "{out}");
    assert!(out.contains("PREFIX=none"), "{out}");
    assert_eq!(err, "NOTE\n");
}

#[test]
fn an_active_conda_that_can_build_ros_is_kept() {
    let (ok, out, err) = run_deactivation("usable", 0, true);
    assert!(ok, "{err}");
    assert!(out.contains("conda root/bin:/usr/bin:/bin"), "{out}");
    assert!(!out.contains("PREFIX=none"), "{out}");
    assert_eq!(err, "");
}

#[test]
fn caches_naming_the_deactivated_conda_stop_the_build() {
    let (ok, out, err) = run_deactivation("stale", 1, true);
    assert!(!ok);
    assert_eq!(out, "");
    assert!(
        err.starts_with("NOTE\nSTALE\n") && err.ends_with("/build/pkg/CMakeCache.txt\n"),
        "{err}"
    );
}

#[test]
fn colcon_bases_prefer_build_args_then_the_defaults_file_then_the_default() {
    use std::path::{Path, PathBuf};
    let root = std::env::temp_dir().join(format!("rospm-bases-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("d.yaml"), "build:\n  build-base: from_file\n").unwrap();
    let ws = |args: &[&str], defaults: Option<&str>| {
        let mut ws: crate::manifest::Workspace = toml::from_str("path = 'w'").unwrap();
        ws.colcon.build_args = args.iter().map(|a| a.to_string()).collect();
        ws.colcon.defaults = defaults.map(PathBuf::from);
        ws
    };
    let bases = |ws: &crate::manifest::Workspace, key, default| {
        super::colcon_bases(&root, ws, key, default)
    };
    let w = root.join("w");
    let with_args = ws(
        &["--build-base", "a", "--build-base=b", "--install-base=/abs"],
        Some("d.yaml"),
    );
    assert_eq!(
        bases(&with_args, "build-base", "build"),
        [w.join("b"), w.join("build")]
    );
    assert_eq!(
        bases(&with_args, "install-base", "install"),
        [PathBuf::from("/abs"), w.join("install")]
    );
    let from_file = ws(&["--symlink-install"], Some("d.yaml"));
    assert_eq!(
        bases(&from_file, "build-base", "build"),
        [w.join("from_file"), w.join("build")]
    );
    assert_eq!(
        bases(&from_file, "install-base", "install"),
        [w.join("install")]
    );
    let same = ws(&["--build-base", "./build"], None);
    assert_eq!(bases(&same, "build-base", "build"), [w.join("build")]);
    assert_eq!(
        super::existing_base(vec![w.join("b"), root.clone()]).as_deref(),
        Some(root.as_path())
    );
    assert_eq!(
        super::existing_base(vec![Path::new("/nonexistent/x").into()]),
        None
    );
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn take_option_removes_every_occurrence_and_keeps_the_last_value() {
    let mut args: Vec<String> = [
        "--log-base",
        "a",
        "-x",
        "--log-base=b",
        "--log-basement",
        "--log-base",
    ]
    .map(String::from)
    .into();
    assert_eq!(
        super::take_option(&mut args, "--log-base").as_deref(),
        Some("b")
    );
    // A trailing flag without a value is left for colcon to reject.
    assert_eq!(args, ["-x", "--log-basement", "--log-base"]);
    assert_eq!(
        super::take_option(&mut vec!["-x".into()], "--log-base"),
        None
    );
}

#[test]
fn reads_cmake_args_from_a_colcon_defaults_file() {
    let file = std::env::temp_dir().join(format!("rospm-defaults-{}.yaml", std::process::id()));
    std::fs::write(&file, "build:\n  cmake-args: [-DA=1, -DB=2]\n").unwrap();
    assert_eq!(super::default_cmake_args(&file), ["-DA=1", "-DB=2"]);
    std::fs::write(&file, "test: {}\n").unwrap();
    assert!(super::default_cmake_args(&file).is_empty());
    std::fs::remove_file(&file).unwrap();
}
