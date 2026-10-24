//! Verify command implementation.

use anyhow::Result;
use serde::Serialize;

use rospm_core::lock::LOCK_FILE;
use rospm_core::manifest::MANIFEST_FILE;
use rospm_core::mirror;
use rospm_core::probe::{EnvReport, Host, Status};
use rospm_core::provider;
use rospm_core::{msg, Error, Message};

use super::report::{print_env_report, EnvSummary};
use super::{context, load, Global};

#[derive(Serialize)]
struct VerifyReport {
    ok: bool,
    manifest: String,
    lock: String,
    generated_at: String,
    manifest_matches: bool,
    ros_snapshot: Option<String>,
    ubuntu_snapshot: Option<String>,
    environment: EnvSummary,
}

/// `rospm verify`: compare `rospm.lock` with the host (spec §4, read-only).
pub fn verify(global: &Global) -> Result<()> {
    let loaded = load(global)?;
    let lock = loaded
        .load_lock()?
        .ok_or_else(|| Error::Host(msg!("cli.verify.no_lock", file = LOCK_FILE)))?;
    let manifest_text =
        std::fs::read_to_string(&loaded.path).map_err(|e| Error::io(loaded.path.as_path(), e))?;
    let manifest_matches = lock.matches_manifest(&manifest_text);

    let host = Host::probe();
    let ctx = context(&loaded, &host, false, Some(&lock), false, mirror::OFFICIAL)?;

    let mut report = EnvReport::default();
    report.push(
        "manifest",
        Message::text(MANIFEST_FILE),
        lock.manifest_sha256.clone(),
        if manifest_matches {
            msg!("cli.verify.manifest_ok")
        } else {
            msg!("cli.verify.manifest_changed")
        },
        if manifest_matches {
            Status::Ok
        } else {
            Status::Mismatch
        },
    );
    let core_report = provider::for_manifest(&loaded.manifest).verify(&ctx, &lock)?;
    report.items.extend(core_report.items);

    let native = lock.provider.native.as_ref();
    let out = VerifyReport {
        ok: true,
        manifest: loaded.path.display().to_string(),
        lock: loaded.lock_path().display().to_string(),
        generated_at: lock.generated_at.clone(),
        manifest_matches,
        ros_snapshot: native.and_then(|n| n.ros_snapshot.clone()),
        ubuntu_snapshot: native.and_then(|n| n.ubuntu_snapshot.clone()),
        environment: report.clone().into(),
    };

    if global.json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("{}", msg!("cli.verify.manifest_line", path = &out.manifest));
        println!("{}", msg!("cli.verify.lock_line", path = &out.lock));
        println!();
        print_env_report(&report, &ctx.platform.to_string());
    }

    if !out.environment.ready {
        std::process::exit(1);
    }
    Ok(())
}
