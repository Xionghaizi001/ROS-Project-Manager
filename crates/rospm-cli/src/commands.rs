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
mod report;
mod uninstall;
mod verify;

pub use apply::apply;
pub use build::build;
pub use check::check;
pub use deps::{deps_check, deps_install};
pub use doctor::doctor;
pub use environment::{env, shell};
pub use init::init;
pub use lock::lock;
pub use uninstall::uninstall;
pub use verify::verify;

pub struct Global {
    pub manifest: Option<PathBuf>,
    pub json: bool,
}

struct Loaded {
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
