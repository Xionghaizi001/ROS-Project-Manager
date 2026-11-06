//! `rospm init --from-env`: a best-effort draft manifest (decision #3).
//!
//! Only facts that can be read reliably are written as values. Everything
//! else is left at a safe default and marked `# TODO` in the generated file,
//! and listed in [`Draft::todos`] so the CLI can show them. The output must
//! always pass [`Manifest::parse`]; a draft that does not is a bug here.
//!
//! What is inspected:
//! - the host: `$ROS_DISTRO`, `/opt/ros/*`, `/etc/os-release`, installed ROS
//!   metapackages, `$AMENT_PREFIX_PATH` (for the overlay order), ROS-related
//!   environment variables, `$COLCON_DEFAULTS_FILE`;
//! - the project directory: `*.repos` files, directories with a `src/`,
//!   `package.xml` files, `pixi.toml`, `.vscode/`, `.idea/`, `.devcontainer/`.
//!
//! Nothing here shells out except through [`PackageDb`].

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::Serialize;

use crate::manifest::{Manifest, ProviderKind, RESERVED_ENV};
use crate::message::Message;
use crate::msg;
use crate::package_xml::WorkspacePackages;
use crate::pixi::quote;
use crate::platform::{ubuntu_codename, Platform, RosDistro};
use crate::probe::{Host, OsRelease, PackageDb};
use crate::repos::ReposFile;

mod render;
mod workspaces;

use workspaces::detect_workspaces;
#[cfg(test)]
use workspaces::workspace_name;

/// Environment variables worth carrying into `[env]`. Anything derived from
/// `source setup.bash` (`ROS_DISTRO`, `AMENT_PREFIX_PATH`, `PYTHONPATH`, …) is
/// deliberately absent: those are regenerated, not declared.
pub const ROS_ENV_KEYS: &[&str] = &[
    "ROS_DOMAIN_ID",
    "RMW_IMPLEMENTATION",
    "ROS_LOCALHOST_ONLY",
    "ROS_AUTOMATIC_DISCOVERY_RANGE",
    "ROS_STATIC_PEERS",
    "ROS_DISCOVERY_SERVER",
    "ROS_SUPER_CLIENT",
    "CYCLONEDDS_URI",
    "FASTRTPS_DEFAULT_PROFILES_FILE",
    "FASTDDS_DEFAULT_PROFILES_FILE",
    "RMW_FASTRTPS_USE_QOS_FROM_XML",
    "ROS_LOG_DIR",
    "RCUTILS_COLORIZED_OUTPUT",
    "RCUTILS_CONSOLE_OUTPUT_FORMAT",
    "RCUTILS_LOGGING_USE_STDOUT",
    "RCUTILS_LOGGING_BUFFERED_STREAM",
    "ROS_SECURITY_ENABLE",
    "ROS_SECURITY_STRATEGY",
    "ROS_SECURITY_KEYSTORE",
    "ROS_SECURITY_ENCLAVE_OVERRIDE",
];

/// Directories never treated as (or searched for) workspaces.
const SKIP_DIRS: &[&str] = &[
    "build",
    "install",
    "log",
    "src",
    "target",
    "node_modules",
    ".rospm",
];

/// Directory names that say nothing about the workspace's role.
const GENERIC_WS_NAMES: &[&str] = &[
    "ws",
    "workspace",
    "colcon_ws",
    "ros2_ws",
    "ros_ws",
    "dev_ws",
];

// ---------------------------------------------------------------- inputs

/// Host facts relevant to drafting a manifest. Built from a [`Host`] plus the
/// process environment; tests construct it directly.
#[derive(Debug, Clone)]
pub struct HostFacts {
    pub platform: Option<Platform>,
    pub os: Option<OsRelease>,
    pub ros_distro_env: Option<String>,
    pub installed_distros: Vec<String>,
    /// `$AMENT_PREFIX_PATH`, split, topmost overlay first.
    pub ament_prefix_path: Vec<PathBuf>,
    /// Process environment filtered to [`ROS_ENV_KEYS`].
    pub env: BTreeMap<String, String>,
    pub colcon_defaults_file: Option<PathBuf>,
    pub packages: PackageDb,
}

