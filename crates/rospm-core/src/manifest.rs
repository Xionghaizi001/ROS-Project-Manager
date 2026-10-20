//! `rospm.toml` — the project manifest (intent).
//!
//! Schema: `docs/spec.md` §2. Every table uses `deny_unknown_fields` so typos
//! fail loudly instead of being silently ignored.

use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::message::Message;
use crate::msg;
use crate::platform::{Platform, RosDistro};
use crate::replacements::{self, AptReplacement, AptResolution, ReplacementSource};

pub const MANIFEST_FILE: &str = "rospm.toml";
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// Environment variables derived from the workspace overlay order. Users may
/// not set them by hand (spec §2.2).
pub const RESERVED_ENV: &[&str] = &["AMENT_PREFIX_PATH", "COLCON_PREFIX_PATH"];

/// A Debian package name: `^[a-z0-9][a-z0-9+.-]*$` (Debian Policy §5.6.1).
pub fn is_apt_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"+.-".contains(&c))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub project: Project,
    pub environment: Environment,
    #[serde(default, skip_serializing_if = "Build::is_empty")]
    pub build: Build,
    /// Insertion order is preserved and used as the tie-breaker when sorting.
    pub workspaces: IndexMap<String, Workspace>,
    #[serde(default)]
    pub dependencies: Dependencies,
    /// Keyed by platform string (`linux-aarch64`); validated in [`Manifest::validate`].
    #[serde(default)]
    pub target: IndexMap<String, TargetOverride>,
    #[serde(default)]
    pub env: IndexMap<String, String>,
    #[serde(default)]
    pub editor: Editor,
    #[serde(default)]
    pub export: Export,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub ros_distro: RosDistro,
    pub platforms: Vec<Platform>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    #[default]
    Native,
    Pixi,
    Docker,
}

impl ProviderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderKind::Native => "native",
            ProviderKind::Pixi => "pixi",
            ProviderKind::Docker => "docker",
        }
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    pub provider: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<NativeEnv>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pixi: Option<PixiEnv>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker: Option<DockerEnv>,
}

/// Project-wide build options, independent of individual workspaces.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conda: Option<CondaBuild>,
}

impl Build {
    fn is_empty(&self) -> bool {
        self.conda.is_none()
    }
}

/// Select an existing Conda environment or a project-managed environment.
/// A manifest prefix is relative to the manifest directory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CondaBuild {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<ManagedConda>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedConda {
    /// Numeric major.minor[.patch]; emitted as a Conda `python=...` spec.
    pub python: String,
    pub channels: Vec<String>,
    /// Conda environment YAML, relative to the manifest directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_file: Option<PathBuf>,
    /// Explicit Conda lock written by `lock` and replayed by `apply --locked`,
    /// relative to the manifest directory; `{platform}` expands to the Conda
    /// subdir (`linux-64`). Defaults to `conda-{platform}.lock`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_file: Option<PathBuf>,
}

