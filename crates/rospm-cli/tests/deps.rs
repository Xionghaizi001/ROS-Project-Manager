//! End-to-end checks of `rospm deps check` / `rospm deps install` (spec §4,
//! roadmap P2).
//!
//! Cache updates use a fake rosdep and a temporary ROS_HOME. Install tests
//! only preview the plan.

mod common;

use common::*;
use std::{fs, os::unix::fs::PermissionsExt};

#[test]
fn checks_update_a_missing_rosdep_cache_and_reuse_it() {
    let dir = fixture("check-rosdep-update");
    fs::write(
        dir.join("rospm.toml"),
        include_str!("../../rospm-core/tests/fixtures/minimal.toml"),
    )
    .unwrap();
    let bin = dir.join("bin");
    fs::create_dir(&bin).unwrap();
    let rosdep = bin.join("rosdep");
    fs::write(
        &rosdep,
        r#"#!/bin/sh
set -eu
case "$1" in
    update)
        test "$2" = --rosdistro && test "$3" = jazzy && test "$4" = -q
        echo update >> "$ROS_HOME/updates"
        test ! -e "$ROS_HOME/fail" || { echo 'simulated update failure' >&2; exit 1; }
        mkdir -p "$ROS_HOME/rosdep/sources.cache"
        touch "$ROS_HOME/rosdep/sources.cache/index"
        ;;
    check) test -f "$ROS_HOME/rosdep/sources.cache/index" ;;
    resolve)
        test -f "$ROS_HOME/rosdep/sources.cache/index"
        printf '#apt\nrospm-test-rclcpp\n'
        ;;
    *) exit 99 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&rosdep, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:/usr/bin:/bin", bin.display());

    for (subcommand, subject, expected) in [
        (&["check"][..], "rosdep update", "sources.cache"),
        (&["deps", "check"][..], "rclcpp", "rospm-test-rclcpp"),
    ] {
        let ros_home = dir.join(subject);
        fs::create_dir(&ros_home).unwrap();
        let check = |offline| {
            rospm()
                .current_dir(&dir)
                .args(subcommand)
                .arg("--json")
                .env("ROS_HOME", &ros_home)
                .env("ROSPM_NO_NETWORK", if offline { "1" } else { "0" })
                .env("PATH", &path)
                .output()
                .unwrap()
        };
        assert!(check(true).status.success());
        let updates = ros_home.join("updates");
        assert!(!updates.exists());
        fs::write(ros_home.join("fail"), "").unwrap();
        let failed = check(false);
        assert!(failed.status.success());
        assert!(String::from_utf8_lossy(&failed.stderr).contains("simulated update failure"));
        assert_eq!(json(&failed)["environment"]["ready"], false);
        fs::remove_file(ros_home.join("fail")).unwrap();

        let out = check(false);
        assert!(out.status.success(), "{:?}", out);
        let report = json(&out);
        let items = report["environment"]["items"].as_array().unwrap();
        let row = items.iter().find(|row| row["subject"] == subject).unwrap();
        assert!(row["expected"].as_str().unwrap().contains(expected));
        assert_eq!(
            row["status"],
            if subject == "rclcpp" { "missing" } else { "ok" }
        );
        assert!(check(false).status.success());
        assert_eq!(fs::read_to_string(updates).unwrap(), "update\nupdate\n");
    }
    fs::remove_dir_all(dir).unwrap();
}

/// `fixture()` + `init --from-env --json` gives us a manifest whose only
/// workspace has one package (`my_pkg`) depending on the external rosdep key
/// `rclcpp`, without ever writing to the real filesystem outside the temp dir.
fn project(name: &str) -> std::path::PathBuf {
    let dir = fixture(name);
    let out = rospm()
        .current_dir(&dir)
        .args(["init", "--from-env", "--force"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

#[test]
fn deps_check_json_lists_rclcpp_as_external() {
    let dir = project("deps-check-json");

    let out = rospm()
        .current_dir(&dir)
        .args(["deps", "check", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "deps check always exits 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json(&out);
    assert_eq!(v["ok"], true, "{v:#}");

    let external = v["external"].as_array().unwrap();
    assert!(
        external.iter().any(|k| k == "rclcpp"),
        "expected rclcpp in external: {v:#}"
    );
    assert!(v["internal"].as_array().unwrap().is_empty(), "{v:#}");
    assert!(v["skipped"].as_array().unwrap().is_empty(), "{v:#}");

    let ws = v["workspaces"].as_array().unwrap();
    assert_eq!(ws.len(), 1, "{v:#}");
    assert_eq!(ws[0]["name"], "main");
    assert_eq!(ws[0]["packages"], 1);
    assert_eq!(ws[0]["keys"], 1);

    // The environment row(s), if any, only ever come from the `deps`/`tool`
    // message areas; whatever rosdep makes of `rclcpp` on this host (ok,
    // unknown or missing) is acceptable -- only that the field is one of the
    // known statuses.
    let items = v["environment"]["items"].as_array().unwrap();
    for item in items {
        let status = item["status"].as_str().unwrap();
        assert!(
            ["ok", "missing", "mismatch", "unknown", "info"].contains(&status),
            "unexpected status `{status}` in {v:#}"
        );
    }
}

#[test]
fn deps_install_dry_run_only_plans() {
    let dir = project("deps-install-dry-run");

    let out = rospm()
        .current_dir(&dir)
        .args(["deps", "install", "--dry-run"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("# plan:"), "{stdout}");
    assert!(stdout.contains("dry-run: nothing executed"), "{stdout}");
    assert!(
        !stdout.contains("==> ["),
        "dry-run must not execute anything:\n{stdout}"
    );
}

#[test]
fn deps_check_in_chinese_prints_chinese_headers() {
    // `init --from-env` itself runs in English (`project()` uses `rospm()`);
    // only the `deps check` call below needs to be Chinese.
    let dir = project("deps-check-zh");

    let out = rospm_zh()
        .current_dir(&dir)
        .args(["deps", "check"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(has_cjk(&stdout), "{stdout}");
    assert!(stdout.contains("外部"), "{stdout}");
}
