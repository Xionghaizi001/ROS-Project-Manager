use super::{context, load, Global};
use anyhow::{Context as _, Result};
use rospm_core::{mirror, probe::Host, worker::WorkspaceLock};

pub fn create(global: &Global, uv: bool, dry_run: bool) -> Result<()> {
    let loaded = load(global)?;
    let host = Host::probe();
    let ctx = context(&loaded, &host, false, None, false, mirror::OFFICIAL)?;
    let plan = rospm_core::venv::plan_create(&ctx, uv)?;
    let _lease = if dry_run {
        None
    } else {
        Some(
            WorkspaceLock::try_acquire(loaded.root.join(".rospm/worker"), &loaded.root)?
                .context(crate::i18n::tr("worker_cli.already_running"))?,
        )
    };
    plan.execute(dry_run, &mut std::io::stdout().lock())?;
    Ok(())
}
