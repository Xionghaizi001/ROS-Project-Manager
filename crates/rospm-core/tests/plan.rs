use std::path::{Path, PathBuf};

use rospm_core::lock::{Lock, LockedApt, LockedNative, LockedProvider, LockedSource};
use rospm_core::manifest::{Manifest, ProviderKind};
use rospm_core::mirror;
use rospm_core::plan::{shell_quote, Command, Plan, Step};
use rospm_core::platform::Platform;
use rospm_core::probe::{
    parse_dpkg_query, AptSources, Host, LocaleState, OsRelease, PackageDb, Status, ROS_APT_KEYRING,
    ROS_APT_LIST, UBUNTU_SNAPSHOT_LIST,
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
        ..AptSources::default()
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
        lock: None,
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
    assert!(!rendered.contains("autoremove -y"), "{rendered}");
    assert!(
        rendered.contains("native.uninstall.autoremove_hint"),
        "{rendered}"
    );
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
        .plan_build(&ctx(&m, &host), Some("main"), false)
        .unwrap();
    assert_eq!(plan.action_count(), 1);
    match &plan.steps[0] {
        Step::Run(c) => {
            assert_eq!(c.program, "colcon");
            assert_eq!(c.args[..4], ["build", "--mixin", "release", "ccache"]);
            assert!(c.preamble[0].contains("/opt/ros/jazzy/setup.bash"));
            assert!(c.preamble[1].contains("deps_ws/install/setup.bash"));
            let header = c.log.as_deref().unwrap();
            assert!(header.starts_with("workspace: main\npython: system\n"));
            assert!(header.contains("/log/latest_build/<package>/stdout_stderr.log"));
        }
        other => panic!("unexpected step {other:?}"),
    }
    assert_eq!(
        plan.log.as_deref(),
        Some(Path::new("/proj/.rospm/log/build.log"))
    );
}

#[test]
fn logged_commands_forward_and_save_output_and_keep_the_failure() {
    let dir = std::env::temp_dir().join(format!("rospm-build-log-{}", std::process::id()));
    let log = dir.join("log dir/build.log");
    let mut plan = Plan::new(rospm_core::message::Message::text("t"));
    plan.log = Some(log.clone());
    plan.run(
        Command::new("bash")
            .args(["-c", "printf '\\033[31mred\\033[0m\\n'; echo oops >&2"])
            .log("workspace: a\n"),
    );
    plan.run(Command::new("bash").args(["-c", "echo unlogged"]));
    plan.run(
        Command::new("bash")
            .args(["-c", "exit 3"])
            .log("verify: x\n"),
    );
    plan.run(
        Command::new("bash")
            .args(["-c", "echo never"])
            .log("workspace: b\n"),
    );

    let mut out = Vec::new();
    let err = plan.perform(&mut out).unwrap_err().to_string();
    assert!(err.contains("error.command_failed"), "{err}");
    let shown = String::from_utf8(out).unwrap();
    assert_eq!(shown.matches("plan.log(path=").count(), 1, "{shown}");

    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("==> command: bash -c"), "{text}");
    assert!(text.contains("workspace: a\n"), "{text}");
    assert!(text.contains("\x1b[31mred\x1b[0m\n"), "{text}");
    assert!(text.contains("oops\n"), "{text}");
    assert!(text.contains("verify: x\n"), "{text}");
    assert!(text.contains("==> exit status: 3"), "{text}");
    assert!(
        !text.contains("unlogged") && !text.contains("never"),
        "{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_verify_runs_after_full_build_with_project_environment() {
    let text = format!("{FULL}\n[build]\nverify = ['ros2 pkg list']\n");
    let m = Manifest::parse(&text, Path::new("t")).unwrap();
    let host = bare_host();
    let provider = provider::for_manifest(&m);
    let plain = provider.plan_build(&ctx(&m, &host), None, false).unwrap();
    let plan = provider.plan_build(&ctx(&m, &host), None, true).unwrap();
    assert_eq!(plan.action_count(), plain.action_count() + 1);
    match plan.steps.last().unwrap() {
        Step::Run(c) => {
            assert_eq!(c.program, "bash");
            assert_eq!(c.cwd.as_deref(), Some(Path::new("/proj")));
            let script = c.args.last().unwrap();
            assert!(script.starts_with("set -e\n"));
            assert!(script.contains("ws/install/setup.bash"));
            assert!(script.ends_with("ros2 pkg list"));
            assert_eq!(
                c.log.as_deref(),
                Some("verify: ros2 pkg list\npython: system\n")
            );
        }
        other => panic!("unexpected step {other:?}"),
    }

    let conda = Manifest::parse(
        &format!("{text}\n[build.conda]\nname = 'robot-dev'\n"),
        Path::new("t"),
    )
    .unwrap();
    let plan = provider::for_manifest(&conda)
        .plan_build(&ctx(&conda, &host), None, true)
        .unwrap();
    match plan.steps.last().unwrap() {
        Step::Run(c) => {
            assert_eq!(c.program, "conda");
            assert_eq!(c.args[4..8], ["bash", "--noprofile", "--norc", "-c"]);
            assert!(c
                .log
                .as_deref()
                .unwrap()
                .ends_with("python: conda -n robot-dev\n"));
        }
        other => panic!("unexpected step {other:?}"),
    }

    let bad = Manifest::parse(
        &format!("{FULL}\n[build]\nverify = [' ']\n"),
        Path::new("t"),
    );
    assert!(bad.is_err());
}