impl HostFacts {
    pub fn gather(host: &Host) -> HostFacts {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        HostFacts {
            platform: host.platform,
            os: host.os.clone(),
            ros_distro_env: var("ROS_DISTRO"),
            installed_distros: host.ros_distros.clone(),
            ament_prefix_path: var("AMENT_PREFIX_PATH")
                .map(|v| {
                    v.split(':')
                        .filter(|s| !s.is_empty())
                        .map(PathBuf::from)
                        .collect()
                })
                .unwrap_or_default(),
            env: ROS_ENV_KEYS
                .iter()
                .filter_map(|k| var(k).map(|v| (k.to_string(), v)))
                .collect(),
            colcon_defaults_file: var("COLCON_DEFAULTS_FILE").map(PathBuf::from),
            packages: host.packages.clone(),
        }
    }

    /// A host that knows nothing; tests fill in fields.
    pub fn empty() -> HostFacts {
        HostFacts {
            platform: None,
            os: None,
            ros_distro_env: None,
            installed_distros: Vec::new(),
            ament_prefix_path: Vec::new(),
            env: BTreeMap::new(),
            colcon_defaults_file: None,
            packages: PackageDb::Unavailable,
        }
    }
}

// ---------------------------------------------------------------- output

#[derive(Debug, Clone, Serialize)]
pub struct DraftWorkspace {
    pub name: String,
    /// Relative to the project root.
    pub path: PathBuf,
    /// Relative to the project root.
    pub sources: Option<PathBuf>,
    pub underlays: Vec<String>,
    pub colcon_defaults: Option<PathBuf>,
    pub packages: usize,
    /// How this workspace was found (shown as a comment).
    pub origin: Message,
}

#[derive(Debug, Clone, Serialize)]
pub struct Draft {
    pub name: String,
    pub ros_distro: Option<RosDistro>,
    pub platforms: Vec<Platform>,
    pub provider: ProviderKind,
    pub pixi_manifest: Option<PathBuf>,
    pub ubuntu: Option<String>,
    pub ros_base: Option<String>,
    pub workspaces: Vec<DraftWorkspace>,
    /// rosdep keys referenced by `package.xml` files that no workspace package provides.
    pub external_keys: Vec<String>,
    pub env: IndexMap<String, String>,
    pub vscode: bool,
    pub clion: bool,
    pub devcontainer: bool,
    /// Fields that could not be detected, with the reason. Keys are namespaced
    /// by manifest field (`init.ros_base.*`, `init.workspaces.*`, …).
    pub todos: Vec<Message>,
    /// Facts that were detected, with their source.
    pub notes: Vec<Message>,
}

/// Build a draft for the project in `root`.
pub fn draft(root: &Path, facts: &HostFacts) -> Draft {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut d = Draft {
        name: root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "my_project".into()),
        ros_distro: None,
        platforms: Vec::new(),
        provider: ProviderKind::Native,
        pixi_manifest: None,
        ubuntu: None,
        ros_base: None,
        workspaces: Vec::new(),
        external_keys: Vec::new(),
        env: IndexMap::new(),
        vscode: root.join(".vscode").is_dir(),
        clion: root.join(".idea").is_dir(),
        devcontainer: root.join(".devcontainer").is_dir()
            || root.join(".devcontainer.json").is_file(),
        todos: Vec::new(),
        notes: Vec::new(),
    };

    detect_platform(&mut d, facts);
    detect_provider(&mut d, &root);
    detect_distro(&mut d, facts);
    detect_os(&mut d, facts);
    detect_ros_base(&mut d, facts);
    detect_workspaces(&mut d, &root, facts);
    detect_env(&mut d, facts);
    d
}

fn detect_platform(d: &mut Draft, facts: &HostFacts) {
    match facts.platform {
        Some(p) => {
            d.platforms.push(p);
            d.notes.push(msg!("init.platforms.host", platform = p));
        }
        None => {
            d.platforms.push(Platform::LinuxX86_64);
            d.todos.push(msg!("init.platforms.unsupported"));
        }
    }
}

