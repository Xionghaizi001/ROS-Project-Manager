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
use crate::platform::{Platform, RosDistro};

pub const MANIFEST_FILE: &str = "rospm.toml";
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// Environment variables derived from the workspace overlay order. Users may
/// not set them by hand (spec §2.2).
pub const RESERVED_ENV: &[&str] = &["AMENT_PREFIX_PATH", "COLCON_PREFIX_PATH"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub project: Project,
    pub environment: Environment,
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
        let mut errs: Vec<String> = Vec::new();

        if self.project.name.trim().is_empty() {
            errs.push("project.name must not be empty".into());
        }
        if self.project.platforms.is_empty() {
            errs.push("project.platforms must list at least one platform".into());
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
                errs.push(format!(
                    "environment.{name} is present but environment.provider is \"{}\"",
                    env.provider
                ));
            }
        }
        // `native` has usable defaults; `pixi` and `docker` need their table.
        if env.provider != ProviderKind::Native && !present.contains(&env.provider.as_str()) {
            errs.push(format!(
                "environment.{p} table is required when environment.provider = \"{p}\"",
                p = env.provider
            ));
        }

        if self.workspaces.is_empty() {
            errs.push("at least one [workspaces.<name>] is required".into());
        }
        let mut underlay_errors = false;
        for (name, ws) in &self.workspaces {
            for underlay in &ws.underlays {
                if underlay == name {
                    errs.push(format!("workspace `{name}` lists itself as an underlay"));
                    underlay_errors = true;
                } else if !self.workspaces.contains_key(underlay) {
                    errs.push(format!(
                        "workspace `{name}` references unknown underlay `{underlay}`"
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
                Ok(p) if !self.project.platforms.contains(&p) => errs.push(format!(
                    "target.{key} is declared but `{key}` is not listed in project.platforms"
                )),
                Ok(_) => {}
                Err(e) => errs.push(format!("target.{key}: {e}")),
            }
        }

        for key in self.env.keys() {
            if RESERVED_ENV.contains(&key.as_str()) {
                errs.push(format!(
                    "env.{key} is derived from the workspace overlay order and must not be set manually"
                ));
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
                        return Err(Error::Validation(vec![format!(
                            "workspace `{name}` references unknown underlay `{underlay}`"
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
            return Err(Error::Validation(vec![format!(
                "underlay cycle detected among workspaces: {}",
                stuck.join(", ")
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

    /// Extra apt packages for `platform` after applying overrides
    /// (append → replace → exclude), de-duplicated and order-preserving.
    pub fn effective_apt(&self, platform: Platform) -> Vec<String> {
        let mut pkgs: Vec<String> = self.dependencies.apt.clone();
        if let Some(t) = self.target_for(platform) {
            pkgs.extend(t.dependencies.apt.iter().cloned());
            for p in pkgs.iter_mut() {
                if let Some(replacement) = t.replace.get(p.as_str()) {
                    *p = replacement.clone();
                }
            }
            pkgs.retain(|p| !t.exclude.apt.contains(p));
        }
        let mut seen = HashSet::new();
        pkgs.retain(|p| seen.insert(p.clone()));
        pkgs
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
            .unwrap_or_else(|| format!("ros-{}-ros-base", self.project.ros_distro))
    }
}