#[test]
fn conda_build_wraps_every_workspace_and_preserves_colcon_configuration() {
    for selector in [
        "name = 'robot-dev'",
        "prefix = '.conda/robot dev'",
        "prefix = '/opt/robot dev'",
    ] {
        let m = Manifest::parse(
            &format!("{FULL}\n[build.conda]\n{selector}\n"),
            Path::new("t"),
        )
        .unwrap();
        let host = bare_host();
        let plan = provider::for_manifest(&m)
            .plan_build(&ctx(&m, &host), None, false)
            .unwrap();
        assert_eq!(plan.action_count(), 4);
        let mut commands: Vec<_> = plan
            .steps
            .iter()
            .filter_map(|s| match s {
                Step::Run(c) => Some(c),
                _ => None,
            })
            .collect();
        let pin = commands.pop().unwrap();
        assert_eq!(pin.args[4..7], ["python", "-I", "-c"]);
        assert_eq!(pin.args[8..], ["/proj/deps_ws/install", "/proj/ws/install"]);
        assert_eq!(commands[0].cwd.as_deref(), Some(Path::new("/proj/deps_ws")));
        assert_eq!(commands[1].cwd.as_deref(), Some(Path::new("/proj/ws")));
        for cmd in &commands {
            assert_eq!(cmd.program, "conda");
            assert_eq!(cmd.args[..2], ["run", "--no-capture-output"]);
            match selector {
                s if s.starts_with("name") => assert_eq!(cmd.args[2..4], ["-n", "robot-dev"]),
                s if s.contains("/opt/") => assert_eq!(cmd.args[2..4], ["-p", "/opt/robot dev"]),
                _ => assert_eq!(cmd.args[2..4], ["-p", "/proj/.conda/robot dev"]),
            }
            assert_eq!(cmd.args[4..8], ["bash", "--noprofile", "--norc", "-c"]);
            assert!(cmd.preamble.is_empty(), "ROS must be sourced inside Conda");
            assert!(cmd.args[8].starts_with("set -e\nsource /opt/ros/jazzy/setup.bash"));
            assert!(cmd.args[8].contains("colcon build --mixin release"));
        }
        let deps_script = &commands[0].args[8];
        assert!(deps_script.contains("COLCON_DEFAULTS_FILE=/proj/colcon/deps.defaults.yaml"));
        let main_script = &commands[1].args[8];
        assert!(main_script.contains("deps_ws/install/setup.bash"));
        // A not-yet-built underlay must not stop the `&&` chain before colcon.
        let guard = main_script
            .split(" && ")
            .find(|l| l.contains("deps_ws/install/setup.bash"))
            .unwrap();
        let ran = std::process::Command::new("bash")
            .args(["-c", &format!("set -e\n{guard} && echo colcon")])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&ran.stdout), "colcon\n", "{guard}");
        assert!(main_script.contains("--mixin release ccache --symlink-install"));
    }
}

