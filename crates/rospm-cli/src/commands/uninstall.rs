//! Uninstall command implementation.

use std::io::Write;

use anyhow::Result;

use rospm_core::mirror;
use rospm_core::probe::Host;
use rospm_core::provider;

use crate::i18n::tr;

use super::{context, load, Global};

/// Removes packages, so unlike `apply` it asks first: `--yes` skips the
/// prompt, and without a terminal on stdin the prompt cannot be answered.
pub fn uninstall(global: &Global, keep_source: bool, yes: bool, dry_run: bool) -> Result<()> {
    let loaded = load(global)?;
    let host = Host::probe();
    let ctx = context(&loaded, &host, false, None, false, mirror::OFFICIAL)?;
    let plan = provider::for_manifest(&loaded.manifest).plan_uninstall(&ctx, keep_source)?;
    let mut out = std::io::stdout().lock();
    if dry_run {
        plan.execute(true, &mut out)?;
        out.flush()?;
        return Ok(());
    }
    write!(out, "{}", plan.render())?;
    out.flush()?;
    if plan.action_count() == 0 {
        println!("{}", tr("cli.uninstall.nothing_to_do"));
        return Ok(());
    }
    rospm_core::confirmation::uninstall(
        &super::interaction::task_id(),
        &plan,
        yes,
        &super::interaction::Terminal,
        |plan| plan.perform(&mut out),
    )?;
    out.flush()?;
    Ok(())
}
