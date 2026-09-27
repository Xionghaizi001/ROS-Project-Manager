//! `rospm apply --locked` (roadmap P2, spec §3.2): lock validation and the
//! snapshot-pinned plan. Only `--dry-run` is ever run here (safety
//! constraint: a real `apply` mutates the host).

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use rospm_core::lock::{sha256_hex, Lock, LockedApt, LockedNative, LockedProvider, LockedSource};
use rospm_core::manifest::ProviderKind;
use rospm_core::platform::Platform;

use common::{has_cjk, rospm, rospm_zh};

/// This host's Ubuntu release mapped to a distro rospm supports, or `None`
/// on anything else (`plan_apply` refuses a manifest for another release
/// before it gets anywhere near `--locked`, so these tests do not apply there).
fn host_distro() -> Option<(&'static str, &'static str, &'static str)> {
    let os = fs::read_to_string("/etc/os-release").ok()?;
    let version = os
        .lines()
        .find_map(|l| l.strip_prefix("VERSION_ID="))
        .map(|v| v.trim_matches('"'))?;
    match version {
        "22.04" => Some(("22.04", "humble", "ros-humble-ros-base")),
        "24.04" => Some(("24.04", "jazzy", "ros-jazzy-ros-base")),
        _ => None,
    }
}

/// A project fixture: one workspace, one git source (`demo_pkg`), rosdep
/// disabled (kept out of the plan so assertions stay focused on `--locked`).
fn fixture_dir(name: &str, ubuntu: &str, distro: &str, ros_base: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rospm-cli-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("ws/src/my_pkg")).unwrap();
    fs::write(
        dir.join("ws/src/my_pkg/package.xml"),
        "<package format=\"3\"><name>my_pkg</name><depend>rclcpp</depend></package>",
    )
    .unwrap();
    fs::write(
        dir.join("main.repos"),
        "repositories:\n  demo_pkg:\n    type: git\n    url: https://github.com/example/demo_pkg.git\n    version: main\n",
    )
    .unwrap();
    fs::write(
        dir.join("rospm.toml"),
        format!(
            r#"schema_version = 1

[project]
name = "locked_demo"
ros_distro = "{distro}"
platforms = ["linux-x86_64"]

[environment]
provider = "native"

[environment.native]
ubuntu = "{ubuntu}"
ros_base = "{ros_base}"

[workspaces.main]
path = "ws"
sources = "main.repos"
underlays = []

[dependencies.rosdep]
enabled = false
"#
        ),
    )
    .unwrap();
    dir
}

/// A lock consistent with the manifest at `dir/rospm.toml`: pinned apt for
/// `linux-x86_64` only, the `demo_pkg` git source pinned to a revision, and
/// (unless overridden by the caller) both snapshots recorded.
fn write_lock(
    dir: &Path,
    ros_base: &str,
    ros_snapshot: Option<&str>,
    ubuntu_snapshot: Option<&str>,
) {
    let manifest_text = fs::read_to_string(dir.join("rospm.toml")).unwrap();
    let lock = Lock {
        schema_version: 1,
        generated_at: "2026-09-11T00:00:00Z".into(),
        manifest_sha256: sha256_hex(&manifest_text),
        provider: LockedProvider {
            kind: ProviderKind::Native,
            native: Some(LockedNative {
                ubuntu: "unused-by-apply".into(), // apply reads the manifest, not this
                ros_snapshot: ros_snapshot.map(str::to_string),
                ubuntu_snapshot: ubuntu_snapshot.map(str::to_string),
            }),
            pixi: None,
            docker: None,
        },
        sources: vec![LockedSource {
            workspace: "main".into(),
            name: "demo_pkg".into(),
            kind: "git".into(),
            url: "https://github.com/example/demo_pkg.git".into(),
            rev: "cafef00ddeadbeefcafef00ddeadbeefcafef00d".into(),
            version: None,
        }],
        apt: vec![LockedApt {
            platform: Platform::LinuxX86_64,
            name: ros_base.into(),
            version: "1.2.3-1noble.20260910.000000".into(),
            sha256: None,
        }],
        pip: vec![],
    };
    lock.save(&dir.join("rospm.lock")).unwrap();
}

