//! Shared confirmation rules. Frontends display requests; these functions keep
//! the reviewed scope borrowed until the approved operation has run.
use crate::{
    execution::{Confirmation, Interaction, Operation},
    mirror::Mirror,
    msg,
    plan::{Command, Plan, Step},
    provider::{
        native::{colcon_bases, existing_base},
        Context,
    },
    Error, Result,
};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

fn commands(plan: &Plan) -> Vec<Command> {
    plan.steps
        .iter()
        .filter_map(|step| match step {
            Step::Run(command) => Some(command.clone()),
            _ => None,
        })
        .collect()
}

/// Only called for an automatically proposed mirror. Explicit --mirror choices
/// and their reachability checks do not require another confirmation.
pub fn switch_mirror(
    task: &str,
    mirror: Mirror,
    candidates: &[Mirror],
    ros_snapshot: bool,
    interaction: &dyn Interaction,
) -> Result<Mirror> {
    let request = Confirmation {
        task: task.into(),
        step: 0,
        operation: Operation::SwitchMirror {
            name: mirror.name.into(),
            ros_apt: (!ros_snapshot).then(|| mirror.ros_apt.into()),
            rosdistro: mirror.rosdistro_base().into(),
        },
        prompt: msg!(
            "cli.apply.confirm_mirror",
            name = mirror.name,
            label = mirror.label
        ),
        explanation: None,
        unavailable: Some(msg!(
            "cli.apply.need_mirror_flag",
            best = mirror.name,
            names = candidates
                .iter()
                .map(|m| m.name)
                .collect::<Vec<_>>()
                .join(" | ")
        )),
    };
    if crate::execution::confirm(&request, interaction)? {
        Ok(mirror)
    } else {
        Err(Error::Host(msg!("cli.apply.aborted")))
    }
}

/// `yes` represents explicit authorization such as CLI --yes, never a timeout.
/// The same immutable plan that was reviewed is passed to the executor.
pub fn uninstall(
    task: &str,
    plan: &Plan,
    yes: bool,
    interaction: &dyn Interaction,
    execute: impl FnOnce(&Plan) -> Result<()>,
) -> Result<()> {
    if plan.action_count() == 0 {
        return Ok(());
    }
    // Native uninstall currently consists only of commands and notes. Fail
    // closed if a provider introduces an operation this review cannot describe.
    if plan
        .steps
        .iter()
        .any(|s| !matches!(s, Step::Run(_) | Step::Note(_)))
    {
        return Err(Error::Host(msg!("confirmation.unsupported_plan")));
    }
    let request = Confirmation {
        task: task.into(),
        step: plan
            .steps
            .iter()
            .position(|s| matches!(s, Step::Run(_)))
            .unwrap()
            + 1,
        operation: Operation::Uninstall {
            commands: commands(plan),
        },
        prompt: msg!("cli.uninstall.confirm"),
        explanation: None,
        unavailable: Some(msg!("cli.uninstall.needs_confirmation")),
    };
    if !yes && !crate::execution::confirm(&request, interaction)? {
        return Err(Error::Host(msg!("cli.uninstall.aborted")));
    }
    crate::execution::checkpoint(task, request.step, interaction)?;
    execute(plan)
}

/// Offer exactly one clean retry for the same failures the CLI has supported.
/// False means no eligible directories or no approval; preserve the first error.
pub fn retry_build(
    task: &str,
    plan: &Plan,
    ctx: &Context<'_>,
    workspace: Option<&str>,
    error: &Error,
    interaction: &dyn Interaction,
    execute: impl FnOnce(&Plan) -> Result<()>,
) -> Result<bool> {
    let failed = plan.steps.iter().position(|step| match (step, error) {
        (Step::Conda(_), Error::Host(message)) => message.key_name() == Some("conda.stale_cache"),
        (
            Step::Run(command),
            Error::CommandFailed {
                command: failed, ..
            },
        ) => {
            command.render() == *failed
                && (failed.contains("colcon build") || failed.contains("colcon --log-base"))
        }
        _ => false,
    });
    let Some(failed) = failed else {
        return Ok(false);
    };
    let mut dirs = Vec::new();
    for (name, ws) in &ctx.manifest.workspaces {
        if workspace.is_some_and(|selected| name != selected) {
            continue;
        }
        for (key, default) in [("build-base", "build"), ("install-base", "install")] {
            if let Some(path) = existing_base(colcon_bases(ctx.root, ws, key, default)) {
                let path: PathBuf = path.components().collect();
                let metadata = fs::symlink_metadata(&path).map_err(|e| Error::io(&path, e))?;
                if metadata.file_type().is_symlink() {
                    return Err(unsafe_directory(&path));
                }
                let path = path.canonicalize().map_err(|e| Error::io(&path, e))?;
                if dirs.iter().any(|(p, _, _)| p == &path) {
                    continue;
                }
                validate_directory(ctx, &path)?;
                dirs.push((path, metadata.dev(), metadata.ino()));
            }
        }
    }
    if dirs.is_empty() {
        return Ok(false);
    }
    // An outer directory already includes its descendants; delete each once.
    let paths: Vec<PathBuf> = dirs
        .iter()
        .filter(|(path, _, _)| {
            !dirs
                .iter()
                .any(|(parent, _, _)| path != parent && path.starts_with(parent))
        })
        .map(|(path, _, _)| path.clone())
        .collect();
    let request = Confirmation {
        task: task.into(),
        step: failed + 1,
        operation: Operation::CleanBuild {
            paths: paths.clone(),
            retry: commands(plan),
        },
        prompt: msg!("cli.build.clean_confirm"),
        explanation: Some(msg!("cli.build.clean_offer")),
        unavailable: None,
    };
    if !crate::execution::confirm(&request, interaction)? {
        return Ok(false);
    }
    // Validate every reviewed directory before deleting any of them.
    for (path, dev, ino) in &dirs {
        let metadata = fs::symlink_metadata(path).map_err(|e| Error::io(path, e))?;
        if !metadata.is_dir()
            || metadata.dev() != *dev
            || metadata.ino() != *ino
            || path.canonicalize().map_err(|e| Error::io(path, e))? != *path
        {
            return Err(Error::Host(msg!(
                "confirmation.directory_changed",
                path = path.display()
            )));
        }
    }
    crate::execution::checkpoint(task, request.step, interaction)?;
    for path in paths {
        fs::remove_dir_all(&path).map_err(|e| Error::io(&path, e))?;
    }
    crate::execution::checkpoint(task, request.step, interaction)?;
    execute(plan)?;
    Ok(true)
}

fn unsafe_directory(path: &Path) -> Error {
    Error::Host(msg!("confirmation.unsafe_directory", path = path.display()))
}

fn validate_directory(ctx: &Context<'_>, path: &Path) -> Result<()> {
    let root = ctx
        .root
        .canonicalize()
        .map_err(|e| Error::io(ctx.root, e))?;
    if root.starts_with(path) {
        return Err(unsafe_directory(path));
    }
    for ws in ctx.manifest.workspaces.values() {
        let workspace = root.join(&ws.path);
        if let Ok(workspace) = workspace.canonicalize() {
            if workspace.starts_with(path) || path.starts_with(workspace.join("src")) {
                return Err(unsafe_directory(path));
            }
            if let Ok(source) = workspace.join("src").canonicalize() {
                if source.starts_with(path) || path.starts_with(source) {
                    return Err(unsafe_directory(path));
                }
            }
        }
    }
    Ok(())
}
