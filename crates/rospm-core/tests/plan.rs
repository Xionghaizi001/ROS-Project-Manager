use std::path::Path;

use rospm_core::manifest::Manifest;
use rospm_core::plan::{shell_quote, Command, Step};
use rospm_core::platform::Platform;
use rospm_core::provider::{self, Context, Shell};

const FULL: &str = include_str!("fixtures/full.toml");

fn ctx(m: &Manifest) -> Context<'_> {
    Context {
        manifest: m,
        root: Path::new("/proj"),
        platform: Platform::LinuxX86_64,
        locked: false,
        offline: false,
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
    let plan = provider::for_manifest(&m).plan_apply(&ctx(&m)).unwrap();
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
    assert!(rendered.contains("pip dependencies are recorded"));

    let mut sink = Vec::new();
    plan.execute(true, &mut sink).unwrap();
    assert!(String::from_utf8(sink).unwrap().contains("dry-run"));
}

#[test]
fn native_build_plan_sources_underlays_first() {
    let m = Manifest::parse(FULL, Path::new("t")).unwrap();
    let plan = provider::for_manifest(&m)
        .plan_build(&ctx(&m), Some("main"))
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
    let script = provider::for_manifest(&m)
        .env_script(&ctx(&m), Shell::Zsh)
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
    let err = provider::for_manifest(&m).plan_apply(&ctx(&m)).unwrap_err();
    assert!(err.to_string().contains("P3"), "{err}");
}
