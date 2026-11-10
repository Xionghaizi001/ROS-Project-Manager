//! Command implementations. Anything scheduled for a later roadmap phase
//! returns [`not_implemented`] with the phase so users know where it stands.
//!
//! All user-facing text goes through the i18n layer (spec §4.2): either a
//! `msg!` key rendered by [`crate::i18n::render`] via `Display`, or
//! [`tr`] for plain CLI strings. `--json` key names and enum values stay
//! English.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _, Result};

use rospm_core::lock::{Lock, LOCK_FILE};
use rospm_core::manifest::{Manifest, MANIFEST_FILE};
use rospm_core::mirror::Mirror;
use rospm_core::platform::Platform;
use rospm_core::probe::Host;
use rospm_core::provider::Context;
use rospm_core::{msg, Error};

use crate::i18n::tr;

mod apply;
mod build;
mod check;
mod deps;
mod doctor;
mod environment;
mod init;
mod interaction;
mod lock;
mod logging;
mod report;
mod run;
mod uninstall;
pub mod venv;
mod verify;
pub mod worker;

pub use apply::apply;
pub use build::build;
pub use check::check;
pub use deps::{deps_check, deps_install};
pub use doctor::doctor;
pub use environment::{env, shell};
pub use init::init;
pub use lock::lock;
pub use run::run;
pub use uninstall::uninstall;
pub use verify::verify;

pub struct Global {
    pub manifest: Option<PathBuf>,
    pub json: bool,
}

struct Loaded {
    venv: Option<PathBuf>,
    path: PathBuf,
    root: PathBuf,
    manifest: Manifest,
}

impl Loaded {
    /// `rospm.lock` next to the manifest.
    fn lock_path(&self) -> PathBuf {
        self.root.join(LOCK_FILE)
    }

    /// The lock file, if there is one. A present but unparseable lock is an
    /// error: it is a project file, not a cache.
    fn load_lock(&self) -> Result<Option<Lock>> {
        let path = self.lock_path();
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(Lock::load(&path)?))
    }
}

/// Acquire at the command boundary; nested build/retry helpers reuse the guard.
fn load_mutating(
    global: &Global,
    dry_run: bool,
) -> Result<(Loaded, Vec<rospm_core::worker::WorkspaceLock>)> {
    let loaded = load(global)?;
    if dry_run {
        return Ok((loaded, Vec::new()));
    }
    let mut leases = vec![mutation_lease(&loaded.root)?];
    // A previous operation may have changed the manifest before we got the lock.
    let loaded = load(global)?;
    leases.extend(loaded.workspace_leases()?);
    Ok((loaded, leases))
}

fn mutation_lease(path: &Path) -> Result<rospm_core::worker::WorkspaceLock> {
    rospm_core::worker::WorkspaceLock::try_acquire_mutation(path)?
        .with_context(|| msg!("cli.err.workspace_busy", path = path.display()).to_string())
}

impl Loaded {
    /// Caller already owns the project lease (CLI boundary or TaskRunner).
    fn workspace_leases(&self) -> Result<Vec<rospm_core::worker::WorkspaceLock>> {
        let root = rospm_core::worker::resolve_workspace(&self.root)?;
        let paths = self
            .manifest
            .workspaces
            .values()
            .map(|ws| rospm_core::worker::resolve_workspace(&self.root.join(&ws.path)))
            .collect::<std::result::Result<std::collections::BTreeSet<_>, _>>()?;
        paths
            .into_iter()
            .filter(|path| path != &root)
            .map(|path| mutation_lease(&path))
            .collect()
    }
}

fn locate(global: &Global) -> Result<PathBuf> {
    if let Some(p) = &global.manifest {
        return Ok(p.clone());
    }
    let cwd = std::env::current_dir().context(tr("cli.err.no_cwd"))?;
    Manifest::find(&cwd).ok_or_else(|| {
        anyhow!(
            "{}",
            msg!(
                "cli.err.manifest_not_found",
                file = MANIFEST_FILE,
                dir = cwd.display()
            )
        )
    })
}

fn load(global: &Global) -> Result<Loaded> {
    let path = locate(global)?;
    let manifest = Manifest::load(&path)?;
    // `--manifest rospm.toml` (no directory part) has an empty parent.
    let root = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let root = root
        .canonicalize()
        .with_context(|| msg!("cli.err.cannot_resolve", path = root.display()).to_string())?;
    Ok(Loaded {
        venv: None,
        path,
        root,
        manifest,
    })
}

fn host_platform() -> Result<Platform> {
    Platform::current().ok_or_else(|| anyhow!("{}", tr("cli.err.unsupported_host")))
}

pub fn not_implemented(feature: &'static str, phase: &'static str) -> Result<()> {
    Err(Error::NotImplemented { feature, phase }.into())
}

/// The error chain as one localised line. A `rospm_core::Error` already
/// describes its own source, so the chain stops there.
pub fn describe(err: &anyhow::Error) -> String {
    let mut parts: Vec<String> = Vec::new();
    for cause in err.chain() {
        if let Some(core) = cause.downcast_ref::<Error>() {
            parts.push(core.to_string());
            break;
        }
        parts.push(cause.to_string());
    }
    parts.join(": ")
}

fn context<'a>(
    loaded: &'a Loaded,
    host: &'a Host,
    locked: bool,
    lock: Option<&'a Lock>,
    offline: bool,
    mirror: Mirror,
) -> Result<Context<'a>> {
    Ok(Context {
        venv: loaded.venv.as_deref(),
        manifest: &loaded.manifest,
        root: &loaded.root,
        platform: host_platform()?,
        locked,
        lock,
        offline,
        host,
        mirror,
    })
}

#[derive(clap::Args, Default)]
pub struct VenvArgs {
    #[arg(long, conflicts_with = "venv_path", help = tr("venv.project"))]
    pub venv: bool,
    #[arg(long, value_name = "PATH", help = tr("venv.path"))]
    pub venv_path: Option<PathBuf>,
}

impl VenvArgs {
    fn select(&self, loaded: &mut Loaded) -> Result<()> {
        self.select_report(loaded)?;
        if let Some(path) = &loaded.venv {
            loaded.venv = Some(rospm_core::venv::resolve(path, &loaded.root)?);
        }
        Ok(())
    }

    fn select_report(&self, loaded: &mut Loaded) -> Result<()> {
        let path = if self.venv {
            Some(loaded.root.join(".venv"))
        } else {
            self.venv_path.clone()
        };
        if let Some(path) = path {
            loaded.venv = Some(rospm_core::venv::input_path(
                &path,
                &std::env::current_dir()?,
            )?);
        }
        Ok(())
    }
}
