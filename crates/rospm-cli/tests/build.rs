//! Conda selection and build invocation; no Conda or ROS installation required.
mod common;

use std::fs;
use std::path::Path;

use common::*;

const MINIMAL: &str = include_str!("../../rospm-core/tests/fixtures/minimal.toml");

fn preview(dir: &Path, args: &[&str]) -> String {
    let out = rospm()
        .current_dir(dir)
        .args(["build", "--dry-run"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn conda_cli_overrides_the_entire_manifest_selection_without_writing() {
    let dir = fixture("build-conda-overrides");
    let manifest = dir.join("rospm.toml");
    fs::write(&manifest, MINIMAL).unwrap();
    let plain = preview(&dir, &[]);
    assert!(plain.contains("colcon build"), "{plain}");
    assert!(!plain.contains("conda run"), "{plain}");

    let named = format!("{MINIMAL}\n[build.conda]\nname = 'manifest-env'\n");
    fs::write(&manifest, &named).unwrap();
    assert!(preview(&dir, &[]).contains("conda run --no-capture-output -n manifest-env"));
    let by_prefix = preview(&dir, &["--conda-prefix", "env with spaces"]);
    let cli_prefix = dir.join("env with spaces");
    assert!(
        by_prefix.contains(&format!("-p '{}'", cli_prefix.display())),
        "{by_prefix}"
    );
    assert!(!by_prefix.contains("-n manifest-env"));
    assert_eq!(fs::read_to_string(&manifest).unwrap(), named);

    let prefixed = format!("{MINIMAL}\n[build.conda]\nprefix = '.conda/project env'\n");
    fs::write(&manifest, &prefixed).unwrap();
    // Invoke from a nested directory: manifest and CLI relative paths have
    // deliberately different bases, neither is relative to the workspace.
    let nested = dir.join("nested");
    fs::create_dir(&nested).unwrap();
    let default = preview(&nested, &[]);
    assert!(
        default.contains(&format!(
            "-p '{}'",
            dir.join(".conda/project env").display()
        )),
        "{default}"
    );
    let named_override = preview(&nested, &["--conda-env", "cli-env"]);
    assert!(named_override.contains("-n cli-env"), "{named_override}");
    assert!(!named_override.contains("-p "));
    let relative_override = preview(&nested, &["--conda-prefix", "local-env"]);
    assert!(
        relative_override.contains(&format!("-p {}", nested.join("local-env").display())),
        "{relative_override}"
    );
    let absolute = dir.join("absolute-env");
    assert!(
        preview(&nested, &["--conda-prefix", absolute.to_str().unwrap()])
            .contains(&format!("-p {}", absolute.display()))
    );
    assert_eq!(fs::read_to_string(&manifest).unwrap(), prefixed);
    for path in [
        ".rospm",
        ".conda",
        "env with spaces",
        "nested/local-env",
        "ws/build",
        "ws/install",
        "ws/log",
    ] {
        assert!(!dir.join(path).exists(), "preview created {path}");
    }
}

#[test]
fn conda_flags_are_mutually_exclusive_and_reject_empty_values() {
    let dir = fixture("build-conda-invalid");
    fs::write(dir.join("rospm.toml"), MINIMAL).unwrap();
    for args in [
        vec!["--conda-env", "robot", "--conda-prefix", "env"],
        vec!["--conda-env", ""],
        vec!["--conda-env", "   "],
        vec!["--conda-prefix", ""],
    ] {
        let out = rospm()
            .current_dir(&dir)
            .args(["build", "--dry-run"])
            .args(&args)
            .output()
            .unwrap();
        assert!(!out.status.success(), "accepted {args:?}");
    }
}

#[test]
fn conda_help_and_configuration_errors_are_localized() {
    let dir = fixture("build-conda-help");
    fs::write(
        dir.join("rospm.toml"),
        format!("{MINIMAL}\n[build.conda]\n"),
    )
    .unwrap();
    for (lang, help, error) in [
        (
            "en",
            "overriding build.conda",
            "exactly one of name or prefix",
        ),
        ("zh", "覆盖 build.conda", "必须且只能设置"),
    ] {
        let out = rospm()
            .args(["--lang", lang, "build", "--help"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(text.contains("--conda-env") && text.contains("--conda-prefix"));
        assert!(text.contains(help), "{text}");
        let out = rospm()
            .current_dir(&dir)
            .args(["--lang", lang, "build", "--dry-run"])
            .output()
            .unwrap();
        assert!(!out.status.success());
        let text = String::from_utf8(out.stderr).unwrap();
        assert!(text.contains(error), "{text}");
    }
}

#[cfg(unix)]
#[test]
fn build_invokes_conda_and_propagates_its_failure_without_fallback() {
    use std::os::unix::fs::PermissionsExt;

    let dir = fixture("build-conda-execute");
    fs::write(
        dir.join("rospm.toml"),
        format!("{MINIMAL}\n[build.conda]\nname = 'robot'\n"),
    )
    .unwrap();
    let bin = dir.join("bin");
    fs::create_dir(&bin).unwrap();
    let conda = bin.join("conda");
    fs::write(
        &conda,
        "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\" > \"$CONDA_CALL\"\nexit 19\n",
    )
    .unwrap();
    fs::set_permissions(&conda, fs::Permissions::from_mode(0o755)).unwrap();
    let call = dir.join("conda-call");
    let out = rospm()
        .current_dir(&dir)
        .env("PATH", &bin)
        .env("CONDA_CALL", &call)
        .args(["build", "--dry-run"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!call.exists());
    let out = rospm()
        .current_dir(&dir)
        .env("PATH", &bin)
        .env("CONDA_CALL", &call)
        .args(["build"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let args = fs::read_to_string(&call).unwrap();
    assert!(
        args.starts_with(&format!(
            "{}\nrun\n--no-capture-output\n-n\nrobot\nbash\n--noprofile\n--norc\n-c\n",
            dir.join("ws").display()
        )),
        "{args}"
    );
    assert!(args.contains("source /opt/ros/jazzy/setup.bash"));
    assert!(args.contains("colcon build"));
    assert!(String::from_utf8_lossy(&out.stderr).contains("19"));
}
