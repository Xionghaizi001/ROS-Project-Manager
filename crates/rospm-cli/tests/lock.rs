//! `rospm lock` end to end, network-free (`ROSPM_NO_NETWORK=1`).

mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

use common::*;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn lock_cmd(dir: &Path) -> Command {
    let mut c = rospm();
    c.current_dir(dir).env("ROSPM_NO_NETWORK", "1");
    c
}

#[test]
fn lock_refuses_unimported_repos_then_writes_and_is_idempotent() {
    let dir = fixture("lock");
    // The fixture's main.repos names ros2/demos, which is not imported.
    assert!(lock_cmd(&dir)
        .args(["init", "--from-env"])
        .output()
        .unwrap()
        .status
        .success());
    let out = lock_cmd(&dir).args(["lock", "--json"]).output().unwrap();
    assert!(!out.status.success());
    let v = json(&out);
    assert_eq!(v["ok"], false);
    let err = v["error"].as_str().unwrap();
    assert!(
        err.contains("not imported") && err.contains("rospm apply"),
        "{err}"
    );
    assert!(!dir.join("rospm.lock").exists());
    let out = rospm_zh()
        .current_dir(&dir)
        .env("ROSPM_NO_NETWORK", "1")
        .arg("lock")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("尚未导入"), "{stderr}");

    // Point main.repos at a local repository and import it.
    let upstream = dir.join("upstream");
    fs::create_dir_all(&upstream).unwrap();
    git(&upstream, &["init", "-q", "-b", "main"]);
    fs::write(upstream.join("README"), "x").unwrap();
    git(&upstream, &["add", "-A"]);
    git(&upstream, &["commit", "-qm", "init"]);
    fs::write(
        dir.join("main.repos"),
        format!(
            "repositories:\n  ros2/demos:\n    type: git\n    url: file://{}\n    version: main\n",
            upstream.display()
        ),
    )
    .unwrap();
    fs::create_dir_all(dir.join("ws/src/ros2")).unwrap();
    git(
        &dir.join("ws/src/ros2"),
        &["clone", "-q", upstream.to_str().unwrap(), "demos"],
    );

    let has_dpkg = which::which("dpkg-query").is_ok();
    let out = lock_cmd(&dir).args(["lock", "--json"]).output().unwrap();
    if !has_dpkg {
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).contains("dpkg-query"));
        return;
    }
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json(&out);
    assert_eq!(v["ok"], true);
    assert_eq!(v["changed"], true);
    assert_eq!(v["sources"], 1);
    assert!(v["ros_snapshot"].is_null());
    assert!(v["notes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n.as_str().unwrap().contains("ROSPM_NO_NETWORK")));
    let text = fs::read_to_string(dir.join("rospm.lock")).unwrap();
    assert!(text.contains("name = \"ros2/demos\""), "{text}");
    assert!(text.contains("version = \"main\""), "{text}");

    // Same input, same output: the file is not rewritten.
    let out = lock_cmd(&dir).args(["lock", "--json"]).output().unwrap();
    assert!(out.status.success());
    let v2 = json(&out);
    assert_eq!(v2["changed"], false);
    assert_eq!(v2["generated_at"], v["generated_at"]);
    assert_eq!(fs::read_to_string(dir.join("rospm.lock")).unwrap(), text);
    let out = lock_cmd(&dir).arg("lock").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("unchanged"));

    // A foreign platform cannot be resolved here.
    let foreign = if cfg!(target_arch = "x86_64") {
        "linux-aarch64"
    } else {
        "linux-x86_64"
    };
    let out = lock_cmd(&dir)
        .args(["lock", "--platform", foreign])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains(foreign));
    let _ = fs::remove_dir_all(&dir);
}
