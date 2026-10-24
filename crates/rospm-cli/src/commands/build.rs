//! Build command implementation.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context as _, Result};

use rospm_core::manifest::CondaBuild;
use rospm_core::mirror;
use rospm_core::plan::{Plan, Step};
use rospm_core::probe::Host;
use rospm_core::provider;
use rospm_core::provider::native::{colcon_bases, existing_base};
use rospm_core::{Error, Message};

use crate::i18n::tr;

use super::{context, load, Global, Loaded};

pub fn build(
    global: &Global,
    workspace: Option<&str>,
    conda_env: Option<String>,
    conda_prefix: Option<PathBuf>,
    colcon_args: Vec<String>,
    no_env_check: bool,
    dry_run: bool,
) -> Result<()> {
    let mut loaded = load(global)?;
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
        global.json,
    )
}

/// `--no-env-check`: the user vouches for the Conda environment.
pub(super) fn skip_env_checks(plan: &mut Plan) {
    for step in &mut plan.steps {
        if let Step::Conda(conda) = step {
            conda.skip_checks();
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_build(
    loaded: &Loaded,
    host: &Host,
    workspace: Option<&str>,
    verify: bool,
    no_env_check: bool,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    let ctx = context(loaded, host, false, None, false, mirror::OFFICIAL)?;
    let mut plan = provider::for_manifest(&loaded.manifest).plan_build(&ctx, workspace, verify)?;
    if no_env_check {
        skip_env_checks(&mut plan);
    }
    let mut out = std::io::stdout().lock();
    let started = std::time::SystemTime::now();
    let Err(err) = plan.execute(dry_run, &mut out) else {
        out.flush()?;
        return Ok(());
    };
    out.flush()?;
    // Fallback for a failed build: stale outputs (caches naming another
    // interpreter, half-built packages) are a common cause, so offer one
    // clean rebuild. Only interactively, and only with the user's consent.
    let stale_cache =
        matches!(&err, Error::Host(Message::Key { key, .. }) if *key == "conda.stale_cache");
    let colcon_failed = matches!(&err, Error::CommandFailed { command, .. } if (command.contains("colcon build") || command.contains("colcon --log-base")));
    let workspaces: Vec<_> = match workspace {
        Some(name) => loaded.manifest.workspaces.get(name).into_iter().collect(),
        None => loaded.manifest.workspaces.values().collect(),
    };
    // Configured base first, else colcon's default; neither existing: nothing to offer.
    let dirs: Vec<PathBuf> = workspaces
        .iter()
        .flat_map(|ws| {
            [("build-base", "build"), ("install-base", "install")]
                .map(|(key, default)| existing_base(colcon_bases(&loaded.root, ws, key, default)))
        })
        .flatten()
        .collect();
    let stdin = std::io::stdin();
    if json || !(stale_cache || colcon_failed) || dirs.is_empty() || !stdin.is_terminal() {
        return Err(build_failure(&plan, err, started));
    }
    eprintln!("{}", tr("cli.build.clean_offer"));
    for dir in &dirs {
        eprintln!("  {}", dir.display());
    }
    eprint!("{}", tr("cli.build.clean_confirm"));
    let mut answer = String::new();
    stdin.read_line(&mut answer)?;
    if !matches!(answer.trim(), "y" | "Y" | "yes") {
        return Err(build_failure(&plan, err, started));
    }
    for dir in &dirs {
        // Does not follow symlinks: `--symlink-install` links into sources stay intact.
        std::fs::remove_dir_all(dir).with_context(|| dir.display().to_string())?;
    }
    writeln!(out, "# {}", tr("cli.build.clean_retry"))?;
    let started = std::time::SystemTime::now();
    plan.perform(&mut out)
        .map_err(|err| build_failure(&plan, err, started))?;
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
