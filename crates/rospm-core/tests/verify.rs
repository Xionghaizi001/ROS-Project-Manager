//! `rospm verify` — native provider (spec §4, roadmap P2).
//!
//! No renderer is installed here, so `Message::to_string()` yields the
//! developer form `key(arg=value, ...)` (spec §4.2). The CLI tests cover the
//! translated text.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rospm_core::lock::Lock;
use rospm_core::manifest::Manifest;
use rospm_core::mirror;
use rospm_core::platform::Platform;
use rospm_core::probe::{parse_dpkg_query, Host, PackageDb, Status};
use rospm_core::provider::{self, Context};

const MANIFEST: &str = r#"
schema_version = 1

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

fn manifest() -> Manifest {
    Manifest::parse(MANIFEST, Path::new("rospm.toml")).unwrap()
}

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rospm-verify-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// `git init` a one-commit repo at `dir`; returns the commit's full SHA.
fn git_repo(dir: &Path) -> String {
    let git = which::which("git").expect("git required for this test");
    fs::create_dir_all(dir).unwrap();
    let run = |args: &[&str]| {
        let status = Command::new(&git)
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
    let out = Command::new(&git)
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A synthetic host: nothing installed by default, git wired to the real binary.
fn host_with_git() -> Host {
    let mut h = Host::empty();
    let git = which::which("git").expect("git required for this test");
    h.tools.insert("git".to_string(), Some(git));
    h
}

fn installed(pairs: &[(&str, &str)]) -> PackageDb {
    let text: String = pairs
        .iter()
        .map(|(n, v)| format!("{n}\tinstalled\t{v}\n"))
        .collect();
    PackageDb::Static(parse_dpkg_query(&text))
}

fn ctx<'a>(m: &'a Manifest, root: &'a Path, host: &'a Host, lock: &'a Lock) -> Context<'a> {
    Context {
        manifest: m,
        root,
        platform: Platform::LinuxX86_64,
        locked: false,
        lock: Some(lock),
        offline: false,
        host,
        mirror: mirror::OFFICIAL,
    }
}

fn item<'a>(
    report: &'a rospm_core::probe::EnvReport,
    area: &str,
) -> &'a rospm_core::probe::EnvItem {
    report
        .items
        .iter()
        .find(|i| i.area == area)
        .unwrap_or_else(|| panic!("no `{area}` row in {report:#?}"))
}

#[test]
fn matching_source_and_apt_is_ready() {
    let root = tmp("ready");
    let repo = root.join("ws/src/demo_pkg");
    let rev = git_repo(&repo);
    let lock_text = format!(
        r#"
schema_version = 1
generated_at = "2026-01-01T00:00:00Z"
manifest_sha256 = "sha256:deadbeef"

[provider]
kind = "native"

[[sources]]
workspace = "main"
name = "demo_pkg"
type = "git"
url = "https://example.com/demo.git"
rev = "{rev}"

[[apt]]
platform = "linux-x86_64"
name = "ros-jazzy-ros-base"
version = "1.2.3"
"#
    );
    let lock = Lock::parse(&lock_text, Path::new("rospm.lock")).unwrap();
    let m = manifest();
    let mut host = host_with_git();
    host.packages = installed(&[("ros-jazzy-ros-base", "1.2.3")]);
    let report = provider::for_manifest(&m)
        .verify(&ctx(&m, &root, &host, &lock), &lock)
        .unwrap();
    assert!(report.ready(), "{report:#?}");
    assert_eq!(item(&report, "manifest").status, Status::Ok);
    assert_eq!(item(&report, "source").status, Status::Ok);
    assert_eq!(item(&report, "ros").status, Status::Ok);
}

#[test]
fn head_moved_since_lock_is_a_mismatch() {
    let root = tmp("head-moved");
    let repo = root.join("ws/src/demo_pkg");
    git_repo(&repo);
    // The lock names a revision that is not (and never was) checked out.
    let locked_rev = "1234567890abcdef1234567890abcdef12345678";
    let lock_text = format!(
        r#"
schema_version = 1
generated_at = "2026-01-01T00:00:00Z"
manifest_sha256 = "sha256:deadbeef"

[provider]
kind = "native"

[[sources]]
workspace = "main"
name = "demo_pkg"
type = "git"
url = "https://example.com/demo.git"
rev = "{locked_rev}"
"#
    );
    let lock = Lock::parse(&lock_text, Path::new("rospm.lock")).unwrap();
    let m = manifest();
    let host = host_with_git();
    let report = provider::for_manifest(&m)
        .verify(&ctx(&m, &root, &host, &lock), &lock)
        .unwrap();
    let source = item(&report, "source");
    assert_eq!(source.status, Status::Mismatch, "{report:#?}");
    assert!(
        source
            .actual
            .to_string()
            .starts_with("verify.source.rev_mismatch("),
        "{}",
        source.actual
    );
    assert!(!report.ready());
}

