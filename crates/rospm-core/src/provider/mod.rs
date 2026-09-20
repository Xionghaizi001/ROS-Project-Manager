//! Environment providers turn a manifest into plans.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use crate::error::{Error, Result};
use crate::manifest::{Manifest, ProviderKind};
use crate::plan::Plan;
use crate::platform::Platform;

pub mod native;

/// Everything a provider needs to know about one invocation.
pub struct Context<'a> {
    pub manifest: &'a Manifest,
    /// Directory containing `rospm.toml`; all manifest paths are relative to it.
    pub root: &'a Path,
    pub platform: Platform,
    /// `apply --locked`: honour `rospm.lock` (snapshots, pinned revisions).
    pub locked: bool,
    /// `apply --offline`: use only bundled payloads.
    pub offline: bool,
}

/// Login shells we can emit `source` lines for. ROS ships `setup.bash`,
/// `setup.zsh` and `setup.sh`; fish is not officially supported upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
}

impl Shell {
    pub fn as_str(&self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
        }
    }
}

impl fmt::Display for Shell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("unsupported shell `{0}` (expected bash or zsh)")]
pub struct ParseShellError(pub String);

impl FromStr for Shell {
    type Err = ParseShellError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "bash" => Ok(Shell::Bash),
            "zsh" => Ok(Shell::Zsh),
            other => Err(ParseShellError(other.to_string())),
        }
    }
}

pub trait Provider {
    fn kind(&self) -> ProviderKind;

    /// Base install → source import → rosdep → extra packages (spec §4 `apply`).
    fn plan_apply(&self, ctx: &Context) -> Result<Plan>;

    /// Build `only` (one workspace) or every workspace in overlay order.
    fn plan_build(&self, ctx: &Context, only: Option<&str>) -> Result<Plan>;

    /// A script the user can `source` to enter the environment (spec §4 `env`).
    fn env_script(&self, ctx: &Context, shell: Shell) -> Result<String>;
}

/// Select the provider declared in the manifest.
pub fn for_manifest(manifest: &Manifest) -> Box<dyn Provider> {
    match manifest.environment.provider {
        ProviderKind::Native => Box::new(native::Native),
        ProviderKind::Pixi => Box::new(Unimplemented {
            kind: ProviderKind::Pixi,
            phase: "P3",
        }),
        ProviderKind::Docker => Box::new(Unimplemented {
            kind: ProviderKind::Docker,
            phase: "P4",
        }),
    }
}

/// Placeholder for providers scheduled in later roadmap phases.
struct Unimplemented {
    kind: ProviderKind,
    phase: &'static str,
}

impl Unimplemented {
    fn err(&self) -> Error {
        Error::NotImplemented {
            feature: match self.kind {
                ProviderKind::Pixi => "pixi provider",
                ProviderKind::Docker => "docker provider",
                ProviderKind::Native => "native provider",
            },
            phase: self.phase,
        }
    }
}

impl Provider for Unimplemented {
    fn kind(&self) -> ProviderKind {
        self.kind
    }
    fn plan_apply(&self, _ctx: &Context) -> Result<Plan> {
        Err(self.err())
    }
    fn plan_build(&self, _ctx: &Context, _only: Option<&str>) -> Result<Plan> {
        Err(self.err())
    }
    fn env_script(&self, _ctx: &Context, _shell: Shell) -> Result<String> {
        Err(self.err())
    }
}
