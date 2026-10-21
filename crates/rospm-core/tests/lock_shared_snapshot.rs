//! `rospm lock` on one platform must not move the shared ROS snapshot date
//! past the versions another platform locked. Own binary: it narrows `PATH`
//! (no gpg, so canned snapshots count as signed-skipped) for the process.

use std::fs;
use std::path::Path;

use rospm_core::lock::{Lock, LockedApt};
use rospm_core::manifest::Manifest;
use rospm_core::mirror;
use rospm_core::platform::Platform;
use rospm_core::probe::{parse_dpkg_query, Host, PackageDb};
use rospm_core::provider::{self, Context};
use rospm_core::snapshot::{self, Canned};

const RELEASE: &str = "Suite: noble\nCodename: noble\nDate: Fri, 11 Sep 2026 18:21:46 UTC\nArchitectures: amd64 arm64\nComponents: main\n";
const AMD64: &str = "Package: ros-jazzy-rclcpp\nVersion: 28.1.5-1noble\n";

/// Snapshots 2026-09-11 (the previous lock's) and 2026-10-02 (newest) with the
/// given amd64/arm64 indexes; the two dates between them are unusable.
fn world(old_amd64: &str, old_arm64: &str, new_arm64: &str) -> Canned {
    let mut c = Canned::default().with(
        snapshot::ros_snapshot_index_url("jazzy"),
        200,
        ["2026-09-11", "2026-09-18", "2026-09-25", "2026-10-02"]
            .map(|d| format!("<a href=\"{d}/\">{d}/</a>"))
            .join("\n"),
    );
    for (date, amd64, arm64) in [
        ("2026-09-11", old_amd64, old_arm64),
        ("2026-10-02", AMD64, new_arm64),
    ] {
        let base = snapshot::ros_snapshot_url("jazzy", date);
        c = c
            .with(snapshot::release_url(&base, "noble"), 200, RELEASE)
            .with(
                snapshot::packages_url(&base, "noble", "main", "amd64"),
                200,
                amd64,
            )
            .with(
                snapshot::packages_url(&base, "noble", "main", "arm64"),
                200,
                arm64,
            );
    }
    c
}

#[test]
fn relocking_one_platform_keeps_a_date_that_serves_the_others() {
    // Hide gpg: only git (for `vcs`-less source resolution) stays reachable.
    let bin = std::env::temp_dir().join(format!("rospm-shared-snap-bin-{}", std::process::id()));
    fs::create_dir_all(&bin).unwrap();
    let git = which::which("git").unwrap();
    let _ = std::os::unix::fs::symlink(&git, bin.join("git"));
    std::env::set_var("PATH", &bin);

    let root = std::env::temp_dir().join(format!("rospm-shared-snap-{}", std::process::id()));
    fs::create_dir_all(root.join("ws/src")).unwrap();
    fs::write(root.join("main.repos"), "repositories: {}\n").unwrap();
    fs::write(
        root.join("rospm.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\nros_distro = \"jazzy\"\nplatforms = [\"linux-x86_64\", \"linux-aarch64\"]\n[environment]\nprovider = \"native\"\n[workspaces.main]\npath = \"ws\"\nsources = \"main.repos\"\n",
    )
    .unwrap();
    let m = Manifest::load(&root.join("rospm.toml")).unwrap();
    let mut host = Host::empty();
    host.platform = Some(Platform::LinuxX86_64);
    host.tools.insert("git".into(), Some(git));
    host.packages = PackageDb::Static(parse_dpkg_query(
        "ros-jazzy-rclcpp\tinstalled\t28.1.5-1noble\n",
    ));

    // The previous lock: arm64 pinned rclcpp 28.1.4 at the 2026-09-11 snapshot.
    let mut previous = provider::for_manifest(&m)
        .resolve_lock(&ctx(&m, &root, &host, None), None)
        .unwrap()
        .lock;
    previous.provider.native.as_mut().unwrap().ros_snapshot = Some("2026-09-11".into());
    previous.apt.push(LockedApt {
        platform: Platform::LinuxAarch64,
        name: "ros-jazzy-rclcpp".into(),
        version: "28.1.4-1noble".into(),
        sha256: None,
    });
    let resolve = |fetch: &Canned| {
        let res = provider::for_manifest(&m)
            .resolve_lock(&ctx(&m, &root, &host, Some(&previous)), Some(fetch))
            .unwrap();
        let keys: Vec<String> = res.notes.iter().map(|n| n.to_string()).collect();
        (res.lock.provider.native.unwrap().ros_snapshot, keys)
    };
    let arm_old = "Package: ros-jazzy-rclcpp\nVersion: 28.1.4-1noble\n";
    let arm_new = "Package: ros-jazzy-rclcpp\nVersion: 28.1.5-1noble\n";

    // The old date still serves both platforms: kept, with a note.
    let (date, notes) = resolve(&world(AMD64, arm_old, arm_new));
    assert_eq!(date.as_deref(), Some("2026-09-11"), "{notes:?}");
    assert!(
        notes
            .iter()
            .any(|n| n.starts_with("lock.ros_snapshot_kept(date=2026-09-11")),
        "{notes:?}"
    );

    // Nothing serves both: this host's newest date, and the note says arm64 must re-lock.
    let (date, notes) = resolve(&world("", arm_old, arm_new));
    assert_eq!(date.as_deref(), Some("2026-10-02"), "{notes:?}");
    assert!(
        notes.iter().any(|n| n.starts_with(
            "lock.ros_snapshot_strands_other_platforms(date=2026-10-02, platforms=linux-aarch64"
        )),
        "{notes:?}"
    );

    // Newest serves both: it is taken without either note.
    let (date, notes) = resolve(&world(AMD64, arm_old, arm_old));
    assert_eq!(date.as_deref(), Some("2026-10-02"), "{notes:?}");
    assert!(
        !notes
            .iter()
            .any(|n| n.contains("kept") || n.contains("strands")),
        "{notes:?}"
    );

    fs::remove_dir_all(&root).unwrap();
    fs::remove_dir_all(&bin).unwrap();
}

fn ctx<'a>(m: &'a Manifest, root: &'a Path, host: &'a Host, lock: Option<&'a Lock>) -> Context<'a> {
    Context {
        manifest: m,
        root,
        platform: Platform::LinuxX86_64,
        locked: false,
        lock,
        offline: false,
        host,
        mirror: mirror::OFFICIAL,
    }
}
