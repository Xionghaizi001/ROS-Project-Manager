//! Build command implementation.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context as _, Result};

use rospm_core::manifest::CondaBuild;
use rospm_core::mirror;
use rospm_core::plan::{Plan, Step};
use rospm_core::probe::Host;
use rospm_core::provider;
use rospm_core::Error;

use crate::i18n::tr;

use super::{context, Global, Loaded};

#[allow(clippy::too_many_arguments)]
pub fn build(
    global: &Global,
    workspace: Option<&str>,
    venv: super::VenvArgs,
    conda_env: Option<String>,
    conda_prefix: Option<PathBuf>,
    colcon_args: Vec<String>,
    no_env_check: bool,
    dry_run: bool,
) -> Result<()> {
    let (mut loaded, _leases) = super::load_mutating(global, dry_run)?;
    venv.select(&mut loaded)?;
    // After the manifest's build_args, so colcon's last-wins parsing lets the
    // command line override them; never written back.
    if !colcon_args.is_empty() {
        for (name, ws) in &mut loaded.manifest.workspaces {
            if workspace.is_none_or(|w| w == name) {
                ws.colcon.build_args.extend(colcon_args.iter().cloned());
            }
        }
    }
    if conda_env.is_some() || conda_prefix.is_some() {
        let mut conda = CondaBuild {
            name: conda_env,
            prefix: conda_prefix,
            managed: None,
        };
        // Validate before resolving: an empty CLI path must not become cwd.
        conda.validate()?;
        if let Some(prefix) = &mut conda.prefix {
            if prefix.is_relative() {
                *prefix = std::env::current_dir()
                    .context(tr("cli.err.no_cwd"))?
                    .join(&*prefix);
            }
        }
        loaded.manifest.build.conda = Some(conda);
        loaded.manifest.validate()?;
    }
    run_build(
        &loaded,
        &Host::probe(),
        workspace,
        false,
        no_env_check,
        dry_run,
        global,
    )
}

pub fn rebuild(global: &Global) -> Result<()> {
    let (loaded, _leases) = super::load_mutating(global, false)?;
    let host = Host::probe();
    let ctx = context(&loaded, &host, false, None, false, mirror::OFFICIAL)?;
    rospm_core::confirmation::clean_build_artifacts(&ctx)?;
    run_build(&loaded, &host, None, false, false, false, global)
}

/// `--no-env-check`: the user vouches for the Conda environment.
pub(super) fn skip_env_checks(plan: &mut Plan) {
    for step in &mut plan.steps {
        if let Step::Conda(conda) = step {
            conda.skip_checks();
        }
    }
}

/// Caller holds the mutation leases across this build and any clean retry.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_build(
    loaded: &Loaded,
    host: &Host,
    workspace: Option<&str>,
    verify: bool,
    no_env_check: bool,
    dry_run: bool,
    global: &Global,
) -> Result<()> {
    let ctx = context(loaded, host, false, None, false, mirror::OFFICIAL)?;
    let mut plan = provider::for_manifest(&loaded.manifest).plan_build(&ctx, workspace, verify)?;
    if no_env_check {
        skip_env_checks(&mut plan);
    }
    let mut out = std::io::stdout().lock();
    let started = std::time::SystemTime::now();
    let Err(err) = global.execute_plan(&plan, dry_run, &mut out) else {
        if !dry_run {
            writeln!(out, "{}", tr("cli.build.success"))?;
        }
        out.flush()?;
        return Ok(());
    };
    out.flush()?;
    if global.json || !global.interactive {
        return Err(build_failure(&plan, err, started));
    }
    let mut retry_started = started;
    let retried = rospm_core::confirmation::retry_build(
        global.task.unwrap_or(&super::interaction::task_id()),
        &plan,
        &ctx,
        workspace,
        &err,
        global.interaction(),
        |plan| {
            writeln!(out, "# {}", tr("cli.build.clean_retry"))
                .map_err(|e| Error::io("<stdout>", e))?;
            retry_started = std::time::SystemTime::now();
            global.perform_plan(plan, &mut out)
        },
    )
    .map_err(|error| build_failure(&plan, error, retry_started))?;
    if !retried {
        return Err(build_failure(&plan, err, started));
    }
    writeln!(out, "{}", tr("cli.build.success"))?;
    out.flush()?;
    Ok(())
}

fn build_failure(plan: &Plan, err: Error, started: std::time::SystemTime) -> anyhow::Error {
    let summary = crate::build_summary::collect(plan, &err, started);
    let error = anyhow::Error::from(err);
    match summary {
        Some(summary) => error.context(summary),
        None => error,
    }
}
