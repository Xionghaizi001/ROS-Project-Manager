//! End-to-end checks of `rospm deps check` / `rospm deps install` (spec §4,
//! roadmap P2).
//!
//! These only touch a temporary directory and never modify the system:
//! `deps check` is read-only and `deps install` is only ever run with
//! `--dry-run`. Whether this host's `rosdep` can actually resolve `rclcpp`
//! (it has no `rosdep update` cache for any ROS distro) is host-dependent,
//! so assertions on the environment rows accept any of ok/unknown/missing
//! rather than pinning one outcome.

mod common;

use common::*;

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
