use super::*;
use crate::diagnostic::{parse_cpp, parse_python};
use std::path::Path;

struct Fixture {
    manifest: Manifest,
    packages: Vec<PackageXml>,
    deps: DepsReport,
    env: EnvReport,
    mappings: Vec<DependencyMapping>,
}

impl Fixture {
    fn new() -> Self {
        let manifest = Manifest::parse(
            include_str!("../../../tests/fixtures/minimal.toml"),
            Path::new("rospm.toml"),
        )
        .unwrap();
        let package = PackageXml::parse("<package><name>demo</name><depend>rclcpp</depend><exec_depend>python3-yaml</exec_depend></package>", Path::new("ws/src/demo/package.xml")).unwrap();
        let mut environment = EnvReport::default();
        environment.push(
            "deps",
            Message::text("rclcpp"),
            Message::text("ros-jazzy-rclcpp libextra"),
            Message::text("not installed"),
            Status::Missing,
        );
        environment.push(
            "deps",
            Message::text("python3-yaml"),
            Message::text("python3-yaml"),
            Message::text("not installed"),
            Status::Missing,
        );
        Self {
            manifest,
            packages: vec![package],
            deps: DepsReport {
                workspaces: vec![],
                internal: Default::default(),
                skipped: Default::default(),
                external: ["rclcpp".into(), "python3-yaml".into()].into(),
                environment,
            },
            env: EnvReport::default(),
            mappings: vec![DependencyMapping {
                code: Code::MissingHeader,
                name: "rclcpp/rclcpp.hpp".into(),
                target: DependencyTarget::RosdepKey("rclcpp".into()),
                source: "fixture package file index".into(),
            }],
        }
    }
    fn context(&self) -> DependencyContext<'_> {
        DependencyContext {
            manifest: &self.manifest,
            platform: Platform::LinuxX86_64,
            package: self.packages.first(),
            project_packages: &self.packages,
            deps: Some(&self.deps),
            environment: Some(&self.env),
            mappings: &self.mappings,
            system_python: true,
        }
    }
    fn header(&self) -> Diagnostic {
        let mut ds =
            parse_cpp("demo.cpp:1:1: fatal error: rclcpp/rclcpp.hpp: No such file or directory");
        suggest_dependencies(&mut ds, &self.context());
        ds.remove(0)
    }
}

fn has(d: &Diagnostic, key: &str) -> bool {
    d.suggestions.iter().any(|m| m.key_name() == Some(key))
}
fn installs(d: &Diagnostic) -> bool {
    has(d, "diagnostic.dependency_rosdep_missing") || has(d, "diagnostic.dependency_apt_missing")
}

#[test]
fn mapped_missing_dependency_gets_reviewable_restore_without_rewriting_facts() {
    let f = Fixture::new();
    let original =
        parse_cpp("demo.cpp:1:1: fatal error: rclcpp/rclcpp.hpp: No such file or directory")
            .remove(0);
    let mut d = f.header();
    assert!(installs(&d));
    assert!(has(&d, "diagnostic.dependency_mapping"));
    assert_eq!(d.raw, original.raw);
    assert_eq!(d.location, original.location);
    assert_eq!(d.code, original.code);
    assert_eq!(d.explanation, original.explanation);
    let message = d
        .suggestions
        .iter()
        .find(|m| m.key_name() == Some("diagnostic.dependency_rosdep_missing"))
        .unwrap();
    assert!(message
        .args()
        .contains(&("packages", "ros-jazzy-rclcpp libextra".into())));
    let once = d.clone();
    suggest_dependencies(std::slice::from_mut(&mut d), &f.context());
    assert_eq!(d, once);
}

#[test]
fn no_mapping_no_probe_unknown_or_installed_never_becomes_missing() {
    let mut f = Fixture::new();
    f.mappings[0].source.clear();
    assert!(!installs(&f.header()));
    f.mappings[0].source = "file index".into();
    for status in [Status::Unknown, Status::Mismatch, Status::Info, Status::Ok] {
        f.deps.environment.items[0].status = status;
        let d = f.header();
        assert!(!installs(&d));
        if status == Status::Ok {
            assert!(has(&d, "diagnostic.dependency_present"));
        }
    }
    f.deps.environment.items.clear();
    assert!(!installs(&f.header()));
}

