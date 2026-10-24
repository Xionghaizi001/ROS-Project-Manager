//! Deps command implementation.

use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use rospm_core::mirror;
use rospm_core::msg;
use rospm_core::probe::{EnvReport, Host};
use rospm_core::provider;

use crate::i18n::tr;

use super::apply::requested_mirror;
use super::report::{print_report, EnvSummary};
use super::{context, load, Global};

#[derive(Serialize)]
struct DepsCheckReport {
    ok: bool,
    workspaces: Vec<rospm_core::provider::native::deps::WorkspaceDeps>,
    internal: Vec<String>,
    external: Vec<String>,
    skipped: Vec<String>,
    environment: EnvSummary,
}

fn join_or_none<'a>(keys: impl IntoIterator<Item = &'a String>) -> String {
    let joined = keys.into_iter().cloned().collect::<Vec<_>>().join(", ");
    if joined.is_empty() {
        tr("cli.deps.none")
    } else {
        joined
    }
}

fn print_deps_environment(r: &EnvReport) {
    if r.items.is_empty() {
        println!("{}", tr("cli.deps.env_header"));
        println!("{}", tr("cli.deps.env_empty"));
        return;
    }
    print_report(
        r,
        tr("cli.deps.env_header"),
        "cli.deps.ready",
        "cli.deps.not_ready",
    );
}

/// `rospm deps check`: rosdep keys of every workspace versus the installed set.
pub fn deps_check(global: &Global) -> Result<()> {
    let loaded = load(global)?;
    let host = Host::probe();
    let ctx = context(&loaded, &host, false, None, false, mirror::OFFICIAL)?;
    let report = provider::for_manifest(&loaded.manifest).probe_deps(&ctx)?;

    if global.json {
        let out = DepsCheckReport {
            ok: true,
            workspaces: report.workspaces,
            internal: report.internal.into_iter().collect(),
            external: report.external.into_iter().collect(),
            skipped: report.skipped.into_iter().collect(),
            environment: report.environment.into(),
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    for ws in &report.workspaces {
        println!(
            "{}",
            msg!(
                "cli.deps.workspace_line",
                name = ws.name,
                packages = ws.packages,
                keys = ws.keys
            )
        );
    }
    println!(
        "{}",
        msg!(
            "cli.deps.internal_line",
            keys = join_or_none(&report.internal)
        )
    );
    println!(
        "{}",
        msg!(
            "cli.deps.external_line",
            keys = join_or_none(&report.external)
        )
    );
    if !report.skipped.is_empty() {
        println!(
            "{}",
            msg!(
                "cli.deps.skipped_line",
                keys = join_or_none(&report.skipped)
            )
        );
    }
    println!();
    print_deps_environment(&report.environment);
    Ok(())
}

/// `rospm deps install [--dry-run]`: hand the missing keys to rosdep.
pub fn deps_install(global: &Global, dry_run: bool) -> Result<()> {
    let loaded = load(global)?;
    let host = Host::probe();
    let mirror = requested_mirror(None)?.unwrap_or(mirror::OFFICIAL);
    let ctx = context(&loaded, &host, false, None, false, mirror)?;
    let plan = provider::for_manifest(&loaded.manifest).plan_deps_install(&ctx)?;
    let mut out = std::io::stdout().lock();
    plan.execute(dry_run, &mut out)?;
    out.flush()?;
    Ok(())
}