impl CondaBuild {
    pub fn validate(&self) -> Result<()> {
        if let Some(managed) = &self.managed {
            if self.name.is_some() || self.prefix.is_some() {
                return Err(Error::Validation(vec![msg!(
                    "manifest.conda_selector_required"
                )]));
            }
            let components: Vec<_> = managed.python.split('.').collect();
            if !(2..=3).contains(&components.len())
                || components
                    .iter()
                    .any(|s| s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()))
            {
                return Err(Error::Validation(vec![msg!(
                    "manifest.conda_python_invalid"
                )]));
            }
            if !managed.channels.iter().any(|c| c != "nodefaults")
                || managed.channels.iter().any(|c| {
                    c.trim().is_empty()
                        || c.trim() != c
                        || c.starts_with('-')
                        || c.contains(['\0', '\n', '\r'])
                })
            {
                return Err(Error::Validation(vec![msg!(
                    "manifest.conda_channels_invalid"
                )]));
            }
            for path in [&managed.environment_file, &managed.lock_file]
                .into_iter()
                .flatten()
            {
                if path.to_string_lossy().trim().is_empty() || path.to_string_lossy().contains('\0')
                {
                    return Err(Error::Validation(vec![msg!(
                        "manifest.conda_prefix_invalid"
                    )]));
                }
            }
            return Ok(());
        }
        let message = match (&self.name, &self.prefix) {
            (Some(name), None)
                if !name.is_empty()
                    && !name.starts_with('-')
                    && name != "."
                    && name != ".."
                    && !name
                        .chars()
                        .any(|c| c.is_whitespace() || "/\\:#\0".contains(c)) =>
            {
                return Ok(());
            }
            (Some(_), None) => msg!("manifest.conda_name_invalid"),
            (None, Some(prefix))
                if !prefix.to_string_lossy().trim().is_empty()
                    && !prefix.to_string_lossy().contains('\0') =>
            {
                return Ok(());
            }
            (None, Some(_)) => msg!("manifest.conda_prefix_invalid"),
            _ => msg!("manifest.conda_selector_required"),
        };
        Err(Error::Validation(vec![message]))
    }
}

/// The two official ROS 2 base metapackages (install guide: "Desktop Install
/// (Recommended)" vs "ROS-Base Install (Bare Bones)"). `environment.native.ros_base`
/// stays a free-form package name so `desktop-full` remains reachable by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RosFlavor {
    /// `ros-<distro>-ros-base`: libraries, build tools, CLI; no GUI.
    #[default]
    RosBase,
    /// `ros-<distro>-desktop`: ros-base plus RViz, demos and tutorials.
    Desktop,
}

impl RosFlavor {
    pub const ALL: [RosFlavor; 2] = [RosFlavor::RosBase, RosFlavor::Desktop];

    pub fn as_str(&self) -> &'static str {
        match self {
            RosFlavor::RosBase => "ros-base",
            RosFlavor::Desktop => "desktop",
        }
    }

    /// The apt metapackage for `distro`.
    pub fn package(&self, distro: RosDistro) -> String {
        format!("ros-{distro}-{}", self.as_str())
    }
}

impl fmt::Display for RosFlavor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("unknown ROS flavor `{0}` (expected ros-base or desktop)")]
pub struct ParseFlavorError(pub String);

