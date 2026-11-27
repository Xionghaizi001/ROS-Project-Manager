//! Deps command implementation.

use std::io::Write;

use anyhow::Result;
use serde::Serialize;

use rospm_core::execution::{Event, Interaction, Stream};
use rospm_core::manifest::{Manifest, ProviderKind};
use rospm_core::mirror;
use rospm_core::msg;
use rospm_core::plan::Plan;
use rospm_core::probe::{EnvReport, Host, RosdepState};
use rospm_core::provider;
use rospm_core::worker::WorkspaceLock;
use rospm_core::Error;

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

struct CheckProgress;

impl Interaction for CheckProgress {
    fn event(&self, event: Event<'_>) {
        if let Event::Log { bytes, .. } = event {
            let _ = std::io::stderr().write_all(bytes);
        }
    }
}

pub(super) fn refresh_rosdep(
    manifest: &Manifest,
    host: &mut Host,
    worker: Option<(&str, &dyn Interaction)>,
) -> Result<()> {
    if manifest.environment.provider != ProviderKind::Native
        || !manifest.dependencies.rosdep.enabled
        || !host.has_tool("rosdep")
        || host.rosdep.updated
        || std::env::var_os("ROSPM_NO_NETWORK").is_some_and(|v| !v.is_empty() && v != "0")
    {
        return Ok(());
    }

    let _cache_lease = WorkspaceLock::acquire_mutation(host.rosdep.cache_dir.join("index"))?;
    host.rosdep = RosdepState::probe(&host.ros_home);
    if host.rosdep.updated {
        return Ok(());
    }

    let distro = manifest.project.ros_distro.as_str();
    let mirror = requested_mirror(None)?.unwrap_or(mirror::OFFICIAL);
    let mut plan = Plan::new(msg!("cli.deps.updating_rosdep", distro = distro));
    plan.run(provider::native::rosdep_update(distro, &mirror).arg("-q"));

    let (task, interaction) = worker.unwrap_or(("rosdep-update", &CheckProgress));
    let log = |message: String| {
        interaction.event(Event::Log {
            task,
            step: 0,
            stream: Stream::Stderr,
            bytes: format!("{message}\n").as_bytes(),
        });
    };
    if interaction.cancel_requested(task) {
        return Err(Error::Cancelled.into());
    }
    log(plan.title.to_string());
    match plan.perform_observed(task, interaction, false, &mut std::io::sink()) {
        Ok(()) => host.rosdep = RosdepState::probe(&host.ros_home),
        Err(Error::Cancelled) => return Err(Error::Cancelled.into()),
        Err(error) => log(msg!("cli.deps.rosdep_update_failed", error = error).to_string()),
    }
    Ok(())
}

/// `rospm deps check`: rosdep keys of every workspace versus the installed set.
pub fn deps_check(global: &Global) -> Result<()> {
    let loaded = load(global)?;
    let _read_leases = loaded.workspace_read_leases()?;
    let mut host = Host::probe();
    refresh_rosdep(&loaded.manifest, &mut host, None)?;
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

/// Install workspace dependencies via rosdep, or a selected ROS debug tool via apt.
pub fn deps_install(global: &Global, dry_run: bool, tool: Option<&str>) -> Result<()> {
    let (loaded, _leases) = super::load_mutating(global, dry_run)?;
    let host = Host::probe();
    let mirror = if tool.is_some() {
        mirror::OFFICIAL
    } else {
        requested_mirror(None)?.unwrap_or(mirror::OFFICIAL)
    };
    let ctx = context(&loaded, &host, false, None, false, mirror)?;
    let plan = match tool {
        Some(tool) => provider::native::deps::plan_debug_tool_install(&ctx, tool)?,
        None => {
            let mut plan = provider::for_manifest(&loaded.manifest).plan_deps_install(&ctx)?;
            super::logging::commands(&mut plan, &loaded.root, "deps");
            plan
        }
    };
    let mut out = std::io::stdout().lock();
    global.execute_plan(&plan, dry_run, &mut out)?;
    out.flush()?;
    Ok(())
}

/// Install the explicit apt and pip components declared by the project.
pub fn components_install(global: &Global, dry_run: bool) -> Result<()> {
    let (loaded, _leases) = super::load_mutating(global, dry_run)?;
    let host = Host::probe();
    let mirror = requested_mirror(None)?.unwrap_or(mirror::OFFICIAL);
    let ctx = context(&loaded, &host, false, None, false, mirror)?;
    let mut plan = provider::for_manifest(&loaded.manifest).plan_components_install(&ctx)?;
    super::logging::commands(&mut plan, &loaded.root, "components");
    let mut out = std::io::stdout().lock();
    global.execute_plan(&plan, dry_run, &mut out)?;
    out.flush()?;
    Ok(())
}
