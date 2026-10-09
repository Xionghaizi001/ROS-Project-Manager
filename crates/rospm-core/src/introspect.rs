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
use crate::platform::{ubuntu_codename, Platform, RosDistro};
use crate::probe::{Host, OsRelease, PackageDb};
use crate::repos::ReposFile;

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

// ---------------------------------------------------------------- workspaces

struct WsCandidate {
    dir: PathBuf,
    rel: PathBuf,
    packages: WorkspacePackages,
    built: bool,
    sources: Option<PathBuf>,
}

fn list_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.path())
                .filter(|p| {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_str())
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// `root` itself and its subdirectories (two levels) that hold a `src/`.
fn find_workspace_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if root.join("src").is_dir() {
        out.push(root.to_path_buf());
    }
    for level1 in list_dirs(root) {
        if level1.join("src").is_dir() {
            out.push(level1);
            continue;
        }
        for level2 in list_dirs(&level1) {
            if level2.join("src").is_dir() {
                out.push(level2);
            }
        }
    }
    out
}

/// `*.repos` files in `root` and two levels below (never inside `src/`).
fn find_repos_files(root: &Path) -> Vec<PathBuf> {
    fn files_in(dir: &Path) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_file() && p.extension().map(|e| e == "repos").unwrap_or(false))
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }
    let mut out = files_in(root);
    for level1 in list_dirs(root) {
        out.extend(files_in(&level1));
        for level2 in list_dirs(&level1) {
            out.extend(files_in(&level2));
        }
    }
    out
}

fn workspace_name(rel: &Path, sources: Option<&Path>) -> String {
    let dir_name = rel
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = |p: &Path| p.file_stem().map(|s| s.to_string_lossy().into_owned());
    let mut name = if dir_name.is_empty()
        || rel == Path::new(".")
        || GENERIC_WS_NAMES.contains(&dir_name.as_str())
    {
        sources.and_then(stem).unwrap_or_else(|| "main".into())
    } else {
        dir_name
            .strip_suffix("_ws")
            .or_else(|| dir_name.strip_suffix("-ws"))
            .unwrap_or(&dir_name)
            .to_string()
    };
    name = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() {
        name = "main".into();
    }
    name
}

