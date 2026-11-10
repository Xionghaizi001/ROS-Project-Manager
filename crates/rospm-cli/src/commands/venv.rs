use super::{context, Global};
use anyhow::Result;
use rospm_core::{mirror, probe::Host};

pub fn create(global: &Global, uv: bool, dry_run: bool) -> Result<()> {
    let (loaded, _leases) = super::load_mutating(global, dry_run)?;
    let host = Host::probe();
    let ctx = context(&loaded, &host, false, None, false, mirror::OFFICIAL)?;
    let plan = rospm_core::venv::plan_create(&ctx, uv)?;
    plan.execute(dry_run, &mut std::io::stdout().lock())?;
    Ok(())
}
