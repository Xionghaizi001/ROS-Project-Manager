use std::path::{Path, PathBuf};

use rospm_core::manifest::Manifest;
use rospm_core::mirror;
use rospm_core::plan::{shell_quote, Command, Step};
use rospm_core::platform::Platform;
use rospm_core::probe::{
    parse_dpkg_query, AptSources, Host, LocaleState, OsRelease, PackageDb, Status, ROS_APT_KEYRING,
    ROS_APT_LIST,
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
    h.locale = LocaleState::with_system("C.UTF-8");
    h.apt_sources = AptSources {
        ros2: Some(PathBuf::from(ROS_APT_LIST)),
        ros2_mirror: Some("official"),
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
        mirror: mirror::OFFICIAL,
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
    let prereqs = pos(
        "sudo apt-get install -y locales software-properties-common curl ca-certificates gnupg",
    );
    let locale = pos("sudo locale-gen en_US en_US.UTF-8");
    // Only LANG is set system-wide; LC_ALL would pin every category for everyone.
    pos("sudo update-locale LANG=en_US.UTF-8\n");
    assert!(!rendered.contains("LC_ALL="), "{rendered}");
    pos("native.apply.locale_switched(file=/etc/default/locale, from=native.apply.locale_no_file)");
    let universe = pos("sudo add-apt-repository -y universe");
    let key = pos(&format!(
        "plan.fetch_key(url=https://raw.githubusercontent.com/ros/rosdistro/master/ros.key, dest={ROS_APT_KEYRING}, fingerprint=C1CF6E31E6BADE8868B172B4F42ED6FBAB17C654)"
    ));
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
        "plan.fetch_key(",
        "plan.write_file_sudo(path=/etc/apt",
        "rosdep init",
    ] {
        assert!(!rendered.contains(absent), "`{absent}` in:\n{rendered}");
    }
    assert!(rendered.contains(
        "native.apply.source_already_configured(file=/etc/apt/sources.list.d/ros2.list, source=official)"
    ));
    assert!(rendered.contains("sudo apt-get install -y ros-jazzy-ros-base"));
}

#[test]
fn native_apply_reads_the_system_locale_not_the_callers_environment() {
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();

    // `LANG=C rospm apply` on a host whose /etc/default/locale is UTF-8 must
    // not rewrite that file (it did before P1.5's last item).
    let mut host = ready_host();
    host.locale = LocaleState::with_system("zh_CN.UTF-8");
    host.locale.process = Some("C".into());
    let rendered = provider::for_manifest(&m)
        .plan_apply(&ctx(&m, &host))
        .unwrap()
        .render();
    assert!(!rendered.contains("locale-gen"), "{rendered}");
    assert!(!rendered.contains("update-locale"), "{rendered}");
    assert!(
        rendered.contains(
            "native.apply.shell_locale_not_utf8(value=C, file=/etc/default/locale, system=zh_CN.UTF-8)"
        ),
        "{rendered}"
    );

    // The reverse: a UTF-8 shell does not excuse a POSIX system default.
    let mut host = ready_host();
    host.locale = LocaleState::with_system("C");
    host.locale.process = Some("en_US.UTF-8".into());
    let rendered = provider::for_manifest(&m)
        .plan_apply(&ctx(&m, &host))
        .unwrap()
        .render();
    assert!(
        rendered.contains("sudo locale-gen en_US en_US.UTF-8"),
        "{rendered}"
    );
    assert!(
        rendered.contains("sudo update-locale LANG=en_US.UTF-8\n"),
        "{rendered}"
    );
    assert!(
        rendered.contains("native.apply.locale_switched(file=/etc/default/locale, from=LANG=C)"),
        "{rendered}"
    );

    // A non-UTF-8 LC_ALL in the file overrides LANG, so it is removed
    // (bare variable name) rather than overwritten.
    let mut host = ready_host();
    host.locale = LocaleState::from_default_locale("LANG=en_US.UTF-8\nLC_ALL=C\n");
    let rendered = provider::for_manifest(&m)
        .plan_apply(&ctx(&m, &host))
        .unwrap()
        .render();
    assert!(
        rendered.contains("sudo update-locale LANG=en_US.UTF-8 LC_ALL\n"),
        "{rendered}"
    );
    assert!(rendered.contains("from=LC_ALL=C)"), "{rendered}");

    // The file exists but selects nothing: POSIX, fix it.
    let mut host = ready_host();
    host.locale = LocaleState::from_default_locale("# empty\n");
    let rendered = provider::for_manifest(&m)
        .plan_apply(&ctx(&m, &host))
        .unwrap()
        .render();
    assert!(rendered.contains("from=POSIX)"), "{rendered}");
}

