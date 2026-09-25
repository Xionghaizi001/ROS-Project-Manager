use std::path::Path;

use rospm_core::manifest::{Manifest, ProviderKind};
use rospm_core::platform::{Platform, RosDistro};
use rospm_core::Error;

const FULL: &str = include_str!("fixtures/full.toml");
const MINIMAL: &str = include_str!("fixtures/minimal.toml");

fn parse(text: &str) -> Result<Manifest, Error> {
    Manifest::parse(text, Path::new("test.toml"))
}

/// Validation messages in developer form (`key(arg=value, …)`): the core
/// library never renders a language, the CLI does (spec §4.2).
fn validation_errors(text: &str) -> Vec<String> {
    match parse(text) {
        Err(Error::Validation(list)) => list.iter().map(|m| m.developer_form()).collect(),
        other => panic!("expected validation error, got {other:?}"),
    }
}

#[test]
fn spec_example_parses() {
    let m = parse(FULL).expect("spec example must be valid");
    assert_eq!(m.project.ros_distro, RosDistro::Jazzy);
    assert_eq!(m.environment.provider, ProviderKind::Native);
    assert_eq!(m.workspaces.len(), 2);
    assert_eq!(m.env["ROS_DOMAIN_ID"], "42");
}

#[test]
fn minimal_example_parses() {
    let m = parse(MINIMAL).expect("minimal example must be valid");
    assert_eq!(m.build_order().unwrap(), vec!["main"]);
    assert_eq!(m.ros_base_package(), "ros-jazzy-ros-base");
    assert_eq!(m.ubuntu(), "24.04");
}

#[test]
fn build_order_puts_underlays_first() {
    let m = parse(FULL).unwrap();
    assert_eq!(m.build_order().unwrap(), vec!["deps", "main"]);
    assert_eq!(m.transitive_underlays("main").unwrap(), vec!["deps"]);
    assert!(m.transitive_underlays("deps").unwrap().is_empty());
}

#[test]
fn build_order_is_stable_and_transitive() {
    let text = r#"
schema_version = 1
[project]
name = "x"
ros_distro = "humble"
platforms = ["linux-x86_64"]
[environment]
provider = "native"
[workspaces.c]
path = "c"
underlays = ["b"]
[workspaces.a]
path = "a"
[workspaces.b]
path = "b"
underlays = ["a"]
[workspaces.d]
path = "d"
"#;
    let m = parse(text).unwrap();
    assert_eq!(m.build_order().unwrap(), vec!["a", "d", "b", "c"]);
    assert_eq!(m.transitive_underlays("c").unwrap(), vec!["a", "b"]);
}

#[test]
fn cycle_is_rejected() {
    let text = r#"
schema_version = 1
[project]
name = "x"
ros_distro = "humble"
platforms = ["linux-x86_64"]
[environment]
provider = "native"
[workspaces.a]
path = "a"
underlays = ["b"]
[workspaces.b]
path = "b"
underlays = ["a"]
"#;
    let errs = validation_errors(text);
    assert!(
        errs.iter()
            .any(|e| e.starts_with("manifest.underlay_cycle(")),
        "{errs:?}"
    );
}

#[test]
fn unknown_underlay_is_rejected() {
    let text = r#"
schema_version = 1
[project]
name = "x"
ros_distro = "humble"
platforms = ["linux-x86_64"]
[environment]
provider = "native"
[workspaces.a]
path = "a"
underlays = ["nope"]
"#;
    let errs = validation_errors(text);
    assert!(
        errs.iter()
            .any(|e| e == "manifest.unknown_underlay(name=a, underlay=nope)"),
        "{errs:?}"
    );
}

#[test]
fn provider_table_must_match_provider() {
    let text = r#"
schema_version = 1
[project]
name = "x"
ros_distro = "humble"
platforms = ["linux-x86_64"]
[environment]
provider = "pixi"
[environment.native]
ubuntu = "22.04"
[workspaces.a]
path = "a"
"#;
    let errs = validation_errors(text);
    assert!(
        errs.iter()
            .any(|e| e == "manifest.provider_table_mismatch(name=native, provider=pixi)"),
        "{errs:?}"
    );
    assert!(
        errs.iter()
            .any(|e| e == "manifest.provider_table_required(provider=pixi)"),
        "{errs:?}"
    );
}

#[test]
fn unknown_fields_are_rejected() {
    let text = MINIMAL.replace("[env]", "[env]\n[project.extra]\nfoo = 1\n[env2]");
    assert!(matches!(parse(&text), Err(Error::ManifestParse { .. })));
}

#[test]
fn reserved_env_is_rejected() {
    let text = format!("{MINIMAL}\nAMENT_PREFIX_PATH = \"/x\"\n");
    let errs = validation_errors(&text);
    assert!(
        errs.iter()
            .any(|e| e == "manifest.env_reserved(key=AMENT_PREFIX_PATH)"),
        "{errs:?}"
    );
}

#[test]
fn target_platform_must_be_declared() {
    let text = format!("{MINIMAL}\n[target.linux-aarch64.exclude]\napt = [\"x\"]\n");
    let errs = validation_errors(&text);
    assert!(
        errs.iter()
            .any(|e| e == "manifest.target_not_listed(key=linux-aarch64)"),
        "{errs:?}"
    );

    let text = format!("{MINIMAL}\n[target.windows.exclude]\napt = [\"x\"]\n");
    let errs = validation_errors(&text);
    assert!(
        errs.iter()
            .any(|e| e == "manifest.target_unknown_platform(key=windows)"),
        "{errs:?}"
    );
}

#[test]
fn effective_apt_applies_append_replace_exclude() {
    let m = parse(FULL).unwrap();
    assert_eq!(m.effective_apt(Platform::LinuxX86_64), vec!["libfoo-dev"]);
    // aarch64: append libbar-arm-dev; replace/exclude entries target packages
    // that are not in the base list, so they are no-ops here.
    assert_eq!(
        m.effective_apt(Platform::LinuxAarch64),
        vec!["libfoo-dev", "libbar-arm-dev"]
    );

    let text = format!(
        "{}\n[target.linux-aarch64.replace]\n\"libfoo-dev\" = \"libfoo-arm-dev\"\n[target.linux-aarch64.exclude]\napt = [\"libbar-arm-dev\"]\n",
        FULL.replace("[target.linux-aarch64.replace]\n\"nvidia-cuda-toolkit\" = \"cuda-toolkit-12-6\"\n", "")
            .replace("[target.linux-aarch64.exclude]\napt = [\"intel-mkl\"]\n", "")
    );
    let m = parse(&text).unwrap();
    assert_eq!(
        m.effective_apt(Platform::LinuxAarch64),
        vec!["libfoo-arm-dev"]
    );
}

#[test]
fn wrong_schema_version_is_rejected() {
    let text = MINIMAL.replace("schema_version = 1", "schema_version = 99");
    assert!(matches!(
        parse(&text),
        Err(Error::SchemaVersion { found: 99, .. })
    ));
}

#[test]
fn manifest_round_trips_through_toml() {
    let m = parse(FULL).unwrap();
    let text = m.to_toml().unwrap();
    let again = parse(&text).unwrap();
    assert_eq!(again.build_order().unwrap(), m.build_order().unwrap());
    assert_eq!(
        again.effective_apt(Platform::LinuxAarch64),
        m.effective_apt(Platform::LinuxAarch64)
    );
}