#[test]
fn declaration_skip_disable_and_source_packages_take_precedence() {
    let mut f = Fixture::new();
    f.manifest
        .dependencies
        .rosdep
        .skip_keys
        .push("rclcpp".into());
    assert!(has(&f.header(), "diagnostic.dependency_skipped"));
    assert!(!installs(&f.header()));
    f.manifest.dependencies.rosdep.skip_keys.clear();
    f.manifest.dependencies.rosdep.enabled = false;
    assert!(has(&f.header(), "diagnostic.dependency_rosdep_disabled"));
    assert!(!installs(&f.header()));
    f.manifest.dependencies.rosdep.enabled = true;
    f.packages[0].depends[0].condition = Some("$ROS_VERSION == 1".into());
    assert!(has(&f.header(), "diagnostic.dependency_undeclared"));
    assert!(!installs(&f.header()));
    f.packages.push(
        PackageXml::parse(
            "<package><name>rclcpp</name></package>",
            Path::new("underlay/src/rclcpp/package.xml"),
        )
        .unwrap(),
    );
    assert!(has(&f.header(), "diagnostic.dependency_internal"));
    assert!(!installs(&f.header()));
}

#[test]
fn cmake_matching_declared_key_is_not_an_artifact_mapping() {
    let f = Fixture::new();
    let mut ds = parse_cpp("CMake Error at CMakeLists.txt:7 (find_package):\n  Could NOT find rclcpp (missing: rclcpp_DIR)\n");
    suggest_dependencies(&mut ds, &f.context());
    assert_eq!(ds.len(), 1);
    assert!(has(&ds[0], "diagnostic.dependency_check_unknown"));
    assert!(!installs(&ds[0]));
}

#[test]
fn apt_mapping_requires_manifest_and_native_check() {
    let mut f = Fixture::new();
    f.mappings[0].target = DependencyTarget::AptPackage("ros-jazzy-rclcpp".into());
    f.env.push(
        "apt",
        Message::text("ros-jazzy-rclcpp"),
        Message::text("installed"),
        Message::text("missing"),
        Status::Missing,
    );
    assert!(has(&f.header(), "diagnostic.dependency_apt_undeclared"));
    assert!(!installs(&f.header()));
    f.manifest.dependencies.apt.push("ros-jazzy-rclcpp".into());
    assert!(has(&f.header(), "diagnostic.dependency_apt_missing"));
    for provider in [ProviderKind::Pixi, ProviderKind::Docker] {
        f.manifest.environment.provider = provider;
        assert!(!installs(&f.header()));
        assert!(has(&f.header(), "diagnostic.dependency_provider"));
    }
}

#[test]
fn python_module_names_are_not_distribution_names_and_host_is_not_conda() {
    let mut f = Fixture::new();
    f.manifest.dependencies.pip.push("PyYAML".into());
    let parse = || {
        parse_python(
            "ModuleNotFoundError: No module named 'yaml'",
            Some("selected interpreter"),
        )
    };
    let mut ds = parse();
    suggest_dependencies(&mut ds, &f.context());
    assert!(!installs(&ds[0]));
    assert!(has(&ds[0], "diagnostic.dependency_pip_record_only"));
    f.mappings.push(DependencyMapping {
        code: Code::ModuleNotFound,
        name: "yaml".into(),
        target: DependencyTarget::RosdepKey("python3-yaml".into()),
        source: "PyYAML metadata".into(),
    });
    let mut ds = parse();
    suggest_dependencies(&mut ds, &f.context());
    assert!(installs(&ds[0]));
    let mut ds = parse();
    let mut ctx = f.context();
    ctx.system_python = false;
    suggest_dependencies(&mut ds, &ctx);
    assert!(!installs(&ds[0]));
    assert!(has(&ds[0], "diagnostic.dependency_python_scope"));
    f.manifest.build.conda = Some(Default::default());
    let mut ds = parse();
    suggest_dependencies(&mut ds, &f.context());
    assert!(!installs(&ds[0]));
}

