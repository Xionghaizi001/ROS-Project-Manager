//! Environment providers turn a manifest into plans.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use crate::error::{Error, Result};
use crate::lock::Lock;
use crate::manifest::{Manifest, ProviderKind};
use crate::mirror::Mirror;
use crate::msg;
use crate::plan::Plan;
use crate::platform::Platform;
use crate::probe::{EnvReport, Host, Status};
use crate::provider::native::deps::DepsReport;

pub mod native;

/// Everything a provider needs to know about one invocation.
pub struct Context<'a> {
    pub manifest: &'a Manifest,
    /// Directory containing `rospm.toml`; all manifest paths are relative to it.
    pub root: &'a Path,
    pub platform: Platform,
    /// `apply --locked`: honour `rospm.lock` (snapshots, pinned revisions).
    pub locked: bool,
    /// `rospm.lock` next to the manifest, if it exists and parsed. Loaded by
    /// the CLI for every command; `apply --locked` and `verify` require it.
    pub lock: Option<&'a Lock>,
    /// `apply --offline`: use only bundled payloads.
    pub offline: bool,
    /// Facts about this machine, probed once per command. Plans consult it
    /// so they only contain the steps that are actually missing.
    pub host: &'a Host,
    /// Where the native provider downloads ROS from (decision #17).
    pub mirror: Mirror,
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

    /// ROS apt source → base install → source import → rosdep → extra
    /// packages (spec §4 `apply`). Steps the host already satisfies are omitted.
    fn plan_apply(&self, ctx: &Context) -> Result<Plan>;

    /// Remove the ROS installation the manifest describes (spec §4 `uninstall`).
    /// Project files, sources and workspaces are never touched.
    fn plan_uninstall(&self, ctx: &Context, keep_source: bool) -> Result<Plan>;

    /// Build `only` (one workspace) or every workspace in overlay order.
    fn plan_build(&self, ctx: &Context, only: Option<&str>) -> Result<Plan>;

    /// A script the user can `source` to enter the environment (spec §4 `env`).
    fn env_script(&self, ctx: &Context, shell: Shell) -> Result<String>;

    /// Compare what the manifest needs with what `ctx.host` has (spec §4 `check`).
    /// Read-only: never installs, never writes.
    fn probe_env(&self, ctx: &Context) -> Result<EnvReport>;

    /// Resolve what the host currently has into a lock file (spec §3,
    /// `rospm lock`). Read-only on the system; the CLI writes the result.
    fn resolve_lock(&self, ctx: &Context) -> Result<Lock>;

    /// Compare `lock` with the host (spec §4 `verify`): source revisions,
    /// apt versions, pip versions. Read-only.
    fn verify(&self, ctx: &Context, lock: &Lock) -> Result<EnvReport>;

    /// Compare every workspace's `package.xml` dependency keys with what is
    /// already resolved/installed (spec §4 `deps check`). Read-only.
    fn probe_deps(&self, ctx: &Context) -> Result<DepsReport>;

    /// Build a plan that hands the missing dependency keys to rosdep (spec §4
    /// `deps install`).
    fn plan_deps_install(&self, ctx: &Context) -> Result<Plan>;
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
                ProviderKind::Pixi => "provider = \"pixi\"",
                ProviderKind::Docker => "provider = \"docker\"",
                ProviderKind::Native => "provider = \"native\"",
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
    fn plan_uninstall(&self, _ctx: &Context, _keep_source: bool) -> Result<Plan> {
        Err(self.err())
    }
    fn plan_build(&self, _ctx: &Context, _only: Option<&str>) -> Result<Plan> {
        Err(self.err())
    }
    fn env_script(&self, _ctx: &Context, _shell: Shell) -> Result<String> {
        Err(self.err())
    }
    fn resolve_lock(&self, _ctx: &Context) -> Result<Lock> {
        Err(self.err())
    }
    fn verify(&self, _ctx: &Context, _lock: &Lock) -> Result<EnvReport> {
        Err(self.err())
    }
    fn probe_env(&self, _ctx: &Context) -> Result<EnvReport> {
        // `check` must still validate manifests for these providers, so this
        // is a report with one informational line rather than an error.
        let mut report = EnvReport::default();
        report.push(
            "provider",
            self.kind.to_string(),
            msg!("probe.provider.probing"),
            msg!("probe.provider.not_implemented", phase = self.phase),
            Status::Info,
        );
        Ok(report)
    }
    fn probe_deps(&self, _ctx: &Context) -> Result<DepsReport> {
        Err(self.err())
    }
    fn plan_deps_install(&self, _ctx: &Context) -> Result<Plan> {
        Err(self.err())
    }
}
