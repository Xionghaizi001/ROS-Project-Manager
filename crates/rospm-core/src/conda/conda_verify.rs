//! Verification and replay command construction for Conda.

use super::*;

/// `verify` rows for Conda, kept apart from apt. Only a managed environment
/// has a lock; an existing environment's name or path pins no versions.
pub fn verify(
    config: Option<&CondaBuild>,
    root: &Path,
    host: &CondaHost,
    platform: Platform,
    lock: &Lock,
    r: &mut EnvReport,
) -> Result<()> {
    verify_with(config, root, host, platform, lock, r, &Real)
}

pub(crate) fn verify_with(
    config: Option<&CondaBuild>,
    root: &Path,
    host: &CondaHost,
    platform: Platform,
    lock: &Lock,
    r: &mut EnvReport,
    system: &dyn System,
) -> Result<()> {
    for record in lock.conda.iter().filter(|c| c.platform != platform) {
        r.push(
            "conda",
            record.platform.to_string(),
            crate::Message::text("-"),
            msg!("verify.conda.other_platform", lock = record.lock),
            Status::Info,
        );
    }
    let Some(config) = config else {
        for record in lock.conda.iter().filter(|c| c.platform == platform) {
            r.push(
                "conda",
                record.lock.clone(),
                crate::Message::text("-"),
                msg!("verify.conda.not_declared"),
                Status::Mismatch,
            );
        }
        return Ok(());
    };
    let selection = Selection::from_config(config, root)?;
    let Some(managed) = &config.managed else {
        r.push(
            "conda",
            selection.label(),
            crate::Message::text("-"),
            msg!("verify.conda.external"),
            Status::Info,
        );
        return Ok(());
    };
    let expected = match lock.conda_for(platform) {
        Some(record) => format!(
            "{} ({})",
            record.lock,
            record.lock_sha256.get(..12).unwrap_or(&record.lock_sha256)
        ),
        None => lock_path(managed, platform),
    };
    let step = match CondaStep::new(config, root, host, platform, true, Some(lock)) {
        Ok(step) => step,
        Err(e) => {
            let status = match lock.conda_for(platform) {
                Some(_) => Status::Mismatch,
                None => Status::Missing,
            };
            r.push("conda", selection.label(), expected, e.message(), status);
            return Ok(());
        }
    };
    let managed = step.managed.as_ref().expect("managed configuration");
    let (actual, status) = if managed.is_ready() {
        match Contents::query(&managed.prefix, &step.program, system) {
            Ok(contents) if managed.unchanged(&contents) => {
                (msg!("verify.conda.replayed"), Status::Ok)
            }
            Ok(_) => (msg!("verify.conda.changed"), Status::Mismatch),
            Err(e) => (e.message(), Status::Unknown),
        }
    } else if managed.prefix.join("conda-meta/history").is_file() {
        (msg!("verify.conda.differs"), Status::Mismatch)
    } else {
        (msg!("verify.conda.not_prepared"), Status::Missing)
    };
    r.push("conda", selection.label(), expected, actual, status);
    Ok(())
}

pub(super) fn creation_command(managed: &Preparation, conda: &str) -> Command {
    Command::new(conda)
        .args(["env", "create", "--yes", "--prefix"])
        .arg(managed.prefix.to_string_lossy())
        .arg("--file")
        .arg(managed.definition.to_string_lossy())
        .cwd(&managed.root)
}

/// No solving: `conda create --file` installs exactly the explicit lock and
/// verifies each package digest. Definition variables and PyPI pins, which an
/// explicit lock cannot hold, follow.
pub(super) fn replay_commands(
    managed: &Preparation,
    replay: &Replay,
    selection: &Selection,
    conda: &str,
) -> Vec<Command> {
    let mut commands = vec![Command::new(conda)
        .args(["create", "--yes", "--prefix"])
        .arg(managed.prefix.to_string_lossy())
        .arg("--file")
        .arg(replay.lock.to_string_lossy())
        .cwd(&managed.root)];
    if !managed.variables.is_empty() {
        commands.push(
            Command::new(conda)
                .args(["env", "config", "vars", "set", "--prefix"])
                .arg(managed.prefix.to_string_lossy())
                .args(managed.variables.iter().map(|(k, v)| format!("{k}={v}")))
                .cwd(&managed.root),
        );
    }
    if !replay.pip.is_empty() {
        commands.push(
            selection
                .command(conda)
                .args(["python", "-m", "pip", "install", "--no-deps", "--no-input"])
                .arg("--disable-pip-version-check")
                .args(replay.pip.iter().cloned())
                .cwd(&managed.root),
        );
    }
    commands
}
