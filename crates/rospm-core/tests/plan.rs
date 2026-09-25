use std::path::{Path, PathBuf};

use rospm_core::manifest::Manifest;
use rospm_core::plan::{shell_quote, Command, Step};
use rospm_core::platform::Platform;
use rospm_core::probe::{
    parse_dpkg_query, AptSources, Host, OsRelease, PackageDb, ROS_APT_KEYRING, ROS_APT_LIST,
};
use rospm_core::provider::{self, Context, Shell};

const FULL: &str = include_str!("fixtures/full.toml");

// Plans render their notes and titles as message keys here: no renderer is
// installed in the core tests, so `Message` shows its developer form
// `key(arg=value)` (spec §4.2). The CLI tests cover the translated text.

/// A bare host: nothing installed, locale POSIX, no ROS apt source.
fn bare_host() -> Host {
    Host::empty()
}

/// A host the official install guide has already been followed on.
fn ready_host() -> Host {
    let mut h = Host::empty();
    h.os = Some(OsRelease::parse(
        "ID=ubuntu\nVERSION_ID=\"24.04\"\nPRETTY_NAME=\"Ubuntu 24.04.2 LTS\"\n",
    ));
    h.locale_utf8 = true;
    h.apt_sources = AptSources {
        ros2: Some(PathBuf::from(ROS_APT_LIST)),
        keyring: true,
        universe: Some(true),
    };
    h.rosdep.initialized = true;
    h
}

fn ctx<'a>(m: &'a Manifest, host: &'a Host) -> Context<'a> {
    Context {
        manifest: m,
        root: Path::new("/proj"),
        platform: Platform::LinuxX86_64,
        locked: false,
        offline: false,
        host,
    }
}

#[test]
fn shell_quote_escapes_when_needed() {
    assert_eq!(shell_quote("plain-arg_1.0"), "plain-arg_1.0");
    assert_eq!(shell_quote("has space"), "'has space'");
    assert_eq!(shell_quote("it's"), r"'it'\''s'");
    assert_eq!(shell_quote(""), "''");
}

#[test]
fn command_renders_like_a_shell_line() {
    let c = Command::new("colcon")
        .args(["build", "--symlink-install"])
        .cwd("/proj/ws")
        .env("COLCON_DEFAULTS_FILE", "/proj/d.yaml")
        .preamble("source /opt/ros/jazzy/setup.bash");
    assert_eq!(
        c.render(),
        "(cd /proj/ws && source /opt/ros/jazzy/setup.bash && COLCON_DEFAULTS_FILE=/proj/d.yaml colcon build --symlink-install)"
    );
    assert_eq!(
        Command::new("apt-get").arg("update").sudo().render(),
        "sudo apt-get update"
    );
}

#[test]
fn native_apply_plan_follows_overlay_order_and_is_dry_run_safe() {
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let host = bare_host();
    let plan = provider::for_manifest(&m)
        .plan_apply(&ctx(&m, &host))
        .unwrap();
    let rendered = plan.render();

    let deps = rendered
        .find("deps_ws/src")
        .expect("deps workspace imported");
    let main = rendered
        .find("/proj/ws/src")
        .expect("main workspace imported");
    assert!(deps < main, "underlay must be imported first:\n{rendered}");
    assert!(rendered.contains("sudo apt-get install -y ros-jazzy-ros-base"));
    assert!(rendered.contains("--skip-keys libopencv-dev"));
    assert!(rendered.contains("native.apply.pip_recorded_only(packages=numpy>=1.26)"));

    let mut sink = Vec::new();
    plan.execute(true, &mut sink).unwrap();
    assert!(String::from_utf8(sink).unwrap().contains("# plan.dry_run"));
}

