//! Lock command implementation.

use anyhow::{Context as _, Result};
use serde::Serialize;

use rospm_core::lock::sha256_hex;
use rospm_core::mirror;
use rospm_core::msg;
use rospm_core::platform::Platform;
use rospm_core::probe::Host;
use rospm_core::provider::{self, Context};
use rospm_core::snapshot::{Fetch, Http};

use crate::i18n::tr;

use super::{host_platform, load, Global};

/// `rospm lock [--platform <p>]`: resolve the host into `rospm.lock` (spec §3).
/// Idempotent: the file is rewritten only when its content changes.
pub fn lock(global: &Global, platform: Option<Platform>) -> Result<()> {
    let loaded = load(global)?;
    let host = Host::probe();
    let platform = match platform {
        Some(p) => p,
        None => host_platform()?,
    };
    let previous = loaded.load_lock()?;
    let ctx = Context {
        manifest: &loaded.manifest,
        root: &loaded.root,
        platform,
        locked: false,
        lock: previous.as_ref(),
        offline: false,
        host: &host,
        mirror: mirror::OFFICIAL,
    };
    let offline = std::env::var_os("ROSPM_NO_NETWORK").is_some_and(|v| !v.is_empty() && v != "0");
    let http = Http::default();
    let fetch: Option<&dyn Fetch> = if offline { None } else { Some(&http) };
    let mut resolution = provider::for_manifest(&loaded.manifest).resolve_lock(&ctx, fetch)?;

    // The hash is of the file the user edits, wherever `--manifest` points.
    let manifest_text = std::fs::read_to_string(&loaded.path)
        .with_context(|| msg!("cli.err.cannot_read", path = loaded.path.display()).to_string())?;
    resolution.lock.manifest_sha256 = sha256_hex(&manifest_text);

    let lock_path = loaded.lock_path();
    let mut changed = true;
    if let Some(prev) = &previous {
        resolution.lock.merge_other_platforms(prev, platform);
        if resolution.lock.same_content(prev) {
            resolution.lock.generated_at = prev.generated_at.clone();
            changed = false;
        }
    }
    if changed {
        resolution.lock.save(&lock_path)?;
    }

    let l = &resolution.lock;
    let native = l.provider.native.as_ref();
    let report = LockReport {
        ok: true,
        lock: lock_path.display().to_string(),
        changed,
        platform: platform.to_string(),
        generated_at: l.generated_at.clone(),
        sources: l.sources.len(),
        apt: l.apt_for(platform).len(),
        apt_other_platforms: l.apt.len() - l.apt_for(platform).len(),
        pip: l.pip.len(),
        conda: l.conda_for(platform).is_some(),
        ros_snapshot: native.and_then(|n| n.ros_snapshot.clone()),
        ubuntu_snapshot: native.and_then(|n| n.ubuntu_snapshot.clone()),
        notes: resolution.notes.iter().map(ToString::to_string).collect(),
    };
    if global.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    for n in &report.notes {
        println!("{}", msg!("cli.lock.note_line", text = n));
    }
    println!(
        "{}",
        msg!(
            "cli.lock.summary",
            platform = report.platform,
            sources = report.sources,
            apt = report.apt,
            pip = report.pip
        )
    );
    if report.apt_other_platforms > 0 {
        println!(
            "{}",
            msg!(
                "cli.lock.other_platforms_kept",
                count = report.apt_other_platforms
            )
        );
    }
    let live = || tr("cli.lock.live");
    println!(
        "{}",
        msg!(
            "cli.lock.snapshots",
            ros = report.ros_snapshot.clone().unwrap_or_else(live),
            ubuntu = report.ubuntu_snapshot.clone().unwrap_or_else(live)
        )
    );
    if changed {
        println!("{}", msg!("cli.lock.written", path = report.lock));
    } else {
        println!("{}", msg!("cli.lock.unchanged", path = report.lock));
    }
    Ok(())
}

#[derive(Serialize)]
struct LockReport {
    ok: bool,
    lock: String,
    changed: bool,
    platform: String,
    generated_at: String,
    sources: usize,
    apt: usize,
    apt_other_platforms: usize,
    pip: usize,
    /// Whether this platform's managed Conda environment was recorded.
    conda: bool,
    ros_snapshot: Option<String>,
    ubuntu_snapshot: Option<String>,
    notes: Vec<String>,
}
