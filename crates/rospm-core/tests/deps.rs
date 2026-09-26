//! `rospm deps check` / `rospm deps install` (spec §4, roadmap P2).
//!
//! `probe()` shells out to the real `rosdep` binary for the external-key
//! environment rows; this host has `rosdep` installed but no ROS distro
//! registered (no `rosdep update` cache), so those rows may legitimately come
//! back `ok`, `MISSING` or `unknown` depending on the host. The classification
//! into internal/external/skipped is pure manifest + `package.xml` logic and
//! is asserted exactly; `plan_install` never runs anything, so its rendering
//! is asserted exactly too.

use std::fs;
use std::path::Path;

use rospm_core::manifest::Manifest;
use rospm_core::mirror;
use rospm_core::platform::Platform;
use rospm_core::probe::{parse_dpkg_query, Host, PackageDb};
use rospm_core::provider::{self, Context};

const FULL: &str = include_str!("fixtures/full.toml");

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

fn installed(names: &[&str]) -> PackageDb {
    let text: String = names
        .iter()
        .map(|n| format!("{n}\tinstalled\t1.0\n"))
        .collect();
    PackageDb::Static(parse_dpkg_query(&text))
}

/// `FULL`'s two workspaces (`deps` → `deps_ws`, `main` → `ws`, underlay
/// `deps`), with package `a` (underlay, depends on `rclcpp`) and package `b`
/// (overlay, depends on `a` and `libopencv-dev`, the latter in
/// `dependencies.rosdep.skip_keys`).
fn project() -> tempdir::TempProject {
    // Unique per call: the tests in this binary run in parallel.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "rospm-core-deps-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("deps_ws/src/a")).unwrap();
    fs::write(
        root.join("deps_ws/src/a/package.xml"),
        "<package format=\"3\"><name>a</name><depend>rclcpp</depend></package>",
    )
    .unwrap();
    fs::create_dir_all(root.join("ws/src/b")).unwrap();
    fs::write(
        root.join("ws/src/b/package.xml"),
        "<package format=\"3\"><name>b</name><depend>a</depend><depend>libopencv-dev</depend></package>",
    )
    .unwrap();
    tempdir::TempProject { root }
}

// Minimal RAII wrapper so the temp directory is cleaned up even on panic;
// `rospm-core` has no test-only temp-dir helper of its own to reuse.
mod tempdir {
    use std::path::PathBuf;

    pub struct TempProject {
        pub root: PathBuf,
    }

    impl Drop for TempProject {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

#[test]
fn deps_check_classifies_internal_external_and_skipped_keys() {
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let proj = project();
    let mut host = Host::empty();
    host.packages = installed(&["ros-jazzy-rclcpp"]);
    let report = provider::for_manifest(&m)
        .probe_deps(&ctx(&m, &proj.root, &host))
        .unwrap();

    assert_eq!(
        report.internal,
        ["a".to_string()].into_iter().collect(),
        "{:?}",
        report.internal
    );
    assert_eq!(
        report.external,
        ["rclcpp".to_string()].into_iter().collect(),
        "{:?}",
        report.external
    );
    assert_eq!(
        report.skipped,
        ["libopencv-dev".to_string()].into_iter().collect(),
        "{:?}",
        report.skipped
    );

    let deps_ws = report
        .workspaces
        .iter()
        .find(|w| w.name == "deps")
        .expect("deps workspace summarised");
    assert_eq!(deps_ws.packages, 1);
    assert_eq!(deps_ws.keys, 1);
    let main_ws = report
        .workspaces
        .iter()
        .find(|w| w.name == "main")
        .expect("main workspace summarised");
    assert_eq!(main_ws.packages, 1);
    assert_eq!(main_ws.keys, 2);

    // Whatever this host's rosdep can resolve, `deps check` must never crash
    // and only ever report through the `deps`/`tool` areas -- `deps check`'s
    // own exit code is always 0 regardless of the outcome.
    for item in &report.environment.items {
        assert!(
            item.area == "deps" || item.area == "tool",
            "unexpected area `{}`",
            item.area
        );
    }
}

#[test]
fn deps_check_reports_rosdep_disabled_without_shelling_out() {
    let text = FULL.replace("enabled = true", "enabled = false");
    let m = Manifest::parse(&text, Path::new("t")).unwrap();
    let proj = project();
    let host = Host::empty();
    let report = provider::for_manifest(&m)
        .probe_deps(&ctx(&m, &proj.root, &host))
        .unwrap();

    // Classification is unaffected by `rosdep.enabled`.
    assert!(report.external.contains("rclcpp"));
    assert_eq!(report.environment.items.len(), 1);
    assert_eq!(
        report.environment.items[0].actual.to_string(),
        "deps.report.rosdep_disabled"
    );
}

#[test]
fn deps_install_mirrors_apply_and_orders_by_overlay() {
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let mut host = Host::empty();
    host.rosdep.initialized = true;
    host.rosdep.updated = false;
    let plan = provider::for_manifest(&m)
        .plan_deps_install(&ctx(&m, Path::new("/proj"), &host))
        .unwrap();
    let rendered = plan.render();

    let update = rendered
        .find("rosdep update --rosdistro jazzy")
        .expect("rosdep update present when not yet updated");
    let deps_install = rendered
        .find("cd /proj/deps_ws && rosdep install")
        .expect("deps workspace installed");
    let main_install = rendered
        .find("cd /proj/ws && rosdep install")
        .expect("main workspace installed");
    assert!(update < deps_install, "{rendered}");
    assert!(
        deps_install < main_install,
        "underlay before overlay:\n{rendered}"
    );
    assert!(rendered.contains("--skip-keys libopencv-dev"), "{rendered}");
    assert!(rendered.contains("--ignore-src -y --rosdistro jazzy"));
}

#[test]
fn deps_install_skips_rosdep_update_when_already_updated() {
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let mut host = Host::empty();
    host.rosdep.initialized = true;
    host.rosdep.updated = true;
    let rendered = provider::for_manifest(&m)
        .plan_deps_install(&ctx(&m, Path::new("/proj"), &host))
        .unwrap()
        .render();
    assert!(!rendered.contains("rosdep update"), "{rendered}");
    assert!(rendered.contains("rosdep install"), "{rendered}");
}

#[test]
fn deps_install_notes_missing_rosdep_init_instead_of_running_it() {
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let mut host = Host::empty();
    host.rosdep.initialized = false;
    let rendered = provider::for_manifest(&m)
        .plan_deps_install(&ctx(&m, Path::new("/proj"), &host))
        .unwrap()
        .render();
    assert!(!rendered.contains("rosdep init"), "{rendered}");
    assert!(
        rendered.contains("deps.plan.rosdep_not_initialized"),
        "{rendered}"
    );
    // Still plans the installs; the note is advisory, not a gate.
    assert!(rendered.contains("rosdep install"), "{rendered}");
}

#[test]
fn deps_install_is_a_no_op_note_when_rosdep_is_disabled() {
    let text = FULL.replace("enabled = true", "enabled = false");
    let m = Manifest::parse(&text, Path::new("t")).unwrap();
    let host = Host::empty();
    let plan = provider::for_manifest(&m)
        .plan_deps_install(&ctx(&m, Path::new("/proj"), &host))
        .unwrap();
    assert_eq!(plan.action_count(), 0, "{}", plan.render());
    assert!(plan.render().contains("deps.plan.rosdep_disabled"));
}
