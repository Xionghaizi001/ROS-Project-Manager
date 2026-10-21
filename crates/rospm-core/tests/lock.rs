//! `rospm lock` (native): resolution from a synthetic host, a real local git
//! checkout and canned snapshot servers. Nothing here touches the network or
//! the system.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rospm_core::lock::Lock;
use rospm_core::manifest::Manifest;
use rospm_core::mirror;
use rospm_core::platform::Platform;
use rospm_core::probe::{parse_dpkg_query, Host, PackageDb};
use rospm_core::provider::{self, Context};
use rospm_core::snapshot::{self, Canned};

fn git(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A project with one workspace whose `.repos` names one local repository,
/// imported (cloned) into `ws/src/demo`. Returns (root, upstream HEAD).
fn project(name: &str, import: bool) -> (PathBuf, String) {
    let root = std::env::temp_dir().join(format!("rospm-lock-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let upstream = root.join("upstream");
    fs::create_dir_all(&upstream).unwrap();
    git(&upstream, &["init", "-q", "-b", "main"]);
    fs::write(
        upstream.join("package.xml"),
        "<package format=\"3\"><name>demo</name></package>",
    )
    .unwrap();
    git(&upstream, &["add", "-A"]);
    git(&upstream, &["commit", "-qm", "init"]);
    let head = git(&upstream, &["rev-parse", "HEAD"]);
    fs::write(
        root.join("main.repos"),
        format!(
            "repositories:\n  demo:\n    type: git\n    url: file://{}\n    version: main\n",
            upstream.display()
        ),
    )
    .unwrap();
    fs::create_dir_all(root.join("ws/src")).unwrap();
    if import {
        git(
            &root.join("ws/src"),
            &["clone", "-q", upstream.to_str().unwrap(), "demo"],
        );
    }
    fs::write(
        root.join("rospm.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\nros_distro = \"jazzy\"\nplatforms = [\"linux-x86_64\", \"linux-aarch64\"]\n[environment]\nprovider = \"native\"\n[workspaces.main]\npath = \"ws\"\nsources = \"main.repos\"\n[dependencies]\napt = [\"curl\"]\npip = [\"numpy\", \"PyYAML\"]\n",
    )
    .unwrap();
    (root, head)
}

fn synthetic_host() -> Host {
    let mut h = Host::empty();
    for t in ["git", "vcs"] {
        h.tools.insert(t.to_string(), which::which(t).ok());
    }
    h.packages = PackageDb::Static(parse_dpkg_query(
        "ros-jazzy-rclcpp\tinstalled\t28.1.5-1noble\nros-jazzy-ros-base\tinstalled\t0.11.0-1noble\nros-jazzy-old\tconfig-files\t1\ncurl\tinstalled\t8.5.0-2ubuntu10\npython3-rosdep\tinstalled\t0.27.0-1\n",
    ));
    h
}

fn ctx<'a>(m: &'a Manifest, root: &'a Path, host: &'a Host) -> Context<'a> {
    Context {
        manifest: m,
        root,
        platform: Platform::LinuxX86_64,
        locked: false,
        lock: None,
        offline: false,
        host,
        mirror: mirror::OFFICIAL,
    }
}

fn release(codename: &str) -> String {
    format!("Suite: {codename}\nCodename: {codename}\nDate: Fri, 11 Sep 2026 18:21:46 UTC\nArchitectures: amd64 arm64\nComponents: main\n")
}

#[test]
fn arm_lock_records_the_resolved_package_version_and_replacement_notice() {
    let (root, _) = project("arm-replacements", true);
    let text = fs::read_to_string(root.join("rospm.toml"))
        .unwrap()
        .replace("apt = [\"curl\"]", "apt = [\"gcc-x86-64-linux-gnu\"]");
    fs::write(root.join("rospm.toml"), text).unwrap();
    let m = Manifest::load(&root.join("rospm.toml")).unwrap();
    let mut host = synthetic_host();
    host.platform = Some(Platform::LinuxAarch64);
    host.packages = PackageDb::Static(parse_dpkg_query(
        "ros-jazzy-ros-base\tinstalled\t0.11.0-1noble\ngcc-aarch64-linux-gnu\tinstalled\t4:13.2.0-7ubuntu1\n",
    ));
    let mut c = ctx(&m, &root, &host);
    c.platform = Platform::LinuxAarch64;
    let res = provider::for_manifest(&m).resolve_lock(&c, None).unwrap();
    let gcc = res
        .lock
        .apt
        .iter()
        .find(|a| a.name == "gcc-aarch64-linux-gnu")
        .unwrap();
    assert_eq!(gcc.platform, Platform::LinuxAarch64);
    assert_eq!(gcc.version, "4:13.2.0-7ubuntu1");
    assert!(!res
        .lock
        .apt
        .iter()
        .any(|a| a.name == "gcc-x86-64-linux-gnu"));
    assert!(res
        .notes
        .iter()
        .any(|n| n.key_name() == Some("replacement.builtin")));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn lock_records_sources_apt_and_degrades_without_network() {
    let (root, head) = project("full", true);
    let m = Manifest::load(&root.join("rospm.toml")).unwrap();
    let host = synthetic_host();
    let res = provider::for_manifest(&m)
        .resolve_lock(&ctx(&m, &root, &host), None)
        .unwrap();
    let l = &res.lock;
    assert_eq!(l.sources.len(), 1);
    assert_eq!(l.sources[0].rev, head);
    assert_eq!(l.sources[0].name, "demo");
    assert_eq!(l.sources[0].version.as_deref(), Some("main"));
    let names: Vec<&str> = l.apt.iter().map(|a| a.name.as_str()).collect();
    // installed ros-jazzy-* + base + extras + tooling that is installed; not the config-files one
    assert_eq!(
        names,
        vec![
            "curl",
            "python3-rosdep",
            "ros-jazzy-rclcpp",
            "ros-jazzy-ros-base"
        ]
    );
    assert!(l.apt.iter().all(|a| a.platform == Platform::LinuxX86_64));
    let native = l.provider.native.as_ref().unwrap();
    assert_eq!(native.ubuntu, "24.04");
    assert_eq!(native.ros_snapshot, None);
    assert_eq!(native.ubuntu_snapshot, None);
    let keys: Vec<_> = res.notes.iter().filter_map(|n| n.key_name()).collect();
    assert!(keys.contains(&"lock.network_skipped"), "{keys:?}");
    assert!(keys.contains(&"lock.os_release_unreadable"), "{keys:?}");
    assert!(keys.contains(&"lock.apt_missing"), "{keys:?}"); // tooling not installed
    assert!(keys.contains(&"lock.pip_unavailable"), "{keys:?}"); // no python3 tool in the synthetic host
    assert!(l.pip.is_empty());
    // TOML round trip is lossless and deterministic.
    let text = l.to_toml().unwrap();
    let back = Lock::parse(&text, Path::new("rospm.lock")).unwrap();
    assert_eq!(&back, l);
    assert_eq!(back.to_toml().unwrap(), text);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn lock_refuses_unimported_sources_and_foreign_platforms() {
    let (root, _) = project("missing", false);
    let m = Manifest::load(&root.join("rospm.toml")).unwrap();
    let host = synthetic_host();
    let err = provider::for_manifest(&m)
        .resolve_lock(&ctx(&m, &root, &host), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("lock.sources_not_imported"), "{err}");
    assert!(err.contains("main: src/demo"), "{err}");

    let mut c = ctx(&m, &root, &host);
    c.platform = Platform::LinuxAarch64;
    let err = provider::for_manifest(&m)
        .resolve_lock(&c, None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("lock.foreign_platform"), "{err}");

    let mut h = synthetic_host();
    h.os = Some(rospm_core::probe::OsRelease::parse(
        "ID=ubuntu\nVERSION_ID=\"22.04\"\nPRETTY_NAME=\"Ubuntu 22.04.5 LTS\"\n",
    ));
    let err = provider::for_manifest(&m)
        .resolve_lock(&ctx(&m, &root, &h), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("native.apply.wrong_ubuntu"), "{err}");
    h.os = None;
    h.packages = PackageDb::Unavailable;
    let (root2, _) = project("nodpkg", true);
    let err = provider::for_manifest(&m)
        .resolve_lock(&ctx(&m, &root2, &h), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("lock.dpkg_unavailable"), "{err}");
    fs::remove_dir_all(&root).unwrap();
    fs::remove_dir_all(&root2).unwrap();
}

#[test]
fn lock_discovers_snapshots_from_canned_servers() {
    let (root, _) = project("snap", true);
    let m = Manifest::load(&root.join("rospm.toml")).unwrap();
    let host = synthetic_host();
    let ros_base = snapshot::ros_snapshot_url("jazzy", "2026-09-11");
    let mut fetch = Canned::default()
        .with(
            snapshot::ros_snapshot_index_url("jazzy"),
            200,
            "<a href=\"2026-09-11/\">2026-09-11/</a>",
        )
        .with(
            snapshot::release_url(&ros_base, "noble"),
            200,
            release("noble"),
        )
        .with(
            snapshot::inrelease_url(&snapshot::ubuntu_snapshot_url("20260911T182146Z"), "noble"),
            200,
            release("noble"),
        );
    let res = provider::for_manifest(&m)
        .resolve_lock(&ctx(&m, &root, &host), Some(&fetch))
        .unwrap();
    let native = res.lock.provider.native.as_ref().unwrap();
    let keys: Vec<_> = res.notes.iter().filter_map(|n| n.key_name()).collect();
    if which::which("gpg").is_ok() && which::which("gpgv").is_ok() {
        // The canned world has no Release.gpg: an unverifiable snapshot is
        // never recorded, and the failure is a DEGRADED note.
        assert_eq!(native.ros_snapshot, None, "{keys:?}");
        assert!(keys.contains(&"lock.ros_snapshot_failed"), "{keys:?}");
    } else {
        assert_eq!(native.ros_snapshot.as_deref(), Some("2026-09-11"));
        assert_eq!(native.ubuntu_snapshot.as_deref(), Some("20260911T182146Z"));
    }
    // Index down: degraded, not an error.
    fetch.0.clear();
    let res = provider::for_manifest(&m)
        .resolve_lock(&ctx(&m, &root, &host), Some(&fetch))
        .unwrap();
    assert_eq!(
        res.lock.provider.native.as_ref().unwrap().ros_snapshot,
        None
    );
    assert!(res
        .notes
        .iter()
        .any(|n| n.key_name() == Some("lock.ros_snapshot_failed")));
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn previous_lock_keeps_other_platforms_and_same_content_is_stable() {
    let (root, head) = project("merge", true);
    let m = Manifest::load(&root.join("rospm.toml")).unwrap();
    let host = synthetic_host();
    let mut res = provider::for_manifest(&m)
        .resolve_lock(&ctx(&m, &root, &host), None)
        .unwrap();
    let mut previous = res.lock.clone();
    previous.generated_at = "2020-01-01T00:00:00Z".into();
    previous.apt.push(rospm_core::lock::LockedApt {
        platform: Platform::LinuxAarch64,
        name: "ros-jazzy-rclcpp".into(),
        version: "28.1.5-1noble".into(),
        sha256: None,
    });
    res.lock
        .merge_other_platforms(&previous, Platform::LinuxX86_64);
    assert!(res.lock.same_content(&previous));
    assert_eq!(res.lock.apt_for(Platform::LinuxAarch64).len(), 1);
    assert_eq!(res.lock.sources[0].rev, head);
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn lock_only_records_a_ros_snapshot_that_carries_the_installed_versions() {
    let (root, _) = project("snap-versions", true);
    let m = Manifest::load(&root.join("rospm.toml")).unwrap();
    let host = synthetic_host();
    let ros_base = snapshot::ros_snapshot_url("jazzy", "2026-09-11");
    let index = snapshot::packages_url(&ros_base, "noble", "main", "amd64");
    let world = |packages: &str| {
        Canned::default()
            .with(
                snapshot::ros_snapshot_index_url("jazzy"),
                200,
                "<a href=\"2026-09-11/\">2026-09-11/</a>",
            )
            .with(
                snapshot::release_url(&ros_base, "noble"),
                200,
                release("noble"),
            )
            .with(index.clone(), 200, packages)
    };
    let resolve = |fetch: &Canned| {
        let res = provider::for_manifest(&m)
            .resolve_lock(&ctx(&m, &root, &host), Some(fetch))
            .unwrap();
        let note = res
            .notes
            .iter()
            .map(|n| n.to_string())
            .find(|n| n.starts_with("lock.ros_snapshot_"))
            .unwrap();
        (res.lock.provider.native.unwrap().ros_snapshot, note)
    };

    // The snapshot predates the installed rclcpp (the 2026-09-26 finding):
    // not recorded, and the note says which versions are missing.
    let (date, note) = resolve(&world(
        "Package: ros-jazzy-rclcpp\nVersion: 28.1.4-1noble\n\nPackage: ros-jazzy-ros-base\nVersion: 0.11.0-1noble\n",
    ));
    assert_eq!(date, None);
    assert!(
        note.contains("lock.snapshot_lacks_versions(count=1"),
        "{note}"
    );
    assert!(note.contains("ros-jazzy-rclcpp=28.1.5-1noble"), "{note}");

    // It carries both: the version check passes, and what decides next is
    // the signature (absent from the canned world, so it fails where gpg can
    // check it).
    let (date, note) = resolve(&world(
        "Package: ros-jazzy-rclcpp\nVersion: 28.1.5-1noble\n\nPackage: ros-jazzy-ros-base\nVersion: 0.11.0-1noble\n",
    ));
    assert!(!note.contains("snapshot_lacks_versions"), "{note}");
    if which::which("gpg").is_ok() && which::which("gpgv").is_ok() {
        assert_eq!(date, None, "{note}");
    } else {
        assert_eq!(date.as_deref(), Some("2026-09-11"), "{note}");
    }
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn lock_takes_the_ubuntu_snapshot_at_the_last_dpkg_change() {
    let (root, _) = project("snap-ubuntu-ts", true);
    let m = Manifest::load(&root.join("rospm.toml")).unwrap();
    let mut host = synthetic_host();
    host.dpkg_changed_at = Some(1_789_150_906); // 2026-09-11T18:21:46Z
                                                // No ROS snapshot at all: the Ubuntu one no longer depends on it.
    let ubuntu = snapshot::ubuntu_snapshot_url("20260911T182146Z");
    let fetch = Canned::default().with(
        snapshot::inrelease_url(&ubuntu, "noble"),
        200,
        release("noble"),
    );
    let res = provider::for_manifest(&m)
        .resolve_lock(&ctx(&m, &root, &host), Some(&fetch))
        .unwrap();
    let notes: Vec<String> = res.notes.iter().map(|n| n.to_string()).collect();
    // The request went to the dpkg timestamp: either recorded (no keyring
    // to check the canned InRelease against) or rejected by the signature,
    // never "not found".
    let ubuntu_note = notes
        .iter()
        .find(|n| n.starts_with("lock.ubuntu_snapshot_"))
        .unwrap();
    assert!(
        !ubuntu_note.contains("HTTP 404") && !ubuntu_note.contains("http_status"),
        "{notes:?}"
    );
    assert!(
        !ubuntu_note.contains("ubuntu_timestamp_unknown"),
        "{notes:?}"
    );
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn lock_refuses_names_versions_and_revs_that_look_like_options() {
    let text = r#"
schema_version = 1
generated_at = "2026-01-01T00:00:00Z"
manifest_sha256 = "sha256:deadbeef"

[provider]
kind = "native"

[[sources]]
workspace = "main"
name = "demo"
type = "git"
url = "https://example.com/demo.git"
rev = "--upload-pack=touch /tmp/pwn"

[[apt]]
platform = "linux-x86_64"
name = "-oDPkg::Pre-Invoke::=touch /tmp/pwn"
version = "1.0"

[[apt]]
platform = "linux-x86_64"
name = "curl"
version = "1.0 -oAPT::x=y"
"#;
    let err = Lock::parse(text, Path::new("rospm.lock")).unwrap_err();
    let rospm_core::Error::Validation(list) = err else {
        panic!("{err:?}")
    };
    let keys: Vec<_> = list.iter().filter_map(|m| m.key_name()).collect();
    assert_eq!(
        keys,
        [
            "lock.apt_name_invalid",
            "lock.apt_version_invalid",
            "lock.rev_invalid"
        ]
    );
}

#[test]
fn relock_keeps_the_recorded_deb_digest_of_an_unchanged_version() {
    // The synthetic host has no apt-cache index, like a repository that has
    // dropped the installed version: the digest can only come from the old lock.
    let (root, _) = project("keep-sha", true);
    let m = Manifest::load(&root.join("rospm.toml")).unwrap();
    let host = synthetic_host();
    let mut previous = provider::for_manifest(&m)
        .resolve_lock(&ctx(&m, &root, &host), None)
        .unwrap()
        .lock;
    for a in previous.apt.iter_mut() {
        a.sha256 = Some(format!("sha256:{}", a.name));
    }
    previous
        .apt
        .iter_mut()
        .find(|a| a.name == "curl")
        .unwrap()
        .version = "8.4.0-1".into();
    let mut c = ctx(&m, &root, &host);
    c.lock = Some(&previous);
    let res = provider::for_manifest(&m).resolve_lock(&c, None).unwrap();
    let sha = |name: &str| {
        res.lock
            .apt
            .iter()
            .find(|a| a.name == name)
            .unwrap()
            .sha256
            .clone()
    };
    assert_eq!(
        sha("ros-jazzy-rclcpp").as_deref(),
        Some("sha256:ros-jazzy-rclcpp")
    );
    assert_eq!(
        sha("curl"),
        None,
        "a different version never inherits a digest"
    );
    fs::remove_dir_all(&root).unwrap();
}
