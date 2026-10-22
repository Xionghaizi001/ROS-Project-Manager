//! Explicit locks: `lock` writes one per platform from a ready managed
//! environment, `apply --locked` checks it while planning and replays it.

use std::fs;
use std::path::Path;

use indexmap::IndexMap;

use super::bootstrap::Bootstrap;
use super::definition::{environment_yaml, spec_name, Definition};
use super::system::{Real, System};
use super::{stamp_head, Contents, Replay, Selection, MANAGED_PREFIX, READY};
use crate::error::{Error, Result};
use crate::lock::{sha256_hex, Lock, LockedConda};
use crate::manifest::{CondaBuild, ManagedConda};
use crate::message::Message;
use crate::msg;
use crate::pixi::conda_platform;
use crate::platform::Platform;
use crate::probe::{pip_requirement_name, CondaHost};

const DEFAULT_LOCK: &str = "conda-{platform}.lock";

/// The explicit lock of `platform`, relative to the manifest directory.
pub fn lock_path(managed: &ManagedConda, platform: Platform) -> String {
    managed
        .lock_file
        .as_deref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| DEFAULT_LOCK.into())
        .replace("{platform}", conda_platform(platform))
}

/// Everything `apply --locked` needs, checked while planning so a missing,
/// stale or incomplete record fails before anything is written. Never falls
/// back to solving the definition.
pub(super) fn replay(
    managed: &ManagedConda,
    definition: &Definition,
    fingerprint: &str,
    lock: &Lock,
    root: &Path,
    platform: Platform,
) -> Result<Replay> {
    let Some(record) = lock.conda_for(platform) else {
        return Err(Error::Host(msg!(
            "conda.lock_record_missing",
            platform = platform,
            path = root.join(lock_path(managed, platform)).display()
        )));
    };
    if record.definition_sha256 != fingerprint {
        return Err(Error::Host(msg!(
            "conda.lock_definition_stale",
            platform = platform
        )));
    }
    let path = root.join(&record.lock);
    let text = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
    let actual = sha256_hex(&text);
    if actual != record.lock_sha256 {
        return Err(Error::Host(msg!(
            "conda.lock_digest_mismatch",
            path = path.display(),
            expected = record.lock_sha256,
            actual = actual
        )));
    }
    let invalid = |error: Message| {
        Error::Host(msg!(
            "conda.lock_invalid",
            path = path.display(),
            error = error
        ))
    };
    let packages = parse_explicit(&text, platform).map_err(invalid)?;
    // The record matched the definition when it was written; these checks
    // catch a lock edited or generated elsewhere.
    let mut missing: Vec<String> = definition
        .conda
        .iter()
        .map(|spec| spec_name(spec).to_string())
        .filter(|name| !packages.contains_key(name))
        .collect();
    let python = packages.get("python");
    if !python
        .is_some_and(|v| v == &managed.python || v.starts_with(&format!("{}.", managed.python)))
    {
        missing.push(format!("python={}", managed.python));
    }
    if !definition.pip.is_empty() && !packages.contains_key("pip") {
        missing.push("pip".into());
    }
    missing.dedup();
    if !missing.is_empty() {
        return Err(Error::Host(msg!(
            "conda.lock_incomplete",
            path = path.display(),
            packages = missing.join(", ")
        )));
    }
    let pip = exact_pins(&definition.pip)
        .map_err(|loose| Error::Host(msg!("conda.pip_unpinned", packages = loose.join(", "))))?;
    Ok(Replay {
        packages_sha256: super::packages_sha256(&text),
        lock: path,
        lock_sha256: record.lock_sha256.clone(),
        pip: pip
            .into_iter()
            .map(|(name, version)| format!("{name}=={version}"))
            .collect(),
    })
}

/// Package name → version of a `conda list --explicit` file for `platform`.
/// Every package needs a digest so that Conda verifies what it downloads.
pub(super) fn parse_explicit(
    text: &str,
    platform: Platform,
) -> std::result::Result<IndexMap<String, String>, Message> {
    let expected = conda_platform(platform);
    let mut declared = None;
    let mut explicit = false;
    let mut packages = IndexMap::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(comment) = line.strip_prefix('#') {
            if let Some(found) = comment.trim().strip_prefix("platform:") {
                declared = Some(found.trim().to_string());
            }
            continue;
        }
        if line == "@EXPLICIT" {
            explicit = true;
            continue;
        }
        if !explicit {
            return Err(msg!("conda.explicit_marker_missing"));
        }
        let (url, _digest) = line
            .rsplit_once('#')
            .filter(|(_, d)| matches!(d.len(), 32 | 64) && d.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| msg!("conda.explicit_digest_missing", line = line))?;
        let file = url.rsplit('/').next().unwrap_or_default();
        let mut parts = file
            .strip_suffix(".conda")
            .or_else(|| file.strip_suffix(".tar.bz2"))
            .unwrap_or_default()
            .rsplitn(3, '-');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(_build), Some(version), Some(name)) if !name.is_empty() => {
                packages.insert(name.to_string(), version.to_string());
            }
            _ => return Err(msg!("conda.explicit_not_package", line = line)),
        }
    }
    match declared {
        Some(found) if found == expected => {}
        found => {
            return Err(msg!(
                "conda.explicit_platform",
                found = found.unwrap_or_else(|| "-".into()),
                expected = expected
            ))
        }
    }
    if packages.is_empty() {
        return Err(msg!("conda.explicit_empty"));
    }
    Ok(packages)
}