impl std::str::FromStr for RosFlavor {
    type Err = ParseFlavorError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        RosFlavor::ALL
            .iter()
            .copied()
            .find(|f| f.as_str() == s)
            .ok_or_else(|| ParseFlavorError(s.to_string()))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeEnv {
    /// Ubuntu release, e.g. `"24.04"`. Defaults to the distro's official release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ubuntu: Option<String>,
    /// Base metapackage, e.g. `ros-jazzy-ros-base`. Defaults to `ros-<distro>-ros-base`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ros_base: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PixiEnv {
    /// Path to `pixi.toml`; `pixi.lock` is expected next to it (decision #4).
    #[serde(default = "default_pixi_manifest")]
    pub manifest: PathBuf,
}

fn default_pixi_manifest() -> PathBuf {
    PathBuf::from("pixi.toml")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DockerEnv {
    pub base_image: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dockerfile: Option<PathBuf>,
    /// buildx platform strings, e.g. `linux/arm64`.
    #[serde(default)]
    pub platforms: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    /// Relative to the manifest directory.
    pub path: PathBuf,
    /// vcstool `.repos` file, relative to the manifest directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sources: Option<PathBuf>,
    /// Workspaces this one is overlaid on. Empty = directly on the ROS base install.
    #[serde(default)]
    pub underlays: Vec<String>,
    #[serde(default)]
    pub colcon: Colcon,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Colcon {
    /// `COLCON_DEFAULTS_FILE`, relative to the manifest directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defaults: Option<PathBuf>,
    #[serde(default)]
    pub mixins: Vec<String>,
    #[serde(default)]
    pub build_args: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependencies {
    /// Extra apt packages not covered by any `package.xml`.
    #[serde(default)]
    pub apt: Vec<String>,
    /// Recorded only; never installed by rospm (decision #6).
    #[serde(default)]
    pub pip: Vec<String>,
    #[serde(default)]
    pub rosdep: Rosdep,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rosdep {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub skip_keys: Vec<String>,
    #[serde(default)]
    pub extra_sources: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl Default for Rosdep {
    fn default() -> Self {
        Rosdep {
            enabled: true,
            skip_keys: Vec::new(),
            extra_sources: Vec::new(),
        }
    }
}

/// Per-platform overrides. Merge order: append → replace → exclude (spec §2.2).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetOverride {
    #[serde(default)]
    pub dependencies: TargetDeps,
    #[serde(default)]
    pub replace: IndexMap<String, String>,
    #[serde(default)]
    pub exclude: TargetDeps,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetDeps {
    #[serde(default)]
    pub apt: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Editor {
    #[serde(default)]
    pub vscode: bool,
    #[serde(default)]
    pub clion: bool,
    #[serde(default)]
    pub devcontainer: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Export {
    #[serde(default)]
    pub compression: Compression,
    #[serde(default)]
    pub sign: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    #[default]
    None,
    Gzip,
    Zstd,
}

impl Manifest {
    /// Parse, check `schema_version`, and validate. `path` is only used in errors.
    pub fn parse(text: &str, path: &Path) -> Result<Manifest> {
        let manifest: Manifest = toml::from_str(text).map_err(|source| Error::ManifestParse {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
        if manifest.schema_version != SUPPORTED_SCHEMA_VERSION {
            return Err(Error::SchemaVersion {
                found: manifest.schema_version,
                supported: SUPPORTED_SCHEMA_VERSION,
            });
        }
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn load(path: &Path) -> Result<Manifest> {
        let text = fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        Manifest::parse(&text, path)
    }

    /// Walk up from `start` looking for `rospm.toml`.
    pub fn find(start: &Path) -> Option<PathBuf> {
        start
            .ancestors()
            .map(|dir| dir.join(MANIFEST_FILE))
            .find(|candidate| candidate.is_file())
    }

    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|source| Error::Serialize {
            what: "manifest",
            source: Box::new(source),
        })
    }

    /// Structural validation beyond what serde enforces.
    pub fn validate(&self) -> Result<()> {
        let mut errs: Vec<Message> = Vec::new();

        if self.project.name.trim().is_empty() {
            errs.push(msg!("manifest.name_empty"));
        }
        if self.project.platforms.is_empty() {
            errs.push(msg!("manifest.no_platforms"));
        }
        if let Some(conda) = &self.build.conda {
            if let Err(Error::Validation(messages)) = conda.validate() {
                errs.extend(messages);
            }
            if self.environment.provider != ProviderKind::Native {
                errs.push(msg!("manifest.conda_native_only"));
            }
            let lock_file = conda.managed.as_ref().and_then(|m| m.lock_file.as_ref());
            if self.project.platforms.len() > 1
                && lock_file.is_some_and(|p| !p.to_string_lossy().contains("{platform}"))
            {
                errs.push(msg!("manifest.conda_lock_platform_required"));
            }
        }

        // Exactly the matching provider sub-table may be present (spec §2.2).
        let env = &self.environment;
        let present: Vec<&str> = [
            (env.native.is_some(), "native"),
            (env.pixi.is_some(), "pixi"),
            (env.docker.is_some(), "docker"),
        ]
        .into_iter()
        .filter(|(p, _)| *p)
        .map(|(_, n)| n)
        .collect();
        for name in &present {
            if *name != env.provider.as_str() {
                errs.push(msg!(
                    "manifest.provider_table_mismatch",
                    name = name,
                    provider = env.provider
                ));
            }
        }
        // `native` has usable defaults; `pixi` and `docker` need their table.
        if env.provider != ProviderKind::Native && !present.contains(&env.provider.as_str()) {
            errs.push(msg!(
                "manifest.provider_table_required",
                provider = env.provider
            ));
        }

        if self.workspaces.is_empty() {
            errs.push(msg!("manifest.no_workspaces"));
        }
        let mut underlay_errors = false;
        for (name, ws) in &self.workspaces {
            for underlay in &ws.underlays {
                if underlay == name {
                    errs.push(msg!("manifest.self_underlay", name = name));
                    underlay_errors = true;
                } else if !self.workspaces.contains_key(underlay) {
                    errs.push(msg!(
                        "manifest.unknown_underlay",
                        name = name,
                        underlay = underlay
                    ));
                    underlay_errors = true;
                }
            }
        }
        if !underlay_errors {
            if let Err(Error::Validation(cycle)) = self.build_order() {
                errs.extend(cycle);
            }
        }

        for key in self.target.keys() {
            match key.parse::<Platform>() {
                Ok(p) if key != p.as_str() => {
                    errs.push(msg!("manifest.target_unknown_platform", key = key))
                }
                Ok(p) if !self.project.platforms.contains(&p) => {
                    errs.push(msg!("manifest.target_not_listed", key = key))
                }
                Ok(_) => {}
                Err(_) => errs.push(msg!("manifest.target_unknown_platform", key = key)),
            }
        }

        // These names reach `sudo apt-get install` as arguments; a leading `-`
        // would turn one into an apt option (e.g. `-oDPkg::Pre-Invoke=...`).
        let targets = self.target.iter();
        let apt_fields = (self
            .dependencies
            .apt
            .iter()
            .map(|n| ("dependencies.apt".to_string(), n)))
        .chain(targets.clone().flat_map(|(k, t)| {
            t.dependencies
                .apt
                .iter()
                .map(move |n| (format!("target.{k}.dependencies.apt"), n))
        }))
        .chain(targets.clone().flat_map(|(k, t)| {
            t.replace
                .iter()
                .flat_map(|(from, to)| [from, to])
                .map(move |n| (format!("target.{k}.replace"), n))
        }))
        .chain(targets.flat_map(|(k, t)| {
            t.exclude
                .apt
                .iter()
                .map(move |n| (format!("target.{k}.exclude.apt"), n))
        }));
        for (field, name) in apt_fields {
            if !is_apt_name(name) {
                errs.push(msg!(
                    "manifest.apt_name_invalid",
                    field = field,
                    name = name
                ));
            }
        }

        for key in self.env.keys() {
            if RESERVED_ENV.contains(&key.as_str()) {
                errs.push(msg!("manifest.env_reserved", key = key));
            }
        }

        if errs.is_empty() {
            Ok(())
        } else {
            Err(Error::Validation(errs))
        }
    }

    /// Topological order of workspaces: every underlay precedes the workspaces
    /// built on it. Ties are broken by manifest order, so the result is stable.
    pub fn build_order(&self) -> Result<Vec<String>> {
        let mut indegree: IndexMap<&str, usize> =
            self.workspaces.keys().map(|k| (k.as_str(), 0)).collect();
        let mut dependents: IndexMap<&str, Vec<&str>> = self
            .workspaces
            .keys()
            .map(|k| (k.as_str(), Vec::new()))
            .collect();

        for (name, ws) in &self.workspaces {
            for underlay in &ws.underlays {
                match dependents.get_mut(underlay.as_str()) {
                    Some(list) => {
                        list.push(name.as_str());
                        *indegree.get_mut(name.as_str()).expect("known workspace") += 1;
                    }
                    None => {
                        return Err(Error::Validation(vec![msg!(
                            "manifest.unknown_underlay",
                            name = name,
                            underlay = underlay
                        )]))
                    }
                }
            }
        }

        let mut ready: Vec<&str> = indegree
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(n, _)| *n)
            .collect();
        let mut order: Vec<String> = Vec::with_capacity(self.workspaces.len());

        while !ready.is_empty() {
            let current = ready.remove(0);
            order.push(current.to_string());
            for dependent in &dependents[current] {
                let d = indegree.get_mut(dependent).expect("known workspace");
                *d -= 1;
                if *d == 0 {
                    ready.push(dependent);
                }
            }
        }

        if order.len() != self.workspaces.len() {
            let stuck: Vec<&str> = indegree
                .iter()
                .filter(|(_, d)| **d > 0)
                .map(|(n, _)| *n)
                .collect();
            return Err(Error::Validation(vec![msg!(
                "manifest.underlay_cycle",
                workspaces = stuck.join(", ")
            )]));
        }
        Ok(order)
    }

    /// All workspaces `name` (transitively) sits on, in build order. Excludes `name`.
    pub fn transitive_underlays(&self, name: &str) -> Result<Vec<String>> {
        let ws = self
            .workspaces
            .get(name)
            .ok_or_else(|| Error::UnknownWorkspace(name.to_string()))?;
        let mut seen: HashSet<&str> = HashSet::new();
        let mut stack: Vec<&str> = ws.underlays.iter().map(String::as_str).collect();
        while let Some(u) = stack.pop() {
            if seen.insert(u) {
                if let Some(w) = self.workspaces.get(u) {
                    stack.extend(w.underlays.iter().map(String::as_str));
                }
            }
        }
        Ok(self
            .build_order()?
            .into_iter()
            .filter(|n| seen.contains(n.as_str()))
            .collect())
    }

    pub fn target_for(&self, platform: Platform) -> Option<&TargetOverride> {
        self.target.get(platform.as_str())
    }

    /// Extra apt packages after append → replace → exclude → deduplicate.
    /// Manifest replacements override bundled defaults, including identity rules.
    pub fn effective_apt(&self, platform: Platform) -> Vec<String> {
        self.resolve_apt(platform).packages
    }

    /// Resolve exact package names once (no chained or wildcard replacements).
    /// Retain provenance for changes that survive the user's final exclusions.
    pub fn resolve_apt(&self, platform: Platform) -> AptResolution {
        let target = self.target_for(platform);
        let defaults = replacements::builtin().target.get(&platform);
        let mut pkgs: Vec<String> = self.dependencies.apt.clone();
        if let Some(t) = target {
            pkgs.extend(t.dependencies.apt.iter().cloned());
        }
        let mut result = AptResolution::default();
        let mut seen = HashSet::new();
        for from in pkgs {
            let rule = target
                .and_then(|t| t.replace.get(&from))
                .map(|to| (to, ReplacementSource::Manifest))
                .or_else(|| {
                    defaults
                        .and_then(|rules| rules.get(&from))
                        .map(|to| (to, ReplacementSource::Builtin))
                });
            let to = rule.map(|(to, _)| to).unwrap_or(&from);
            if target.is_some_and(|t| t.exclude.apt.contains(to)) {
                continue;
            }
            if let Some((to, source)) = rule.filter(|(to, _)| *to != &from) {
                let replacement = AptReplacement {
                    from: from.clone(),
                    to: to.clone(),
                    source,
                };
                if !result.replacements.contains(&replacement) {
                    result.replacements.push(replacement);
                }
            }
            if seen.insert(to.clone()) {
                result.packages.push(to.clone());
            }
        }
        result
    }

    /// Ubuntu release for the native provider.
    pub fn ubuntu(&self) -> String {
        self.environment
            .native
            .as_ref()
            .and_then(|n| n.ubuntu.clone())
            .unwrap_or_else(|| self.project.ros_distro.default_ubuntu().to_string())
    }

    /// Base metapackage for the native provider.
    pub fn ros_base_package(&self) -> String {
        self.environment
            .native
            .as_ref()
            .and_then(|n| n.ros_base.clone())
            .unwrap_or_else(|| RosFlavor::RosBase.package(self.project.ros_distro))
    }
}
