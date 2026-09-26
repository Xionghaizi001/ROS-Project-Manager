//! `rospm verify` end-to-end (spec §4, roadmap P2): comparing `rospm.lock`
//! with the host through the CLI. Read-only; never touches the system.

mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

use common::*;

const MANIFEST: &str = r#"schema_version = 1

[project]
name = "demo"
ros_distro = "jazzy"
platforms = ["linux-x86_64"]

[environment]
provider = "native"

[environment.native]
ubuntu = "24.04"
ros_base = "ros-jazzy-ros-base"

[workspaces.main]
path = "ws"
"#;

/// `git init` a one-commit repo at `dir`; returns the commit's full SHA.
fn git_repo(dir: &Path) -> String {
    fs::create_dir_all(dir).unwrap();
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git command");
        assert!(status.success(), "git {args:?} failed");
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test"]);
    fs::write(dir.join("README"), "hi").unwrap();
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "init"]);
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn lock_toml(manifest_text: &str, rev: &str) -> String {
    format!(
        r#"schema_version = 1
generated_at = "2026-01-01T00:00:00Z"
manifest_sha256 = "{sha}"

[provider]
kind = "native"

[[sources]]
workspace = "main"
name = "demo_pkg"
type = "git"
url = "https://example.com/demo.git"
rev = "{rev}"
"#,
        sha = rospm_core::lock::sha256_hex(manifest_text)
    )
}

#[test]
fn missing_lock_file_fails_with_a_hint_to_run_lock() {
    let dir = fixture("verify-no-lock");
    fs::write(dir.join("rospm.toml"), MANIFEST).unwrap();

    let out = rospm().current_dir(&dir).args(["verify"]).output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("rospm lock"), "{stderr}");

    let out = rospm()
        .current_dir(&dir)
        .args(["verify", "--json"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let v = json(&out);
    assert_eq!(v["ok"], false);
    assert!(v["error"].as_str().unwrap().contains("rospm lock"), "{v:#}");
}

#[test]
fn matching_lock_reports_ready_then_a_manifest_edit_breaks_it() {
    let dir = fixture("verify-ok");
    fs::write(dir.join("rospm.toml"), MANIFEST).unwrap();
    let rev = git_repo(&dir.join("ws/src/demo_pkg"));
    fs::write(dir.join("rospm.lock"), lock_toml(MANIFEST, &rev)).unwrap();

    let out = rospm()
        .current_dir(&dir)
        .args(["verify", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json(&out);
    assert_eq!(v["ok"], true);
    assert_eq!(v["manifest_matches"], true);
    let items = v["environment"]["items"].as_array().unwrap();
    let source = items
        .iter()
        .find(|i| i["subject"] == "main: src/demo_pkg")
        .expect("the locked source is reported");
    assert_eq!(source["status"], "ok", "{v:#}");
    assert_eq!(v["environment"]["ready"], true, "{v:#}");

    // Edit the manifest without re-locking: the hash no longer matches.
    let edited = format!("{MANIFEST}\n# a harmless comment\n");
    fs::write(dir.join("rospm.toml"), &edited).unwrap();

    let out = rospm()
        .current_dir(&dir)
        .args(["verify", "--json"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let v = json(&out);
    assert_eq!(v["ok"], true);
    assert_eq!(v["manifest_matches"], false);
    let items = v["environment"]["items"].as_array().unwrap();
    let manifest_row = items
        .iter()
        .find(|i| i["area"] == "manifest" && i["status"] == "mismatch")
        .expect("the manifest row reports the drift");
    assert!(
        manifest_row["actual"]
            .as_str()
            .unwrap()
            .contains("rospm lock"),
        "{v:#}"
    );
    assert_eq!(v["environment"]["ready"], false);

    // Same scenario without --json: text mode also fails.
    let out = rospm().current_dir(&dir).args(["verify"]).output().unwrap();
    assert!(!out.status.success());
}

#[test]
fn chinese_locale_shows_chinese_status_words() {
    let dir = fixture("verify-zh");
    fs::write(dir.join("rospm.toml"), MANIFEST).unwrap();
    // A source the lock expects but that was never imported: guarantees a
    // "missing" row so the table has something to translate.
    let lock_text = lock_toml(MANIFEST, "1234567890abcdef1234567890abcdef12345678");
    fs::write(dir.join("rospm.lock"), lock_text).unwrap();

    let out = rospm_zh()
        .current_dir(&dir)
        .args(["verify"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(has_cjk(&stdout), "{stdout}");
    assert!(!stdout.contains("MISSING"), "{stdout}");
}