#[test]
fn native_check_reports_system_and_shell_locale_separately() {
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let mut host = ready_host();
    host.locale = LocaleState::with_system("zh_CN.UTF-8");
    host.locale.process = Some("C".into());
    let report = provider::for_manifest(&m)
        .probe_env(&ctx(&m, &host))
        .unwrap();
    let row = |subject: &str| {
        report
            .items
            .iter()
            .find(|i| i.subject.to_string() == subject)
            .unwrap_or_else(|| panic!("no `{subject}` row"))
    };
    let system = row("probe.subject.locale_system");
    assert_eq!(system.status, Status::Ok);
    assert_eq!(
        system.actual.to_string(),
        "probe.locale.system(value=zh_CN.UTF-8, file=/etc/default/locale)"
    );
    // The shell is only informational: `LANG=C rospm check --strict` in CI passes.
    let shell = row("probe.subject.locale_shell");
    assert_eq!(shell.status, Status::Info);
    assert_eq!(
        shell.actual.to_string(),
        "probe.locale.shell_not_utf8(value=C)"
    );

    host.locale = LocaleState::with_system("C");
    let report = provider::for_manifest(&m)
        .probe_env(&ctx(&m, &host))
        .unwrap();
    let system = report
        .items
        .iter()
        .find(|i| i.subject.to_string() == "probe.subject.locale_system")
        .unwrap();
    assert_eq!(system.status, Status::Missing);

    // No file at all: the process value decides, and the row says so.
    host.locale = LocaleState::default();
    host.locale.process = Some("C.UTF-8".into());
    let report = provider::for_manifest(&m)
        .probe_env(&ctx(&m, &host))
        .unwrap();
    let system = report
        .items
        .iter()
        .find(|i| i.subject.to_string() == "probe.subject.locale_system")
        .unwrap();
    assert_eq!(system.status, Status::Ok);
    assert_eq!(
        system.actual.to_string(),
        "probe.locale.no_file(file=/etc/default/locale, value=C.UTF-8)"
    );
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

#[test]
fn native_apply_with_a_mirror_rewrites_every_download_url() {
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let mut host = bare_host();
    host.rosdep.initialized = true; // an existing GitHub list gets rewritten
    let mut c = ctx(&m, &host);
    c.mirror = mirror::TUNA;
    let plan = provider::for_manifest(&m).plan_apply(&c).unwrap();
    let rendered = plan.render();
    for needle in [
        "plan.fetch_key(url=https://mirrors.tuna.tsinghua.edu.cn/rosdistro/ros.key, dest=",
        "fingerprint=C1CF6E31E6BADE8868B172B4F42ED6FBAB17C654)",
        "https://mirrors.tuna.tsinghua.edu.cn/ros2/ubuntu noble main",
        "plan.write_file_sudo(path=/etc/ros/rosdep/sources.list.d/20-default.list, bytes=",
        "yaml https://mirrors.tuna.tsinghua.edu.cn/rosdistro/rosdep/base.yaml",
        "ROSDISTRO_INDEX_URL=https://mirrors.tuna.tsinghua.edu.cn/rosdistro/index-v4.yaml rosdep update",
        "native.apply.rosdep_list_rewrite(file=/etc/ros/rosdep/sources.list.d/20-default.list, name=tuna)",
        "native.apply.mirror_in_use(name=tuna, label=",
    ] {
        assert!(rendered.contains(needle), "missing `{needle}` in:\n{rendered}");
    }
    assert!(!rendered.contains("githubusercontent"), "{rendered}");
    assert!(!rendered.contains("packages.ros.org"), "{rendered}");
    assert!(!rendered.contains("rosdep init"), "{rendered}");

    // An apt-only mirror keeps rosdep on the official tree.
    c.mirror = mirror::ALIYUN;
    let rendered = provider::for_manifest(&m).plan_apply(&c).unwrap().render();
    assert!(rendered.contains("https://mirrors.aliyun.com/ros2/ubuntu noble main"));
    assert!(rendered.contains(
        "plan.fetch_key(url=https://raw.githubusercontent.com/ros/rosdistro/master/ros.key, dest="
    ));
    assert!(!rendered.contains("20-default.list"), "{rendered}");
    assert!(!rendered.contains("ROSDISTRO_INDEX_URL"), "{rendered}");
}

#[test]
fn native_apply_rewrites_its_own_apt_source_for_a_new_mirror_but_not_foreign_files() {
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let mut host = ready_host(); // ros2.list written by rospm, official
    let mut c = ctx(&m, &host);
    c.mirror = mirror::USTC;
    let rendered = provider::for_manifest(&m).plan_apply(&c).unwrap().render();
    assert!(rendered.contains(
        "native.apply.source_rewrite(file=/etc/apt/sources.list.d/ros2.list, from=official, to=ustc)"
    ));
    assert!(rendered.contains("https://mirrors.ustc.edu.cn/ros2/ubuntu noble main"));

    host.apt_sources.ros2 = Some(PathBuf::from("/etc/apt/sources.list.d/ros2.sources"));
    let c2 = Context {
        mirror: mirror::USTC,
        ..ctx(&m, &host)
    };
    let rendered = provider::for_manifest(&m).plan_apply(&c2).unwrap().render();
    assert!(
        rendered.contains(
            "native.apply.source_foreign(name=ustc, file=/etc/apt/sources.list.d/ros2.sources)"
        ),
        "{rendered}"
    );
    assert!(
        !rendered.contains("plan.write_file_sudo(path=/etc/apt"),
        "{rendered}"
    );
}