#[test]
fn native_apply_bootstraps_the_ros_apt_source_on_a_bare_host() {
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let host = bare_host();
    let plan = provider::for_manifest(&m)
        .plan_apply(&ctx(&m, &host))
        .unwrap();
    let rendered = plan.render();

    // Official install guide, in order: locale → universe → key → source → update → base.
    let pos = |needle: &str| {
        rendered
            .find(needle)
            .unwrap_or_else(|| panic!("missing `{needle}` in:\n{rendered}"))
    };
    let prereqs =
        pos("sudo apt-get install -y locales software-properties-common curl ca-certificates");
    let locale = pos("sudo locale-gen en_US en_US.UTF-8");
    let universe = pos("sudo add-apt-repository -y universe");
    let key = pos(&format!("-o {ROS_APT_KEYRING}"));
    let list = pos(&format!("plan.write_file_sudo(path={ROS_APT_LIST}, bytes="));
    let entry = pos(&format!(
        "| deb [arch=amd64 signed-by={ROS_APT_KEYRING}] http://packages.ros.org/ros2/ubuntu noble main"
    ));
    let base = pos("sudo apt-get install -y ros-jazzy-ros-base");
    assert!(prereqs < locale && locale < universe && universe < key && key < list);
    assert!(list < entry && entry < base, "{rendered}");
    assert!(rendered.contains("sudo rosdep init"), "{rendered}");
    let first_action = plan
        .steps
        .iter()
        .find_map(|s| match s {
            Step::Run(c) => Some(c.render()),
            _ => None,
        })
        .unwrap();
    assert_eq!(first_action, "sudo apt-get update");
}

#[test]
fn native_apply_skips_bootstrap_when_the_host_is_ready() {
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let host = ready_host();
    let plan = provider::for_manifest(&m)
        .plan_apply(&ctx(&m, &host))
        .unwrap();
    let rendered = plan.render();
    for absent in [
        "locale-gen",
        "add-apt-repository",
        "curl",
        "plan.write_file_sudo(path=/etc/apt",
        "rosdep init",
    ] {
        assert!(!rendered.contains(absent), "`{absent}` in:\n{rendered}");
    }
    assert!(rendered.contains(
        "native.apply.source_already_configured(file=/etc/apt/sources.list.d/ros2.list)"
    ));
    assert!(rendered.contains("sudo apt-get install -y ros-jazzy-ros-base"));
}

#[test]
fn native_apply_refuses_the_wrong_ubuntu() {
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let mut host = ready_host();
    host.os = Some(OsRelease::parse(
        "ID=ubuntu\nVERSION_ID=\"22.04\"\nPRETTY_NAME=\"Ubuntu 22.04.5 LTS\"\n",
    ));
    let err = provider::for_manifest(&m)
        .plan_apply(&ctx(&m, &host))
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg == "native.apply.wrong_ubuntu(ubuntu=24.04, distro=jazzy, have=Ubuntu 22.04.5 LTS)",
        "{msg}"
    );

    host.os = Some(OsRelease::parse(
        "ID=debian\nVERSION_ID=\"12\"\nPRETTY_NAME=\"Debian 12\"\n",
    ));
    let err = provider::for_manifest(&m)
        .plan_apply(&ctx(&m, &host))
        .unwrap_err();
    assert!(err.to_string().contains("Debian 12"), "{err}");
}

fn installed(names: &[&str]) -> PackageDb {
    let text: String = names
        .iter()
        .map(|n| format!("{n}\tinstalled\t1.0\n"))
        .collect();
    PackageDb::Static(parse_dpkg_query(&text))
}

