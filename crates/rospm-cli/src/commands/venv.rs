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

pub fn install(
    global: &Global,
    selection: super::VenvArgs,
    requirements: &std::path::Path,
    uv: bool,
    dry_run: bool,
) -> Result<()> {
    let (mut loaded, _leases) = super::load_mutating(global, dry_run)?;
    selection.select(&mut loaded)?;
    let requirements = rospm_core::venv::input_path(requirements, &std::env::current_dir()?)?;
    let host = Host::probe();
    let ctx = context(&loaded, &host, false, None, false, mirror::OFFICIAL)?;
    let plan = rospm_core::venv::plan_install(&ctx, &requirements, uv)?;
    plan.execute(dry_run, &mut std::io::stdout().lock())?;
    Ok(())
}