fn detect_provider(d: &mut Draft, root: &Path) {
    let pixi = root.join("pixi.toml");
    if pixi.is_file() {
        d.provider = ProviderKind::Pixi;
        d.pixi_manifest = Some(PathBuf::from("pixi.toml"));
        d.notes.push(msg!("init.provider.pixi"));
        if let Some(distro) = fs::read_to_string(&pixi)
            .ok()
            .and_then(|t| distro_from_pixi(&t))
        {
            d.ros_distro = Some(distro);
            d.notes
                .push(msg!("init.ros_distro.from_pixi", distro = distro));
        }
    }
}

/// `robostack-<distro>` channel or `ros-<distro>-*` dependency in a pixi.toml.
fn distro_from_pixi(text: &str) -> Option<RosDistro> {
    let value: toml::Value = text.parse().ok()?;
    let mut hits: Vec<RosDistro> = Vec::new();
    let channels = value
        .get("project")
        .or_else(|| value.get("workspace"))
        .and_then(|p| p.get("channels"))
        .and_then(|c| c.as_array());
    if let Some(channels) = channels {
        for c in channels.iter().filter_map(|c| c.as_str()) {
            for distro in RosDistro::ALL {
                if c.contains(&format!("robostack-{distro}")) {
                    hits.push(distro);
                }
            }
        }
    }
    if let Some(deps) = value.get("dependencies").and_then(|d| d.as_table()) {
        for key in deps.keys() {
            for distro in RosDistro::ALL {
                if key.starts_with(&format!("ros-{distro}-")) {
                    hits.push(distro);
                }
            }
        }
    }
    hits.first().copied()
}

fn detect_distro(d: &mut Draft, facts: &HostFacts) {
    if d.ros_distro.is_some() {
        return;
    }
    if let Some(env) = &facts.ros_distro_env {
        match env.parse::<RosDistro>() {
            Ok(distro) => {
                d.ros_distro = Some(distro);
                d.notes
                    .push(msg!("init.ros_distro.from_env", distro = distro));
                return;
            }
            Err(_) => d
                .todos
                .push(msg!("init.ros_distro.env_unsupported", value = env)),
        }
    }
    // A sourced install shows up in AMENT_PREFIX_PATH as /opt/ros/<distro>.
    for entry in &facts.ament_prefix_path {
        if let Ok(rest) = entry.strip_prefix("/opt/ros") {
            if let Some(Ok(distro)) = rest
                .components()
                .next()
                .map(|c| c.as_os_str().to_string_lossy().parse::<RosDistro>())
            {
                d.ros_distro = Some(distro);
                d.notes
                    .push(msg!("init.ros_distro.from_ament", distro = distro));
                return;
            }
        }
    }
    let supported: Vec<RosDistro> = facts
        .installed_distros
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect();
    match supported.as_slice() {
        [one] => {
            d.ros_distro = Some(*one);
            d.notes
                .push(msg!("init.ros_distro.only_install", distro = one));
        }
        [] => {
            // A clean host has no ROS installation to inspect. Ubuntu's ROS
            // release matrix still gives us a reliable distro choice.
            let from_ubuntu = facts.os.as_ref().and_then(|os| {
                if !os.is_ubuntu() {
                    return None;
                }
                match os.version_id.as_deref() {
                    Some("22.04") => Some(RosDistro::Humble),
                    Some("24.04") => Some(RosDistro::Jazzy),
                    _ => None,
                }
            });
            if let Some(distro) = from_ubuntu {
                d.ros_distro = Some(distro);
                d.notes
                    .push(msg!("init.ros_distro.from_ubuntu", distro = distro));
            } else {
                d.todos.push(msg!("init.ros_distro.none"));
            }
        }
        many => d.todos.push(msg!(
            "init.ros_distro.several",
            distros = many
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            default = many[0]
        )),
    }
    if d.ros_distro.is_none() {
        d.ros_distro = supported.first().copied();
    }
}