fn detect_workspaces(d: &mut Draft, root: &Path, facts: &HostFacts) {
    let mut candidates: Vec<WsCandidate> = find_workspace_dirs(root)
        .into_iter()
        .map(|dir| {
            let rel = dir
                .strip_prefix(root)
                .map(Path::to_path_buf)
                .unwrap_or_default();
            let rel = if rel.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                rel
            };
            WsCandidate {
                packages: WorkspacePackages::scan(&dir.join("src")),
                built: dir.join("install").join("setup.bash").is_file(),
                dir,
                rel,
                sources: None,
            }
        })
        .collect();

    // Match every .repos file to the workspace whose src/ holds its checkouts.
    let mut unmatched_repos: Vec<(PathBuf, ReposFile)> = Vec::new();
    for file in find_repos_files(root) {
        let rel = file
            .strip_prefix(root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| file.clone());
        let repos = match ReposFile::load(&file) {
            Ok(r) => r,
            Err(e) => {
                d.todos.push(msg!(
                    "init.workspaces.repos_unparseable",
                    file = rel.display(),
                    error = e
                ));
                continue;
            }
        };
        if repos.repositories.is_empty() {
            d.notes
                .push(msg!("init.workspaces.repos_empty", file = rel.display()));
            continue;
        }
        let best = candidates
            .iter_mut()
            .map(|c| {
                let hits = repos
                    .repositories
                    .keys()
                    .filter(|r| c.dir.join("src").join(r).is_dir())
                    .count();
                (hits, c)
            })
            .filter(|(hits, _)| *hits > 0)
            .max_by_key(|(hits, _)| *hits);
        match best {
            Some((hits, c)) if c.sources.is_none() => {
                d.notes.push(msg!(
                    "init.workspaces.repos_matched",
                    file = rel.display(),
                    workspace = c.rel.display(),
                    hits = hits,
                    total = repos.repositories.len()
                ));
                c.sources = Some(rel);
            }
            Some((_, c)) => d.todos.push(msg!(
                "init.workspaces.repos_conflict",
                file = rel.display(),
                workspace = c.rel.display(),
                existing = c
                    .sources
                    .as_ref()
                    .map(|s| s.display().to_string())
                    .unwrap_or_default()
            )),
            None => unmatched_repos.push((rel, repos)),
        }
    }

    // One leftover .repos and one workspace without a sources file: almost
    // certainly a checkout before `vcs import` (the workspace may already
    // hold local packages next to where the imports will land).
    if unmatched_repos.len() == 1 {
        let without_sources: Vec<usize> = candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| c.sources.is_none())
            .map(|(i, _)| i)
            .collect();
        if let [i] = without_sources.as_slice() {
            let (rel, _) = unmatched_repos.remove(0);
            d.notes.push(msg!(
                "init.workspaces.repos_assigned",
                file = rel.display(),
                workspace = candidates[*i].rel.display()
            ));
            candidates[*i].sources = Some(rel);
        }
    }

    // Overlay order from AMENT_PREFIX_PATH (topmost first). colcon's default
    // isolated install yields `<ws>/install/<pkg>`; merged installs `<ws>/install`.
    let mut sourced: Vec<usize> = Vec::new();
    for entry in &facts.ament_prefix_path {
        let entry = entry.canonicalize().unwrap_or_else(|_| entry.clone());
        for (i, c) in candidates.iter().enumerate() {
            if entry.starts_with(c.dir.join("install")) && !sourced.contains(&i) {
                sourced.push(i);
            }
        }
    }

    let mut names: Vec<String> = Vec::new();
    for c in &candidates {
        let mut name = workspace_name(&c.rel, c.sources.as_deref());
        let base = name.clone();
        let mut n = 2;
        while names.contains(&name) {
            name = format!("{base}{n}");
            n += 1;
        }
        names.push(name);
    }

    let mut underlays: Vec<Vec<String>> = vec![Vec::new(); candidates.len()];
    if sourced.len() >= 2 {
        // bottom-up: the last entry in AMENT_PREFIX_PATH is the lowest overlay.
        for pair in sourced.iter().rev().collect::<Vec<_>>().windows(2) {
            underlays[*pair[1]].push(names[*pair[0]].clone());
        }
        d.notes.push(msg!(
            "init.underlays.from_ament",
            order = sourced
                .iter()
                .rev()
                .map(|i| names[*i].as_str())
                .collect::<Vec<_>>()
                .join(" -> ")
        ));
    } else if candidates.len() >= 2 {
        d.todos.push(msg!("init.underlays.unknown_order"));
    }

    let mut colcon_defaults: Option<PathBuf> = None;
    if let Some(f) = &facts.colcon_defaults_file {
        match f.canonicalize() {
            Err(_) => d
                .todos
                .push(msg!("init.colcon_defaults.missing", file = f.display())),
            Ok(abs) => match abs.strip_prefix(root) {
                Ok(rel) => {
                    d.notes
                        .push(msg!("init.colcon_defaults.from_env", file = rel.display()));
                    colcon_defaults = Some(rel.to_path_buf());
                }
                Err(_) => d.todos.push(msg!(
                    "init.colcon_defaults.outside_project",
                    file = f.display()
                )),
            },
        }
    }

    let mut all_keys: BTreeSet<String> = BTreeSet::new();
    let mut all_names: BTreeSet<String> = BTreeSet::new();
    for c in &candidates {
        all_names.extend(c.packages.names().iter().map(|s| s.to_string()));
    }
    for (i, c) in candidates.iter().enumerate() {
        let n = c.packages.packages.len();
        let origin = msg!(
            "init.origin.found",
            dir = c.rel.display(),
            packages = if n == 1 {
                msg!("init.origin.one_package").to_string()
            } else {
                msg!("init.origin.n_packages", count = n).to_string()
            },
            built = if c.built {
                msg!("init.origin.built").to_string()
            } else {
                String::new()
            },
            repos = if c.sources.is_some() {
                String::new()
            } else {
                msg!("init.origin.no_repos").to_string()
            }
        );
        for (file, err) in &c.packages.errors {
            d.todos.push(msg!(
                "init.workspaces.invalid_package_xml",
                workspace = names[i],
                file = file.display(),
                error = err
            ));
        }
        for key in c.packages.external_keys().keys() {
            if !all_names.contains(key) {
                all_keys.insert(key.clone());
            }
        }
        d.workspaces.push(DraftWorkspace {
            name: names[i].clone(),
            path: c.rel.clone(),
            sources: c.sources.clone(),
            underlays: underlays[i].clone(),
            colcon_defaults: colcon_defaults.clone(),
            packages: n,
            origin,
        });
    }

    // .repos files nobody imported yet become workspaces of their own.
    for (rel, repos) in unmatched_repos {
        let stem = rel
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "main".into());
        let mut name = workspace_name(Path::new(&stem), None);
        let base = name.clone();
        let mut n = 2;
        while names.contains(&name) {
            name = format!("{base}{n}");
            n += 1;
        }
        names.push(name.clone());
        let path = if name == "main" {
            PathBuf::from("ws")
        } else {
            PathBuf::from(format!("{name}_ws"))
        };
        d.todos.push(msg!(
            "init.workspaces.path_guessed",
            workspace = name,
            file = rel.display(),
            path = path.display()
        ));
        d.workspaces.push(DraftWorkspace {
            name,
            path,
            sources: Some(rel.clone()),
            underlays: Vec::new(),
            colcon_defaults: None,
            packages: 0,
            origin: msg!(
                "init.origin.repos_not_imported",
                file = rel.display(),
                count = repos.repositories.len()
            ),
        });
    }

    if d.workspaces.is_empty() {
        d.todos.push(msg!("init.workspaces.placeholder"));
        d.workspaces.push(DraftWorkspace {
            name: "main".into(),
            path: PathBuf::from("ws"),
            sources: Some(PathBuf::from("main.repos")),
            underlays: Vec::new(),
            colcon_defaults: None,
            packages: 0,
            origin: msg!("init.origin.placeholder"),
        });
    }

    d.external_keys = all_keys.into_iter().collect();
    if !d.external_keys.is_empty() {
        d.notes.push(msg!(
            "init.dependencies.rosdep_keys",
            count = d.external_keys.len()
        ));
    }
    d.todos.push(msg!("init.dependencies.apt_undetectable"));
}