#[test]
fn missing_checkout_directory_is_missing() {
    let root = tmp("missing-dir");
    // No `git_repo` call: `ws/src/demo_pkg` never exists.
    let lock_text = r#"
schema_version = 1
generated_at = "2026-01-01T00:00:00Z"
manifest_sha256 = "sha256:deadbeef"

[provider]
kind = "native"

[[sources]]
workspace = "main"
name = "demo_pkg"
type = "git"
url = "https://example.com/demo.git"
rev = "1234567890abcdef1234567890abcdef12345678"
"#;
    let lock = Lock::parse(lock_text, Path::new("rospm.lock")).unwrap();
    let m = manifest();
    let host = host_with_git();
    let report = provider::for_manifest(&m)
        .verify(&ctx(&m, &root, &host, &lock), &lock)
        .unwrap();
    let source = item(&report, "source");
    assert_eq!(source.status, Status::Missing, "{report:#?}");
    assert_eq!(source.actual.to_string(), "probe.source.not_imported");
}

#[test]
fn apt_version_drift_is_a_mismatch() {
    let root = tmp("apt-drift");
    let lock_text = r#"
schema_version = 1
generated_at = "2026-01-01T00:00:00Z"
manifest_sha256 = "sha256:deadbeef"

[provider]
kind = "native"

[[apt]]
platform = "linux-x86_64"
name = "ros-jazzy-rclcpp"
version = "2.0.0"
"#;
    let lock = Lock::parse(lock_text, Path::new("rospm.lock")).unwrap();
    let m = manifest();
    let mut host = Host::empty();
    host.packages = installed(&[("ros-jazzy-rclcpp", "1.9.9")]);
    let report = provider::for_manifest(&m)
        .verify(&ctx(&m, &root, &host, &lock), &lock)
        .unwrap();
    let apt = item(&report, "apt");
    assert_eq!(apt.status, Status::Mismatch, "{report:#?}");
    assert_eq!(apt.expected.to_string(), "2.0.0");
    assert_eq!(apt.actual.to_string(), "1.9.9");
}

#[test]
fn entries_for_another_platform_are_a_single_info_line() {
    let root = tmp("other-platform");
    let lock_text = r#"
schema_version = 1
generated_at = "2026-01-01T00:00:00Z"
manifest_sha256 = "sha256:deadbeef"

[provider]
kind = "native"

[[apt]]
platform = "linux-aarch64"
name = "ros-jazzy-rclcpp"
version = "2.0.0"

[[apt]]
platform = "linux-aarch64"
name = "libfoo-dev"
version = "1.0.0"
"#;
    let lock = Lock::parse(lock_text, Path::new("rospm.lock")).unwrap();
    let m = manifest();
    let host = Host::empty();
    let report = provider::for_manifest(&m)
        .verify(&ctx(&m, &root, &host, &lock), &lock)
        .unwrap();
    // One line summarising the foreign-platform entries (not one per entry),
    // plus one noting that nothing is locked for the host platform itself.
    let apt_rows: Vec<_> = report.items.iter().filter(|i| i.area == "apt").collect();
    assert_eq!(apt_rows.len(), 2, "{report:#?}");
    let other = apt_rows
        .iter()
        .find(|i| i.subject.to_string() == "linux-aarch64")
        .expect("a row for the foreign platform");
    assert_eq!(other.status, Status::Info);
    assert_eq!(
        other.actual.to_string(),
        "verify.apt.other_platform(count=2, platform=linux-aarch64)"
    );
    let none = apt_rows
        .iter()
        .find(|i| i.subject.to_string() == "apt")
        .expect("a row noting nothing is locked for the host platform");
    assert_eq!(none.status, Status::Info);
    assert_eq!(
        none.actual.to_string(),
        "verify.apt.none_for_host(platform=linux-x86_64)"
    );
}