fn detect_os(d: &mut Draft, facts: &HostFacts) {
    if d.provider != ProviderKind::Native {
        return;
    }
    match &facts.os {
        Some(os) if os.is_ubuntu() => match &os.version_id {
            Some(v) => {
                if ubuntu_codename(v).is_none() {
                    d.todos
                        .push(msg!("init.ubuntu.unsupported_release", version = v));
                }
                d.ubuntu = Some(v.clone());
                d.notes
                    .push(msg!("init.ubuntu.from_os_release", version = v));
            }
            None => d.todos.push(msg!("init.ubuntu.no_version_id")),
        },
        Some(os) => d.todos.push(msg!(
            "init.ubuntu.not_ubuntu",
            os = os
                .pretty_name
                .clone()
                .unwrap_or_else(|| msg!("native.unknown_os").to_string())
        )),
        None => d.todos.push(msg!("init.ubuntu.os_release_unreadable")),
    }
}

fn detect_ros_base(d: &mut Draft, facts: &HostFacts) {
    if d.provider != ProviderKind::Native {
        return;
    }
    let Some(distro) = d.ros_distro else {
        d.todos.push(msg!("init.ros_base.needs_distro"));
        return;
    };
    let candidates: Vec<String> = ["desktop-full", "desktop", "ros-base"]
        .iter()
        .map(|suffix| format!("ros-{distro}-{suffix}"))
        .collect();
    match facts.packages.status(&candidates) {
        None => d.todos.push(msg!("init.ros_base.dpkg_unavailable")),
        Some(status) => match candidates.iter().find(|c| status[*c].installed) {
            Some(pkg) => {
                d.ros_base = Some(pkg.clone());
                d.notes.push(msg!("init.ros_base.installed", package = pkg));
            }
            None if facts.installed_distros.iter().any(|x| x == distro.as_str()) => {
                d.todos.push(msg!(
                    "init.ros_base.no_metapackage",
                    distro = distro,
                    candidates = candidates.join(", ")
                ))
            }
            None => d
                .todos
                .push(msg!("init.ros_base.distro_not_installed", distro = distro)),
        },
    }
}