#[test]
fn links_and_exception_chains_keep_their_meaning() {
    let f = Fixture::new();
    let mut ds = parse_cpp("/usr/bin/ld: a.o: undefined reference to `foo()'");
    let original = ds.clone();
    suggest_dependencies(&mut ds, &f.context());
    assert_eq!(ds, original);
    let mut ds = parse_python("ModuleNotFoundError: No module named 'yaml'\n\nDuring handling of the above exception, another exception occurred:\n\nTraceback (most recent call last):\n  File \"main.py\", line 3, in f\nRuntimeError: failed", None);
    suggest_dependencies(&mut ds, &f.context());
    assert_eq!(ds[0].code, Code::RuntimeError);
    assert!(has(&ds[0].related[0], "diagnostic.dependency_unconfirmed"));
    assert!(!installs(&ds[0]));
}

#[test]
fn cmake_and_library_mappings_use_the_same_evidence_gates() {
    let mut f = Fixture::new();
    f.mappings.push(DependencyMapping {
        code: Code::PackageNotFound,
        name: "rclcpp".into(),
        target: DependencyTarget::RosdepKey("rclcpp".into()),
        source: "fixture CMake config file index".into(),
    });
    let mut ds = parse_cpp("CMake Error at CMakeLists.txt:7 (find_package):\n  Could NOT find rclcpp (missing: rclcpp_DIR)\n");
    suggest_dependencies(&mut ds, &f.context());
    assert!(installs(&ds[0]));
    assert!(!has(&ds[0], "diagnostic.dependency_check_unknown"));
    f.mappings.push(DependencyMapping {
        code: Code::LibraryNotFound,
        name: "rclcpp".into(),
        target: DependencyTarget::RosdepKey("rclcpp".into()),
        source: "fixture linker library index".into(),
    });
    let mut ds = parse_cpp("/usr/bin/ld: cannot find -lrclcpp: No such file or directory");
    assert_eq!(ds.len(), 1);
    suggest_dependencies(&mut ds, &f.context());
    assert!(installs(&ds[0]));
}

#[test]
fn incomplete_or_conflicting_reports_do_not_authorize_install_advice() {
    let mut f = Fixture::new();
    let mut duplicate = f.deps.environment.items[0].clone();
    duplicate.status = Status::Ok;
    f.deps.environment.items.push(duplicate);
    assert!(!installs(&f.header()));
    f.deps.environment.items.pop();
    f.deps.external.clear();
    assert!(!installs(&f.header()));
    f.deps.external.insert("rclcpp".into());
    f.deps.environment.items[0].expected = msg!("deps.report.unresolved");
    assert!(!installs(&f.header()));
    let mut ds =
        parse_cpp("demo.cpp:1:1: fatal error: rclcpp/rclcpp.hpp: No such file or directory");
    let mut ctx = f.context();
    ctx.package = None;
    suggest_dependencies(&mut ds, &ctx);
    assert!(has(&ds[0], "diagnostic.dependency_undeclared"));
    assert!(!installs(&ds[0]));
}

#[test]
fn apt_advice_honors_target_replacements_and_exclusions() {
    let mut f = Fixture::new();
    let package = "ros-jazzy-rclcpp";
    f.mappings[0].target = DependencyTarget::AptPackage(package.into());
    f.env.push(
        "apt",
        Message::text(package),
        Message::text("installed"),
        Message::text("missing"),
        Status::Missing,
    );
    f.manifest.dependencies.apt.push("old-package".into());
    let mut target = crate::manifest::TargetOverride::default();
    target.replace.insert("old-package".into(), package.into());
    f.manifest.target.insert("linux-x86_64".into(), target);
    assert!(installs(&f.header()));
    f.manifest
        .target
        .get_mut("linux-x86_64")
        .unwrap()
        .exclude
        .apt
        .push(package.into());
    assert!(!installs(&f.header()));
}