#[test]
fn provider_kind_mismatch_is_a_manifest_mismatch() {
    // Only the CLI has the raw manifest text to hash-compare (Context
    // carries no such field); the provider-kind check is the one part of
    // the "manifest" row this crate can verify on its own.
    let root = tmp("provider-mismatch");
    let lock_text = r#"
schema_version = 1
generated_at = "2026-01-01T00:00:00Z"
manifest_sha256 = "sha256:deadbeef"

[provider]
kind = "pixi"
"#;
    let lock = Lock::parse(lock_text, Path::new("rospm.lock")).unwrap();
    let m = manifest();
    let host = Host::empty();
    let report = provider::for_manifest(&m)
        .verify(&ctx(&m, &root, &host, &lock), &lock)
        .unwrap();
    let manifest_row = item(&report, "manifest");
    assert_eq!(manifest_row.status, Status::Mismatch, "{report:#?}");
    assert_eq!(manifest_row.expected.to_string(), "native");
    assert_eq!(manifest_row.actual.to_string(), "pixi");
}

#[test]
fn dpkg_unavailable_is_unknown_per_package() {
    let root = tmp("dpkg-unavailable");
    let lock_text = r#"
schema_version = 1
generated_at = "2026-01-01T00:00:00Z"
manifest_sha256 = "sha256:deadbeef"

[provider]
kind = "native"

[[apt]]
platform = "linux-x86_64"
name = "ros-jazzy-rclcpp"
version = "2.0.0"
"#;
    let lock = Lock::parse(lock_text, Path::new("rospm.lock")).unwrap();
    let m = manifest();
    let mut host = Host::empty();
    host.packages = PackageDb::Unavailable;
    let report = provider::for_manifest(&m)
        .verify(&ctx(&m, &root, &host, &lock), &lock)
        .unwrap();
    let apt = item(&report, "apt");
    assert_eq!(apt.status, Status::Unknown, "{report:#?}");
    assert_eq!(apt.actual.to_string(), "probe.dpkg_unavailable");
    assert!(!report.ready());
}

#[test]
fn cross_platform_lock_entries_do_not_panic() {
    // A lock with only foreign-platform apt entries and no sources at all
    // must still produce a well-formed report.
    let root = tmp("cross-platform");
    let lock_text = r#"
schema_version = 1
generated_at = "2026-01-01T00:00:00Z"
manifest_sha256 = "sha256:deadbeef"

[provider]
kind = "native"

[[apt]]
platform = "linux-aarch64"
name = "ros-jazzy-rclcpp"
version = "2.0.0"
"#;
    let lock = Lock::parse(lock_text, Path::new("rospm.lock")).unwrap();
    let m = manifest();
    let host = Host::empty();
    let report = provider::for_manifest(&m)
        .verify(&ctx(&m, &root, &host, &lock), &lock)
        .unwrap();
    assert!(report.items.iter().any(|i| i.area == "apt"));
}

#[test]
fn a_lock_without_the_base_package_is_not_ready() {
    // `apply --locked` refuses such a lock (native.apply.locked_base_missing);
    // verify used to call it ready because every locked entry matched.
    let root = tmp("no-base");
    let lock_text = r#"
schema_version = 1
generated_at = "2026-01-01T00:00:00Z"
manifest_sha256 = "sha256:deadbeef"

[provider]
kind = "native"

[[apt]]
platform = "linux-x86_64"
name = "python3-rosdep"
version = "0.26.0-1"
"#;
    let lock = Lock::parse(lock_text, Path::new("rospm.lock")).unwrap();
    let m = manifest();
    let mut host = Host::empty();
    host.packages = installed(&[("python3-rosdep", "0.26.0-1")]);
    let report = provider::for_manifest(&m)
        .verify(&ctx(&m, &root, &host, &lock), &lock)
        .unwrap();
    assert!(!report.ready(), "{report:#?}");
    let base = item(&report, "ros");
    assert_eq!(base.subject.to_string(), "ros-jazzy-ros-base");
    assert_eq!(base.status, Status::Missing);
    assert_eq!(
        base.actual.to_string(),
        "verify.apt.base_not_locked(platform=linux-x86_64)"
    );
}