/// `(normalized name, version)` for each `name==version`; otherwise every
/// requirement that is not such a pin. Pins are installed without
/// dependencies, so loose ranges, URLs and markers cannot be replayed.
pub(super) fn exact_pins(
    pip: &[String],
) -> std::result::Result<Vec<(String, String)>, Vec<String>> {
    let mut pins = Vec::new();
    let mut loose = Vec::new();
    for requirement in pip {
        let pinned = requirement
            .trim()
            .split_once("==")
            .map(|(name, version)| (name.trim(), version.trim()))
            .filter(|(name, version)| {
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                    && !version.is_empty()
                    && version
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._+!-".contains(&b))
            });
        match pinned {
            Some((name, version)) => pins.push((pip_requirement_name(name), version.to_string())),
            None => loose.push(requirement.clone()),
        }
    }
    if loose.is_empty() {
        Ok(pins)
    } else {
        Err(loose)
    }
}

/// `rospm lock` for the Conda environment: write the explicit lock of a
/// ready managed environment and return its record. Whatever cannot be
/// recorded is a note; `apply --locked` then fails on this platform.
pub fn resolve_lock(
    config: &CondaBuild,
    root: &Path,
    host: &CondaHost,
    platform: Platform,
    notes: &mut Vec<Message>,
) -> Result<Option<LockedConda>> {
    resolve_lock_with(config, root, host, platform, notes, &Real)
}

pub(super) fn resolve_lock_with(
    config: &CondaBuild,
    root: &Path,
    host: &CondaHost,
    platform: Platform,
    notes: &mut Vec<Message>,
    system: &dyn System,
) -> Result<Option<LockedConda>> {
    let selection = Selection::from_config(config, root)?;
    let Some(managed) = &config.managed else {
        notes.push(msg!("conda.lock_external", selection = selection.label()));
        return Ok(None);
    };
    let definition = environment_yaml(managed, root)?;
    let fingerprint = sha256_hex(&definition.contents);
    let prefix = root.join(MANAGED_PREFIX);
    let skipped = |notes: &mut Vec<Message>, reason: Message| {
        notes.push(msg!("conda.lock_skipped", reason = reason));
        Ok(None)
    };
    let ready = fs::read_to_string(root.join(READY)).unwrap_or_default();
    if !prefix.join("conda-meta/history").is_file()
        || ready.lines().next() != Some(&stamp_head(&fingerprint, platform))
    {
        return skipped(
            notes,
            msg!("conda.managed_not_ready", prefix = prefix.display()),
        );
    }
    let pins = match exact_pins(&definition.pip) {
        Ok(pins) => pins,
        Err(loose) => {
            return skipped(
                notes,
                msg!("conda.pip_unpinned", packages = loose.join(", ")),
            )
        }
    };
    let program = match Bootstrap::new(host, platform) {
        Some(b) if b.is_ready() => b.conda().to_string_lossy().into_owned(),
        _ => "conda".to_string(),
    };
    let contents = match Contents::query(&prefix, &program, system) {
        Ok(contents) => contents,
        Err(error) => return skipped(notes, error.message()),
    };
    // Explicit locks hold Conda packages only: every PyPI package in the
    // environment must be one of the pins, at the pinned version.
    let installed = &contents.pypi;
    let mut differences: Vec<String> = installed
        .iter()
        .filter(|p| !pins.contains(p))
        .map(|(name, version)| format!("{name}=={version}"))
        .collect();
    differences.extend(
        pins.iter()
            .filter(|p| !installed.contains(p))
            .map(|(name, version)| format!("{name}=={version} (-)")),
    );
    if !differences.is_empty() {
        return skipped(
            notes,
            msg!("conda.pip_differs", packages = differences.join(", ")),
        );
    }
    let text = &contents.explicit;
    let packages = match parse_explicit(text, platform) {
        Ok(packages) => packages,
        Err(error) => return skipped(notes, error),
    };
    let relative = lock_path(managed, platform);
    let path = root.join(&relative);
    if fs::read_to_string(&path).ok().as_deref() != Some(text) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        crate::plan::write_atomic(&path, text.as_bytes()).map_err(|e| Error::io(&path, e))?;
    }
    // The environment is what the lock describes, so `apply --locked` on
    // this host reuses it instead of refusing to touch the prefix.
    let lock_sha256 = sha256_hex(text);
    let stamp = contents.record(&stamp_head(&fingerprint, platform));
    if ready != stamp {
        let ready = root.join(READY);
        fs::write(&ready, stamp).map_err(|e| Error::io(&ready, e))?;
    }
    notes.push(msg!(
        "conda.lock_recorded",
        path = path.display(),
        count = packages.len(),
        pip = pins.len()
    ));
    Ok(Some(LockedConda {
        platform,
        lock: relative,
        lock_sha256,
        definition_sha256: fingerprint,
    }))
}