#[test]
fn conda_build_pins_python_shebangs_to_the_environment() {
    let m = Manifest::parse(
        &format!("{FULL}\n[build.conda]\nname = 'robot'\n"),
        Path::new("t"),
    )
    .unwrap();
    let host = bare_host();
    let plan = provider::for_manifest(&m)
        .plan_build(&ctx(&m, &host), None, false)
        .unwrap();
    let Some(Step::Run(pin)) = plan.steps.last() else {
        panic!("last step is not the shebang command");
    };
    let dir = std::env::temp_dir().join(format!("rospm-shebang-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let node = |name: &str, text: &str, mode: u32| {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("lib/pkg").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    };
    let env = node("env_node", "#!/usr/bin/env python3\nprint(1)\n", 0o755);
    let abs = node("abs_node", "#!/usr/bin/python3 -u\nprint(2)\n", 0o755);
    let sh = node("sh_node", "#!/bin/sh\necho\n", 0o755);
    let data = node("data.py", "#!/usr/bin/python3\n", 0o644);
    let linked = dir.join("lib/pkg/linked");
    std::os::unix::fs::symlink(&data, &linked).unwrap();
    let out = std::process::Command::new("python3")
        .args(&pin.args[5..8])
        .arg(&dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let python = String::from_utf8(
        std::process::Command::new("python3")
            .args(["-I", "-c", "import sys; print(sys.executable)"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let python = python.trim();
    let read = |p: &Path| std::fs::read_to_string(p).unwrap();
    assert_eq!(read(&env), format!("#!{python}\nprint(1)\n"));
    assert_eq!(read(&abs), format!("#!{python} -u\nprint(2)\n"));
    assert_eq!(read(&sh), "#!/bin/sh\necho\n");
    assert_eq!(
        read(&data),
        "#!/usr/bin/python3\n",
        "not executable, and not via symlink"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 2);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn conda_build_passes_the_environment_interpreter_to_cmake() {
    let text = FULL.replace(
        r#"build_args = ["--symlink-install", "--event-handlers""#,
        r#"build_args = ["--cmake-args", "-DFOO=1", "--symlink-install", "--event-handlers""#,
    );
    let m = Manifest::parse(
        &format!("{text}\n[build.conda]\nname = 'robot'\n"),
        Path::new("t"),
    )
    .unwrap();
    let host = bare_host();
    let plan = provider::for_manifest(&m)
        .plan_build(&ctx(&m, &host), None, false)
        .unwrap();
    let scripts: Vec<_> = plan
        .steps
        .iter()
        .filter_map(|s| match s {
            Step::Run(c) => Some(c.args[8].clone()),
            _ => None,
        })
        .collect();
    let defines = r#""-DPython3_EXECUTABLE=$CONDA_PREFIX/bin/python" "-DPython_EXECUTABLE=$CONDA_PREFIX/bin/python" "-DPYTHON_EXECUTABLE=$CONDA_PREFIX/bin/python""#;
    // Appended when the workspace sets none (its defaults file is absent) ...
    assert!(
        scripts[0].ends_with(&format!("--symlink-install --cmake-args {defines}")),
        "{}",
        scripts[0]
    );
    // ... and joined to the user's, since colcon keeps only the last one.
    assert!(
        scripts[1].contains(&format!("--cmake-args {defines} -DFOO=1 --symlink-install")),
        "{}",
        scripts[1]
    );
    assert_eq!(scripts[1].matches("--cmake-args").count(), 1);
    // bash expands the prefix that `conda run` exports.
    let ran = std::process::Command::new("bash")
        .env("CONDA_PREFIX", "/env dir")
        .args(["-c", &format!("printf '%s\\n' {defines}")])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&ran.stdout)
        .starts_with("-DPython3_EXECUTABLE=/env dir/bin/python\n"));
}

#[test]
fn managed_conda_is_prepared_before_apply_mutations_and_build_only_checks() {
    let text =
        format!("{FULL}\n[build.conda.managed]\npython = '3.12'\nchannels = ['conda-forge']\n");
    let m = Manifest::parse(&text, Path::new("t")).unwrap();
    let host = ready_host();
    let context = ctx(&m, &host);
    let provider = provider::for_manifest(&m);
    let apply = provider.plan_apply(&context).unwrap();
    let first_action = apply
        .steps
        .iter()
        .find(|s| !matches!(s, Step::Note(_)))
        .unwrap();
    assert!(matches!(first_action, Step::Conda(_)));
    let preview = apply.render();
    assert!(preview.contains("/proj/.rospm/conda/env"));
    assert!(preview.contains("conda env create --yes --prefix"));
    assert!(preview.contains("python=3.12"));
    let build = provider.plan_build(&context, Some("main"), false).unwrap();
    assert!(matches!(&build.steps[0], Step::Conda(_)));
    assert!(build
        .render()
        .contains("conda run --no-capture-output -p /proj/.rospm/conda/env"));
    assert!(!build.render().contains("conda env create"));

    let mut locked = context;
    locked.locked = true;
    // No system plan can be executed without a complete lock; both the
    // existing native guard and managed environment guard refuse it.
    assert!(provider.plan_apply(&locked).is_err());
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
fn env_script_activates_conda_before_sourcing_ros() {
    for (selector, target) in [
        ("name = 'robot-dev'", "conda activate robot-dev\n"),
        ("prefix = 'my env'", "conda activate '/proj/my env'\n"),
    ] {
        let m = Manifest::parse(
            &format!("{FULL}\n[build.conda]\n{selector}\n"),
            Path::new("t"),
        )
        .unwrap();
        let host = bare_host();
        let script = provider::for_manifest(&m)
            .env_script(&ctx(&m, &host), Shell::Zsh)
            .unwrap();
        let hook = script.find("eval \"$(conda shell.zsh hook)\"\n").unwrap();
        let activate = script.find(target).unwrap();
        let ros = script.find("/opt/ros/jazzy/setup.zsh").unwrap();
        assert!(hook < activate && activate < ros, "{script}");
    }
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

/// A minimal, valid native lock: the given ROS/Ubuntu snapshot dates,
/// pinned apt for `LinuxX86_64` only, and one git source per workspace.
fn locked(ros_snapshot: Option<&str>, ubuntu_snapshot: Option<&str>) -> Lock {
    Lock {
        schema_version: 1,
        generated_at: "2026-09-11T00:00:00Z".into(),
        manifest_sha256: "sha256:0000000000000000000000000000000000000000000000000000000000000000"
            .into(),
        provider: LockedProvider {
            kind: ProviderKind::Native,
            native: Some(LockedNative {
                ubuntu: "24.04".into(),
                ros_snapshot: ros_snapshot.map(str::to_string),
                ubuntu_snapshot: ubuntu_snapshot.map(str::to_string),
            }),
            pixi: None,
            docker: None,
        },
        sources: vec![
            LockedSource {
                workspace: "deps".into(),
                name: "foo_pkg".into(),
                kind: "git".into(),
                url: "https://github.com/example/foo_pkg".into(),
                rev: "abc123deadbeef".into(),
                version: None,
            },
            LockedSource {
                workspace: "main".into(),
                name: "bar_pkg".into(),
                kind: "git".into(),
                url: "https://github.com/example/bar_pkg".into(),
                rev: "def456cafebabe".into(),
                version: None,
            },
        ],
        apt: vec![
            LockedApt {
                platform: Platform::LinuxX86_64,
                name: "ros-jazzy-ros-base".into(),
                version: "2.4.1-1noble.20260910.194500".into(),
                sha256: None,
            },
            LockedApt {
                platform: Platform::LinuxX86_64,
                name: "python3-rosdep".into(),
                version: "0.24.0-1".into(),
                sha256: None,
            },
            LockedApt {
                platform: Platform::LinuxAarch64,
                name: "libbar-arm-dev".into(),
                version: "1.0-1".into(),
                sha256: None,
            },
        ],
        pip: vec![],
        conda: vec![],
    }
}

/// `ctx()` plus `locked: true` and a lock reference.
fn locked_ctx<'a>(m: &'a Manifest, host: &'a Host, lock: &'a Lock) -> Context<'a> {
    Context {
        locked: true,
        lock: Some(lock),
        ..ctx(m, host)
    }
}

#[test]
fn native_apply_locked_pins_the_ros_and_ubuntu_snapshots_and_every_apt_and_git_source() {
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let host = bare_host();
    let lock = locked(Some("2026-09-11"), Some("20260911T182146Z"));
    let plan = provider::for_manifest(&m)
        .plan_apply(&locked_ctx(&m, &host, &lock))
        .unwrap();
    let rendered = plan.render();

    // ROS snapshot: rospm's own file, signed by the snapshot key (not the
    // regular ROS key), key fetched by fingerprint from the keyserver.
    assert!(
        rendered.contains(
            "plan.fetch_key(url=https://keyserver.ubuntu.com/pks/lookup?op=get&options=mr&search=0x4B63CF8FDE49746E98FA01DDAD19BAB3CBF125EA, dest=/usr/share/keyrings/ros2-snapshots-archive-keyring.gpg, fingerprint=4B63CF8FDE49746E98FA01DDAD19BAB3CBF125EA)"
        ),
        "{rendered}"
    );
    assert!(
        rendered.contains(&format!("plan.write_file_sudo(path={ROS_APT_LIST}, bytes=")),
        "{rendered}"
    );
    assert!(
        rendered.contains(
            "| deb [arch=amd64 signed-by=/usr/share/keyrings/ros2-snapshots-archive-keyring.gpg] http://snapshots.ros.org/jazzy/2026-09-11/ubuntu noble main"
        ),
        "{rendered}"
    );
    assert!(!rendered.contains("trusted=yes"), "{rendered}");

    // Ubuntu snapshot: a separate, rospm-owned file; Ubuntu's own
    // sources.list is never mentioned.
    assert!(
        rendered.contains(&format!(
            "plan.write_file_sudo(path={UBUNTU_SNAPSHOT_LIST}, bytes="
        )),
        "{rendered}"
    );
    assert!(
        rendered.contains(
            "| deb [arch=amd64 signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] https://snapshot.ubuntu.com/ubuntu/20260911T182146Z noble-security main restricted universe multiverse"
        ),
        "{rendered}"
    );
    assert!(!rendered.contains("/etc/apt/sources.list\n"), "{rendered}");
    // The snapshot file only takes effect after an `apt-get update` that runs
    // before the first pinned install.
    let ubuntu_write = rendered
        .find(&format!(
            "plan.write_file_sudo(path={UBUNTU_SNAPSHOT_LIST}, bytes="
        ))
        .unwrap();
    let pinned_install = rendered
        .find("apt-get install -y --allow-downgrades")
        .unwrap();
    assert!(
        rendered[ubuntu_write..pinned_install].contains("sudo apt-get update"),
        "{rendered}"
    );

    // Every pinned apt package for this platform, exactly, none for the other.
    assert!(
        rendered.contains("sudo apt-get install -y --allow-downgrades ros-jazzy-ros-base=2.4.1-1noble.20260910.194500"),
        "{rendered}"
    );
    assert!(rendered.contains("python3-rosdep=0.24.0-1"), "{rendered}");
    assert!(!rendered.contains("libbar-arm-dev"), "{rendered}");

    // Git sources pinned by `checkout --detach`, no `rosdep install`.
    assert!(
        rendered.contains("(cd /proj/deps_ws/src/foo_pkg && git checkout --detach abc123deadbeef)"),
        "{rendered}"
    );
    assert!(
        rendered.contains("(cd /proj/ws/src/bar_pkg && git checkout --detach def456cafebabe)"),
        "{rendered}"
    );
    assert!(!rendered.contains("rosdep install"), "{rendered}");
    assert!(rendered.contains("sudo rosdep init"), "{rendered}");
}

#[test]
fn native_apply_locked_degrades_to_the_live_repositories_when_the_lock_has_no_snapshot() {
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let host = bare_host();
    let lock = locked(None, None);
    let rendered = provider::for_manifest(&m)
        .plan_apply(&locked_ctx(&m, &host, &lock))
        .unwrap()
        .render();
    assert!(
        rendered.contains("native.apply.locked_degraded(what=ROS apt)"),
        "{rendered}"
    );
    assert!(
        rendered.contains("native.apply.locked_degraded(what=Ubuntu apt)"),
        "{rendered}"
    );
    // Falls back to exactly the same live-repository bootstrap as a
    // non-locked apply (spec §3.2 degradation).
    assert!(rendered.contains("http://packages.ros.org/ros2/ubuntu noble main"));
    assert!(!rendered.contains("snapshots.ros.org"), "{rendered}");
    assert!(!rendered.contains(UBUNTU_SNAPSHOT_LIST), "{rendered}");
    // The pinned apt set still installs (the lock has no snapshot, not no apt).
    assert!(rendered.contains("--allow-downgrades ros-jazzy-ros-base="));
}

#[test]
fn native_apply_reports_builtin_replacements_but_locked_replay_keeps_recorded_names() {
    let mut m = Manifest::parse(FULL, Path::new("t")).unwrap();
    m.dependencies.apt = vec!["gcc-x86-64-linux-gnu".into()];
    let host = bare_host();
    let mut c = ctx(&m, &host);
    c.platform = Platform::LinuxAarch64;
    let rendered = provider::for_manifest(&m).plan_apply(&c).unwrap().render();
    assert!(rendered.contains("sudo apt-get install -y gcc-aarch64-linux-gnu"));
    assert!(rendered.contains("replacement.builtin(platform=linux-aarch64, from=gcc-x86-64-linux-gnu, to=gcc-aarch64-linux-gnu, revision="));

    let mut lock = locked(None, None);
    for apt in &mut lock.apt {
        apt.platform = Platform::LinuxAarch64;
    }
    lock.apt.push(LockedApt {
        platform: Platform::LinuxAarch64,
        name: "gcc-x86-64-linux-gnu".into(),
        version: "13.2".into(),
        sha256: None,
    });
    c.locked = true;
    c.lock = Some(&lock);
    let rendered = provider::for_manifest(&m).plan_apply(&c).unwrap().render();
    assert!(rendered.contains("gcc-x86-64-linux-gnu=13.2"), "{rendered}");
    assert!(!rendered.contains("gcc-aarch64-linux-gnu"), "{rendered}");
    assert!(!rendered.contains("replacement.builtin"));
}

#[test]
fn native_apply_locked_without_a_lock_is_an_error() {
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let host = bare_host();
    let mut c = ctx(&m, &host);
    c.locked = true;
    let err = provider::for_manifest(&m).plan_apply(&c).unwrap_err();
    assert_eq!(err.to_string(), "native.apply.locked_no_lock");
}

#[test]
fn native_apply_locked_leaves_a_foreign_ros_apt_source_alone() {
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let mut host = bare_host();
    host.apt_sources.ros2 = Some(PathBuf::from("/etc/apt/sources.list.d/ros2.sources"));
    let lock = locked(Some("2026-09-11"), Some("20260911T182146Z"));
    let rendered = provider::for_manifest(&m)
        .plan_apply(&locked_ctx(&m, &host, &lock))
        .unwrap()
        .render();
    assert!(
        rendered.contains(
            "native.apply.locked_snapshot_foreign(file=/etc/apt/sources.list.d/ros2.sources)"
        ),
        "{rendered}"
    );
    assert!(
        !rendered.contains(&format!("plan.write_file_sudo(path={ROS_APT_LIST}")),
        "{rendered}"
    );
    // The Ubuntu snapshot and the pinned apt set are unaffected by the
    // foreign ROS source.
    assert!(rendered.contains(UBUNTU_SNAPSHOT_LIST));
    assert!(rendered.contains("--allow-downgrades ros-jazzy-ros-base="));
}

#[test]
fn native_apply_locked_reinstalls_an_armored_snapshot_keyring() {
    // Pinned to the same snapshot already; only the keyring differs.
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let mut host = bare_host();
    host.apt_sources.ros2 = Some(PathBuf::from(ROS_APT_LIST));
    host.apt_sources.ros2_snapshot =
        Some("http://snapshots.ros.org/jazzy/2026-09-11/ubuntu".to_string());
    host.apt_sources.snapshot_keyring = true;
    let lock = locked(Some("2026-09-11"), Some("20260911T182146Z"));
    let plan_for = |host: &Host| {
        provider::for_manifest(&m)
            .plan_apply(&locked_ctx(&m, host, &lock))
            .unwrap()
            .render()
    };

    let binary = plan_for(&host);
    assert!(!binary.contains("plan.fetch_key("), "{binary}");

    // Written armoured by rospm before the 2026-09-26 fix: apt reports
    // NO_PUBKEY, so it must be fetched (and dearmoured) again.
    host.apt_sources.snapshot_keyring_armored = true;
    let armored = plan_for(&host);
    assert!(
        armored.contains("dest=/usr/share/keyrings/ros2-snapshots-archive-keyring.gpg"),
        "{armored}"
    );
}

#[test]
fn native_uninstall_removes_orphaned_snapshot_leftovers_without_a_ros_source() {
    // An interrupted `apply --locked` + an earlier uninstall: `ros2.list` is
    // gone but the snapshot keyring and the Ubuntu snapshot file remain.
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let mut host = ready_host();
    host.packages = installed(&["python3-rosdep"]);
    host.apt_sources.ros2 = None;
    host.apt_sources.snapshot_keyring = true;
    host.apt_sources.ubuntu_snapshot =
        Some("https://snapshot.ubuntu.com/ubuntu/20260911T182146Z".to_string());
    let rendered = provider::for_manifest(&m)
        .plan_uninstall(&ctx(&m, &host), false)
        .unwrap()
        .render();
    assert!(
        rendered.contains(&format!(
            "sudo rm -f /usr/share/keyrings/ros2-snapshots-archive-keyring.gpg {UBUNTU_SNAPSHOT_LIST}"
        )),
        "{rendered}"
    );
    assert!(rendered.contains("sudo apt-get update"), "{rendered}");

    // `--keep-source` keeps them.
    let kept = provider::for_manifest(&m)
        .plan_uninstall(&ctx(&m, &host), true)
        .unwrap();
    assert_eq!(kept.action_count(), 0, "{}", kept.render());

    // A foreign ROS source (e.g. `ros2-apt-source`) stays, rospm's snapshot files go.
    host.apt_sources.ros2 = Some(PathBuf::from("/etc/apt/sources.list.d/ros2.sources"));
    let rendered = provider::for_manifest(&m)
        .plan_uninstall(&ctx(&m, &host), false)
        .unwrap()
        .render();
    assert!(
        rendered.contains("native.uninstall.source_not_ours"),
        "{rendered}"
    );
    assert!(
        rendered.contains(&format!(
            "sudo rm -f /usr/share/keyrings/ros2-snapshots-archive-keyring.gpg {UBUNTU_SNAPSHOT_LIST}"
        )),
        "{rendered}"
    );
    assert!(
        !rendered
            .lines()
            .any(|l| l.contains("rm -f") && l.contains("ros2.sources")),
        "{rendered}"
    );
    let kept = provider::for_manifest(&m)
        .plan_uninstall(&ctx(&m, &host), true)
        .unwrap();
    assert_eq!(kept.action_count(), 0, "{}", kept.render());
}

#[test]
fn native_apply_locked_removes_its_own_broken_sources_before_the_first_update() {
    // Left by a failed `apply --locked` (2026-09-26): rospm's `ros2.list`
    // points at the snapshot but the keyring is armoured, and the Ubuntu
    // snapshot file is from an older lock. Both would make the initial
    // `apt-get update` fail before the steps that repair them.
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let mut host = bare_host();
    host.apt_sources.ros2 = Some(PathBuf::from(ROS_APT_LIST));
    host.apt_sources.ros2_snapshot =
        Some("http://snapshots.ros.org/jazzy/2026-09-11/ubuntu".to_string());
    host.apt_sources.snapshot_keyring = true;
    host.apt_sources.snapshot_keyring_armored = true;
    host.apt_sources.ubuntu_snapshot =
        Some("https://snapshot.ubuntu.com/ubuntu/20260801T000000Z".to_string());
    let lock = locked(Some("2026-09-11"), Some("20260911T182146Z"));
    let rendered = provider::for_manifest(&m)
        .plan_apply(&locked_ctx(&m, &host, &lock))
        .unwrap()
        .render();
    let rm = rendered
        .find(&format!("sudo rm -f {ROS_APT_LIST} {UBUNTU_SNAPSHOT_LIST}"))
        .unwrap_or_else(|| panic!("{rendered}"));
    let first_update = rendered.find("sudo apt-get update").unwrap();
    assert!(rm < first_update, "{rendered}");
    // Both are written again afterwards.
    assert!(
        rendered[first_update..].contains(&format!("plan.write_file_sudo(path={ROS_APT_LIST}, ")),
        "{rendered}"
    );
    assert!(
        rendered[first_update..].contains(&format!(
            "plan.write_file_sudo(path={UBUNTU_SNAPSHOT_LIST}, "
        )),
        "{rendered}"
    );

    // A healthy, current setup is left alone.
    host.apt_sources.snapshot_keyring_armored = false;
    host.apt_sources.ubuntu_snapshot =
        Some("https://snapshot.ubuntu.com/ubuntu/20260911T182146Z".to_string());
    let rendered = provider::for_manifest(&m)
        .plan_apply(&locked_ctx(&m, &host, &lock))
        .unwrap()
        .render();
    assert!(!rendered.contains("sudo rm -f"), "{rendered}");

    // A foreign ROS source file is never removed.
    host.apt_sources.ros2 = Some(PathBuf::from("/etc/apt/sources.list.d/ros2.sources"));
    host.apt_sources.snapshot_keyring_armored = true;
    let rendered = provider::for_manifest(&m)
        .plan_apply(&locked_ctx(&m, &host, &lock))
        .unwrap()
        .render();
    assert!(!rendered.contains("sudo rm -f"), "{rendered}");
}

#[test]
fn native_apply_locked_keeps_newer_ubuntu_packages_but_pins_the_ros_layer_exactly() {
    // 2026-09-26: `curl=<older>` needs the same older `libcurl4`, which the
    // lock does not record; apt refused the whole install (exit 100).
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let mut host = bare_host();
    let mut lock = locked(Some("2026-09-11"), Some("20260911T182146Z"));
    lock.apt.push(LockedApt {
        platform: Platform::LinuxX86_64,
        name: "curl".into(),
        version: "7.81.0-1ubuntu1.25".into(),
        sha256: None,
    });
    lock.apt.push(LockedApt {
        platform: Platform::LinuxX86_64,
        name: "libfoo-dev".into(),
        version: "2.0-1".into(),
        sha256: None,
    });
    host.packages = PackageDb::Static(parse_dpkg_query(
        // curl newer (kept), libfoo-dev older (upgraded to the lock),
        // rosdep and the base newer (ROS layer: downgraded to the lock).
        "curl\tinstalled\t7.81.0-1ubuntu1.29\nlibfoo-dev\tinstalled\t1.0-1\npython3-rosdep\tinstalled\t0.27.0-1\nros-jazzy-ros-base\tinstalled\t9.9.9-1noble\n",
    ));
    let rendered = provider::for_manifest(&m)
        .plan_apply(&locked_ctx(&m, &host, &lock))
        .unwrap()
        .render();
    assert!(!rendered.contains("curl=7.81.0-1ubuntu1.25"), "{rendered}");
    assert!(
        rendered.contains(
            "native.apply.locked_kept_newer(count=1, packages=curl 7.81.0-1ubuntu1.29 > 7.81.0-1ubuntu1.25)"
        ),
        "{rendered}"
    );
    assert!(rendered.contains("libfoo-dev=2.0-1"), "{rendered}");
    assert!(rendered.contains("python3-rosdep=0.24.0-1"), "{rendered}");
    assert!(
        rendered.contains("ros-jazzy-ros-base=2.4.1-1noble.20260910.194500"),
        "{rendered}"
    );
}