#[test]
fn apply_locked_without_a_lock_file_is_an_error() {
    let dir = fixture_dir("no-lock", "22.04", "humble", "ros-humble-ros-base");
    let manifest = dir.join("rospm.toml").to_string_lossy().into_owned();
    let out = rospm()
        .args(["--manifest", &manifest, "apply", "--locked", "--dry-run"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no rospm.lock; run `rospm lock` first"),
        "{stderr}"
    );
}

#[test]
fn apply_locked_with_a_stale_lock_is_an_error() {
    let dir = fixture_dir("stale-lock", "22.04", "humble", "ros-humble-ros-base");
    write_lock(
        &dir,
        "ros-humble-ros-base",
        Some("2026-09-11"),
        Some("20260911T182146Z"),
    );
    // Change the manifest after the lock was written: the sha256 no longer matches.
    let toml_path = dir.join("rospm.toml");
    let text = fs::read_to_string(&toml_path).unwrap();
    fs::write(
        &toml_path,
        format!("{text}\n# changed after `rospm lock`\n"),
    )
    .unwrap();
    let manifest = toml_path.to_string_lossy().into_owned();
    let out = rospm()
        .args(["--manifest", &manifest, "apply", "--locked", "--dry-run"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("rospm.lock") && stderr.contains("run `rospm lock` again"),
        "{stderr}"
    );
}

#[test]
fn apply_locked_dry_run_plans_the_pinned_snapshot_and_never_touches_the_network() {
    let Some((ubuntu, distro, ros_base)) = host_distro() else {
        eprintln!("skipping: host is neither 22.04 nor 24.04");
        return;
    };
    let dir = fixture_dir("consistent-lock", ubuntu, distro, ros_base);
    write_lock(&dir, ros_base, Some("2026-09-11"), Some("20260911T182146Z"));
    let manifest = dir.join("rospm.toml").to_string_lossy().into_owned();
    let out = rospm()
        .args(["--manifest", &manifest, "apply", "--locked", "--dry-run"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{stderr}");

    // The ROS apt source is pinned to the snapshot, signed by the snapshot
    // key (fetched by fingerprint), not the regular ROS key. The plan reads
    // the real /etc/apt and keyrings: a host that already has a usable
    // snapshot keyring skips the download, so only assert it when it must
    // happen. The core tests (`tests/plan.rs`) cover both cases on fake hosts.
    let keyring = Path::new("/usr/share/keyrings/ros2-snapshots-archive-keyring.gpg");
    let keyring_usable = fs::read(keyring).is_ok_and(|b| !rospm_core::plan::is_ascii_armored(&b));
    if !keyring_usable {
        assert!(
            stdout.contains("4B63CF8FDE49746E98FA01DDAD19BAB3CBF125EA"),
            "{stdout}"
        );
    }
    // Written or already configured, the snapshot URL shows up either way.
    assert!(
        stdout.contains(&format!("snapshots.ros.org/{distro}/2026-09-11/ubuntu")),
        "{stdout}"
    );
    // A separate, rospm-owned Ubuntu snapshot file; Ubuntu's own sources.list
    // is never mentioned.
    assert!(
        stdout.contains("rospm-ubuntu-snapshot.list") && stdout.contains("20260911T182146Z"),
        "{stdout}"
    );
    // The pinned apt set is installed explicitly and exactly.
    assert!(
        stdout.contains(&format!(
            "--allow-downgrades {ros_base}=1.2.3-1noble.20260910.000000"
        )),
        "{stdout}"
    );
    // The git source is pinned to its recorded revision; rosdep (disabled
    // in the fixture manifest, and skipped anyway under --locked) does not run.
    assert!(
        stdout.contains("git checkout --detach cafef00ddeadbeefcafef00ddeadbeefcafef00d"),
        "{stdout}"
    );
    assert!(!stdout.contains("rosdep install"), "{stdout}");
    // `--dry-run` never resolves the network reachability of the snapshot.
    assert!(!stdout.contains("checking whether"), "{stdout}");
}

#[test]
fn apply_locked_reports_a_missing_lock_in_chinese() {
    let dir = fixture_dir("no-lock-zh", "22.04", "humble", "ros-humble-ros-base");
    let manifest = dir.join("rospm.toml").to_string_lossy().into_owned();
    let out = rospm_zh()
        .args(["--manifest", &manifest, "apply", "--locked", "--dry-run"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(has_cjk(&stderr), "{stderr}");
    assert!(
        stderr.contains("rospm.lock") && stderr.contains("rospm lock"),
        "{stderr}"
    );
}