fn detect_env(d: &mut Draft, facts: &HostFacts) {
    for (k, v) in &facts.env {
        if RESERVED_ENV.contains(&k.as_str()) {
            continue;
        }
        d.env.insert(k.clone(), v.clone());
    }
    if !d.env.is_empty() {
        d.notes.push(msg!(
            "init.env.from_environment",
            keys = d.env.keys().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rospm-introspect-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn pkg(dir: &Path, name: &str, deps: &[&str]) {
        fs::create_dir_all(dir).unwrap();
        let deps: String = deps
            .iter()
            .map(|d| format!("<depend>{d}</depend>"))
            .collect();
        fs::write(
            dir.join("package.xml"),
            format!("<package format=\"3\"><name>{name}</name>{deps}</package>"),
        )
        .unwrap();
    }

    #[test]
    fn empty_directory_yields_a_valid_placeholder() {
        let root = tmp("empty");
        let d = draft(&root, &HostFacts::empty());
        assert_eq!(d.workspaces.len(), 1);
        assert!(d.ros_distro.is_none());
        assert!(d.todos.iter().any(|t| t.has_key_prefix("init.ros_distro.")));
        assert!(d
            .todos
            .iter()
            .any(|t| t.key_name() == Some("init.workspaces.placeholder")));
        let text = d.render();
        assert!(text.contains("TODO"), "{text}");
        d.validate().expect("draft must always validate");
    }

    #[test]
    fn flat_workspaces_include_packages_without_inventing_sources() {
        for rel in [".", "robot_ws", "projects/robot_ws"] {
            let root = tmp("flat");
            let ws = root.join(rel);
            pkg(&ws.join("gui_control"), "gui_control", &["rclpy"]);
            pkg(&ws.join("sdk"), "sdk", &["gui_control", "rclcpp"]);
            // A C++ package's src directory is not a separate workspace.
            fs::create_dir_all(ws.join("sdk/src")).unwrap();
            for ignored in ["build", "install", "ignored"] {
                pkg(&ws.join(ignored).join("old"), "old", &["stale_dep"]);
                fs::write(ws.join(ignored).join("COLCON_IGNORE"), "").unwrap();
            }
            let d = draft(&root, &HostFacts::empty());
            assert_eq!(d.workspaces.len(), 1, "{d:#?}");
            assert_eq!(d.workspaces[0].path, Path::new(rel));
            assert_eq!(d.workspaces[0].packages, 2);
            assert!(d.workspaces[0].sources.is_none());
            assert_eq!(d.external_keys, ["rclcpp", "rclpy"]);
            assert!(!d
                .todos
                .iter()
                .any(|t| t.key_name() == Some("init.workspaces.placeholder")));
            d.validate().unwrap();
        }
    }

    #[test]
    fn workspaces_repos_and_overlay_order_are_inferred() {
        let root = tmp("full");
        // deps_ws: imported from deps.repos
        pkg(
            &root.join("deps_ws/src/ros2/examples/pkg_a"),
            "pkg_a",
            &["rclcpp"],
        );
        fs::create_dir_all(root.join("deps_ws/install/pkg_a")).unwrap();
        fs::write(root.join("deps_ws/install/setup.bash"), "").unwrap();
        fs::write(
            root.join("deps.repos"),
            "repositories:\n  ros2/examples:\n    type: git\n    url: https://github.com/ros2/examples.git\n    version: jazzy\n",
        )
        .unwrap();
        // ws: local sources, depends on pkg_a from the underlay
        pkg(
            &root.join("ws/src/my_robot"),
            "my_robot",
            &["pkg_a", "nav2_bringup"],
        );
        fs::create_dir_all(root.join("ws/install/my_robot")).unwrap();
        // two .repos nobody imported: too ambiguous to attach to `ws`, so each
        // becomes a workspace of its own
        for name in ["extra.repos", "other.repos"] {
            fs::write(
                root.join(name),
                "repositories:\n  foo/bar:\n    type: git\n    url: https://example.com/bar.git\n",
            )
            .unwrap();
        }
        fs::create_dir_all(root.join(".vscode")).unwrap();

        let mut facts = HostFacts::empty();
        facts.platform = Some(Platform::LinuxX86_64);
        facts.ros_distro_env = Some("humble".into());
        facts.os = Some(OsRelease::parse("ID=ubuntu\nVERSION_ID=\"22.04\"\n"));
        facts.installed_distros = vec!["humble".into()];
        facts.ament_prefix_path = vec![
            root.join("ws/install/my_robot"),
            root.join("deps_ws/install/pkg_a"),
            PathBuf::from("/opt/ros/humble"),
        ];
        facts.env.insert("ROS_DOMAIN_ID".into(), "7".into());
        facts
            .env
            .insert("AMENT_PREFIX_PATH".into(), "ignored".into());
        let mut db = BTreeMap::new();
        db.insert(
            "ros-humble-desktop".to_string(),
            crate::probe::PackageStatus {
                installed: true,
                version: Some("0.10.0-1".into()),
            },
        );
        facts.packages = PackageDb::Static(db);

        let d = draft(&root, &facts);
        assert_eq!(d.ros_distro, Some(RosDistro::Humble));
        assert_eq!(d.ubuntu.as_deref(), Some("22.04"));
        assert_eq!(d.ros_base.as_deref(), Some("ros-humble-desktop"));
        assert!(d.vscode && !d.clion);
        assert_eq!(d.env.get("ROS_DOMAIN_ID").map(String::as_str), Some("7"));
        assert!(!d.env.contains_key("AMENT_PREFIX_PATH"));

        let names: Vec<&str> = d.workspaces.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["deps", "main", "extra", "other"], "{d:#?}");
        let deps = &d.workspaces[0];
        assert_eq!(deps.sources.as_deref(), Some(Path::new("deps.repos")));
        assert!(deps.underlays.is_empty());
        let main = &d.workspaces[1];
        assert_eq!(main.path, PathBuf::from("ws"));
        assert!(main.sources.is_none());
        assert_eq!(main.underlays, vec!["deps"]);
        let extra = &d.workspaces[2];
        assert_eq!(extra.path, PathBuf::from("extra_ws"));
        assert!(d
            .todos
            .iter()
            .any(|t| t.to_string().contains("extra.repos")));
        assert!(d
            .todos
            .iter()
            .any(|t| t.to_string().contains("other.repos")));

        // pkg_a is provided by the deps workspace, so only external keys remain.
        assert_eq!(d.external_keys, vec!["nav2_bringup", "rclcpp"]);

        let m = d.validate().expect("draft must validate");
        assert_eq!(
            m.build_order().unwrap(),
            vec!["deps", "extra", "other", "main"]
        );
        assert_eq!(m.ros_base_package(), "ros-humble-desktop");
        assert!(m.editor.vscode);
    }

    #[test]
    fn fresh_clone_pairs_single_repos_with_the_only_sourceless_workspace() {
        let root = tmp("fresh");
        pkg(&root.join("ws/src/local_pkg"), "local_pkg", &["rclcpp"]);
        fs::write(
            root.join("main.repos"),
            "repositories:\n  ros2/demos:\n    type: git\n    url: https://github.com/ros2/demos.git\n",
        )
        .unwrap();
        let d = draft(&root, &HostFacts::empty());
        assert_eq!(d.workspaces.len(), 1);
        assert_eq!(d.workspaces[0].name, "main");
        assert_eq!(d.workspaces[0].path, PathBuf::from("ws"));
        assert_eq!(
            d.workspaces[0].sources.as_deref(),
            Some(Path::new("main.repos"))
        );
        d.validate().unwrap();
    }

    #[test]
    fn repos_without_repositories_is_ignored() {
        let root = tmp("emptyrepos");
        fs::write(
            root.join("all_commented.repos"),
            "repositories:\n  # a: b\n",
        )
        .unwrap();
        pkg(&root.join("src/x"), "x", &[]);
        let d = draft(&root, &HostFacts::empty());
        assert_eq!(d.workspaces.len(), 1);
        assert_eq!(d.workspaces[0].path, PathBuf::from("."));
        assert!(d.workspaces[0].sources.is_none());
        assert!(d
            .notes
            .iter()
            .any(|n| n.key_name() == Some("init.workspaces.repos_empty")));
        d.validate().unwrap();
    }

    #[test]
    fn pixi_manifest_switches_provider_and_gives_distro() {
        let root = tmp("pixi");
        fs::write(
            root.join("pixi.toml"),
            "[project]\nname = \"x\"\nchannels = [\"https://prefix.dev/robostack-jazzy\", \"conda-forge\"]\nplatforms = [\"linux-64\"]\n[dependencies]\nros-jazzy-ros-base = \"*\"\n",
        )
        .unwrap();
        let d = draft(&root, &HostFacts::empty());
        assert_eq!(d.provider, ProviderKind::Pixi);
        assert_eq!(d.ros_distro, Some(RosDistro::Jazzy));
        assert!(d.ubuntu.is_none());
        let text = d.render();
        assert!(text.contains("[environment.pixi]"), "{text}");
        assert!(!text.contains("[environment.native]"));
        d.validate().unwrap();
    }

    #[test]
    fn several_installs_without_ros_distro_is_a_todo() {
        let root = tmp("multi");
        let mut facts = HostFacts::empty();
        facts.installed_distros = vec!["humble".into(), "jazzy".into()];
        let d = draft(&root, &facts);
        assert_eq!(d.ros_distro, Some(RosDistro::Humble));
        assert!(d
            .todos
            .iter()
            .any(|t| t.key_name() == Some("init.ros_distro.several")));
        d.validate().unwrap();
    }

    #[test]
    fn clean_ubuntu_host_uses_matching_ros_release() {
        let root = tmp("ubuntu-release");
        let mut facts = HostFacts::empty();
        facts.os = Some(OsRelease::parse("ID=ubuntu\nVERSION_ID=\"22.04\"\n"));
        let d = draft(&root, &facts);
        assert_eq!(d.ros_distro, Some(RosDistro::Humble));
        assert!(d
            .notes
            .iter()
            .any(|n| n.key_name() == Some("init.ros_distro.from_ubuntu")));
        assert!(!d
            .todos
            .iter()
            .any(|t| t.key_name() == Some("init.ros_distro.none")));
        d.validate().unwrap();
    }

    #[test]
    fn workspace_names_are_derived_from_directories() {
        assert_eq!(workspace_name(Path::new("."), None), "main");
        assert_eq!(
            workspace_name(Path::new("ws"), Some(Path::new("deps.repos"))),
            "deps"
        );
        assert_eq!(workspace_name(Path::new("nav_ws"), None), "nav");
        assert_eq!(workspace_name(Path::new("my robot"), None), "my_robot");
    }
}