#[test]
fn native_uninstall_names_every_package_and_removes_the_source() {
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let mut host = ready_host();
    host.ros_distros = vec!["jazzy".into()];
    host.packages = installed(&[
        "ros-jazzy-ros-base",
        "ros-jazzy-rclcpp",
        "ros-humble-rclcpp",
        "python3-rosdep",
    ]);
    let plan = provider::for_manifest(&m)
        .plan_uninstall(&ctx(&m, &host), false)
        .unwrap();
    let rendered = plan.render();
    assert!(
        rendered.contains("native.uninstall.removing(count=2, prefix=ros-jazzy-)"),
        "{rendered}"
    );
    assert!(
        rendered.contains("sudo apt-get remove -y ros-jazzy-rclcpp ros-jazzy-ros-base"),
        "{rendered}"
    );
    assert!(!rendered.contains("ros-humble-rclcpp"), "{rendered}");
    assert!(!rendered.contains("remove -y python3"), "{rendered}");
    assert!(rendered.contains("sudo apt-get autoremove -y"));
    assert!(
        rendered.contains(&format!("sudo rm -f {ROS_APT_LIST} {ROS_APT_KEYRING}")),
        "{rendered}"
    );
    assert!(
        rendered.contains("native.uninstall.files_may_remain(dir=/opt/ros/jazzy)"),
        "{rendered}"
    );

    // --keep-source, or another distro on the machine, leaves the source alone.
    let plan = provider::for_manifest(&m)
        .plan_uninstall(&ctx(&m, &host), true)
        .unwrap();
    assert!(!plan.render().contains("rm -f"));
    host.ros_distros = vec!["humble".into(), "jazzy".into()];
    let plan = provider::for_manifest(&m)
        .plan_uninstall(&ctx(&m, &host), false)
        .unwrap();
    let rendered = plan.render();
    assert!(!rendered.contains("rm -f"), "{rendered}");
    assert!(
        rendered.contains(
            "native.uninstall.source_kept_other_distros(file=/etc/apt/sources.list.d/ros2.list, others=humble)"
        ),
        "{rendered}"
    );
}

#[test]
fn native_uninstall_with_nothing_installed_is_a_no_op_and_needs_dpkg() {
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let mut host = ready_host();
    host.packages = installed(&["python3-rosdep"]);
    host.apt_sources.ros2 = None;
    let plan = provider::for_manifest(&m)
        .plan_uninstall(&ctx(&m, &host), false)
        .unwrap();
    assert_eq!(plan.action_count(), 0, "{}", plan.render());

    host.packages = PackageDb::Unavailable;
    let err = provider::for_manifest(&m)
        .plan_uninstall(&ctx(&m, &host), false)
        .unwrap_err();
    assert_eq!(err.to_string(), "native.uninstall.dpkg_unavailable");
}

#[test]
fn native_build_plan_sources_underlays_first() {
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let host = bare_host();
    let plan = provider::for_manifest(&m)
        .plan_build(&ctx(&m, &host), Some("main"))
        .unwrap();
    assert_eq!(plan.action_count(), 1);
    match &plan.steps[0] {
        Step::Run(c) => {
            assert_eq!(c.program, "colcon");
            assert_eq!(c.args[..4], ["build", "--mixin", "release", "ccache"]);
            assert!(c.preamble[0].contains("/opt/ros/jazzy/setup.bash"));
            assert!(c.preamble[1].contains("deps_ws/install/setup.bash"));
        }
        other => panic!("unexpected step {other:?}"),
    }
}

#[test]
fn env_script_lists_workspaces_in_order_and_exports_env() {
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let host = bare_host();
    let script = provider::for_manifest(&m)
        .env_script(&ctx(&m, &host), Shell::Zsh)
        .unwrap();
    let base = script.find("/opt/ros/jazzy/setup.zsh").unwrap();
    let deps = script.find("deps_ws/install/setup.zsh").unwrap();
    let main = script.find("/proj/ws/install/setup.zsh").unwrap();
    assert!(base < deps && deps < main, "{script}");
    assert!(script.contains("export ROS_DOMAIN_ID=42"));
    assert!(script.contains("export RMW_IMPLEMENTATION=rmw_cyclonedds_cpp"));
}

#[test]
fn pixi_and_docker_providers_report_their_phase() {
    let text = FULL
        .replace("provider = \"native\"", "provider = \"pixi\"")
        .replace("[environment.native]\nubuntu = \"24.04\"\nros_base = \"ros-jazzy-ros-base\"            # 底包，也可为 ros-jazzy-desktop\n", "")
        .replace("# [environment.pixi]\n# manifest", "[environment.pixi]\nmanifest");
    let m = Manifest::parse(&text, Path::new("t")).unwrap();
    let host = bare_host();
    let err = provider::for_manifest(&m)
        .plan_apply(&ctx(&m, &host))
        .unwrap_err();
    assert!(err.to_string().contains("P3"), "{err}");
    let err = provider::for_manifest(&m)
        .plan_uninstall(&ctx(&m, &host), false)
        .unwrap_err();
    assert!(err.to_string().contains("P3"), "{err}");
}