// ---------------------------------------------------------------- rendering

fn q(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

fn qp(p: &Path) -> String {
    q(&p.to_string_lossy())
}

fn list(items: &[String]) -> String {
    format!(
        "[{}]",
        items.iter().map(|s| q(s)).collect::<Vec<_>>().join(", ")
    )
}

impl Draft {
    /// The manifest text. Guaranteed to parse; see [`Draft::validate`].
    pub fn render(&self) -> String {
        let mut o = String::new();
        o.push_str(&format!("# {}\n", msg!("draft.header.drafted_by")));
        o.push_str(&format!("# {}\n", msg!("draft.header.todo_lines")));
        o.push_str(&format!("# {}\n", msg!("draft.header.reference")));
        o.push_str("schema_version = 1\n\n");

        o.push_str("[project]\n");
        o.push_str(&format!("name = {}\n", q(&self.name)));
        match self.ros_distro {
            Some(d) => o.push_str(&format!(
                "ros_distro = {:<24} # humble | jazzy | rolling\n",
                q(d.as_str())
            )),
            None => o.push_str(&format!(
                "ros_distro = \"jazzy\"                  # TODO: {}; humble | jazzy | rolling\n",
                msg!("draft.not_detected")
            )),
        }
        let platforms: Vec<String> = self.platforms.iter().map(ToString::to_string).collect();
        o.push_str(&format!(
            "platforms = {:<25} # {}\n\n",
            list(&platforms),
            msg!("draft.platforms_hint")
        ));

        o.push_str("[environment]\n");
        o.push_str(&format!(
            "provider = {:<26} # native | pixi | docker\n\n",
            q(self.provider.as_str())
        ));
        match self.provider {
            ProviderKind::Pixi => {
                o.push_str("[environment.pixi]\n");
                o.push_str(&format!(
                    "manifest = {}\n\n",
                    qp(self
                        .pixi_manifest
                        .as_deref()
                        .unwrap_or(Path::new("pixi.toml")))
                ));
            }
            ProviderKind::Native => {
                o.push_str("[environment.native]\n");
                match &self.ubuntu {
                    Some(u) => o.push_str(&format!("ubuntu = {}\n", q(u))),
                    None => o.push_str(&format!(
                        "# ubuntu = \"24.04\"                     # TODO: {}; {}\n",
                        msg!("draft.not_detected"),
                        msg!("draft.ubuntu_hint")
                    )),
                }
                match &self.ros_base {
                    Some(b) => o.push_str(&format!("ros_base = {}\n", q(b))),
                    None => o.push_str(&format!(
                        "# ros_base = \"ros-<distro>-ros-base\"    # TODO: {}; {}\n",
                        msg!("draft.not_detected"),
                        msg!("draft.ros_base_hint")
                    )),
                }
                o.push('\n');
            }
            ProviderKind::Docker => {}
        }

        for ws in &self.workspaces {
            o.push_str(&format!("# {}\n", ws.origin));
            o.push_str(&format!("[workspaces.{}]\n", ws.name));
            o.push_str(&format!("path = {}\n", qp(&ws.path)));
            match &ws.sources {
                Some(s) => o.push_str(&format!("sources = {}\n", qp(s))),
                None => o.push_str(&format!(
                    "# sources = \"main.repos\"              # TODO: {}\n",
                    msg!("draft.sources_hint")
                )),
            }
            if ws.underlays.is_empty()
                && self.workspaces.len() > 1
                && self.workspaces.iter().all(|w| w.underlays.is_empty())
            {
                o.push_str(&format!(
                    "underlays = []                        # TODO: {}\n",
                    msg!("draft.underlays_hint")
                ));
            } else {
                o.push_str(&format!("underlays = {}\n", list(&ws.underlays)));
            }
            o.push('\n');
            o.push_str(&format!("[workspaces.{}.colcon]\n", ws.name));
            if let Some(f) = &ws.colcon_defaults {
                o.push_str(&format!("defaults = {}\n", qp(f)));
            }
            o.push_str(&format!(
                "mixins = []                           # TODO: {}\n",
                msg!("draft.mixins_hint")
            ));
            o.push_str("build_args = [\"--symlink-install\"]\n\n");
        }

        o.push_str("[dependencies]\n");
        o.push_str(&format!(
            "apt = []                              # TODO: {}\n",
            msg!("draft.apt_hint")
        ));
        o.push_str(&format!(
            "pip = []                              # {}\n",
            msg!("draft.pip_hint")
        ));
        if !self.external_keys.is_empty() {
            o.push_str(&format!(
                "# {}\n",
                msg!("draft.rosdep_keys", count = self.external_keys.len())
            ));
            for chunk in self.external_keys.chunks(8) {
                o.push_str(&format!("#   {}\n", chunk.join(" ")));
            }
        }
        o.push('\n');
        o.push_str("[dependencies.rosdep]\n");
        o.push_str("enabled = true\n");
        o.push_str("skip_keys = []\n\n");

        o.push_str("[env]\n");
        if self.env.is_empty() {
            o.push_str(&format!(
                "# ROS_DOMAIN_ID = \"0\"                  # TODO: {}\n",
                msg!("draft.env_hint")
            ));
        }
        for (k, v) in &self.env {
            o.push_str(&format!("{k} = {}\n", q(v)));
        }
        o.push('\n');

        o.push_str("[editor]\n");
        o.push_str(&format!("vscode = {}\n", self.vscode));
        o.push_str(&format!("clion = {}\n", self.clion));
        o.push_str(&format!("devcontainer = {}\n", self.devcontainer));
        o
    }

    /// Parse the rendered text with the real validator.
    pub fn validate(&self) -> crate::error::Result<Manifest> {
        Manifest::parse(&self.render(), Path::new("<init --from-env>"))
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
