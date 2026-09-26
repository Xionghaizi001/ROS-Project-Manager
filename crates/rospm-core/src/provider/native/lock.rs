//! `rospm lock` for the native provider (spec §3, roadmap P2).
//!
//! Resolves what the host currently has — source checkouts, installed apt
//! versions, pip versions, the snapshot dates the apt state corresponds to —
//! into a [`Lock`]. Read-only on the system. Everything that could not be
//! recorded is a note in the [`Resolution`], never a silent omission
//! (derived principle 4).

use std::collections::BTreeMap;
use std::path::Path;
use std::process;

use crate::error::{Error, Result};
use crate::lock::{
    self, Lock, LockedApt, LockedNative, LockedPip, LockedProvider, LockedSource, Resolution,
    SUPPORTED_SCHEMA_VERSION,
};
use crate::manifest::ProviderKind;
use crate::message::Message;
use crate::msg;
use crate::platform::ubuntu_codename;
use crate::probe::{pip_requirement_name, Host, PackageDb};
use crate::provider::Context;
use crate::repos::{same_git_url, ReposFile};
use crate::scm::{Scm, Vcstool};
use crate::snapshot::{self, Fetch, Signature};

/// Packages `apply` installs next to the base metapackage; locked so a
/// replay gets the same tooling.
const TOOLING: [&str; 3] = [
    "python3-rosdep",
    "python3-vcstool",
    "python3-colcon-common-extensions",
];

pub fn resolve(ctx: &Context, fetch: Option<&dyn Fetch>) -> Result<Resolution> {
    let m = ctx.manifest;
    let host = ctx.host;
    let mut notes: Vec<Message> = Vec::new();

    // A host can only resolve its own platform (P5 adds repository queries).
    match host.platform {
        Some(p) if p == ctx.platform => {}
        Some(p) => {
            return Err(Error::Host(msg!(
                "lock.foreign_platform",
                platform = ctx.platform,
                host = p
            )))
        }
        None => {
            return Err(Error::Host(msg!(
                "lock.foreign_platform",
                platform = ctx.platform,
                host = msg!("probe.platform.unsupported")
            )))
        }
    }

    let sources = resolve_sources(ctx, &mut notes)?;
    let apt = resolve_apt(ctx, &mut notes)?;
    let pip = resolve_pip(host, &m.dependencies.pip, &mut notes);
    let native = resolve_snapshots(ctx, fetch, &mut notes);

    let manifest_text = std::fs::read_to_string(ctx.root.join(crate::manifest::MANIFEST_FILE))
        .or_else(|_| {
            // `--manifest` may point elsewhere; the CLI fixes the hash up in
            // that case. Fall back to re-serialising the parsed manifest.
            m.to_toml()
        })?;

    let lock = Lock {
        schema_version: SUPPORTED_SCHEMA_VERSION,
        generated_at: lock::now_rfc3339(),
        manifest_sha256: lock::sha256_hex(&manifest_text),
        provider: LockedProvider {
            kind: ProviderKind::Native,
            native: Some(native),
            pixi: None,
            docker: None,
        },
        sources,
        apt,
        pip,
    };
    Ok(Resolution { lock, notes })
}

// ---------------------------------------------------------------- sources

/// One `LockedSource` per repository declared in each workspace's `.repos`
/// file, revisions from `vcs export --exact` (git as fallback).
fn resolve_sources(ctx: &Context, notes: &mut Vec<Message>) -> Result<Vec<LockedSource>> {
    let m = ctx.manifest;
    let host = ctx.host;
    let mut out = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let has_vcs = host.has_tool("vcs");
    let has_git = host.has_tool("git");
    if !has_vcs && !has_git {
        return Err(Error::Host(msg!("lock.no_vcs_no_git")));
    }
    if !has_vcs {
        notes.push(msg!("lock.vcs_missing"));
    }

    for name in m.build_order()? {
        let ws = &m.workspaces[&name];
        let Some(sources) = &ws.sources else {
            continue;
        };
        let repos = ReposFile::load(&ctx.root.join(sources))?;
        if repos.repositories.is_empty() {
            continue;
        }
        let ws_path = ctx.root.join(&ws.path);
        let exported = if has_vcs {
            vcs_export(host, &ws_path, &name, notes)
        } else {
            None
        };
        for (rel, repo) in &repos.repositories {
            let dir = ws_path.join("src").join(rel);
            if !dir.is_dir() {
                missing.push(format!("{name}: src/{rel}"));
                continue;
            }
            let exact = exported.as_ref().and_then(|e| e.repositories.get(rel));
            let (rev, actual_url) = match exact {
                Some(e) => (e.version.clone(), Some(e.url.clone())),
                None if repo.kind == "git" && has_git => {
                    (host.git_head(&dir), host.git_remote_url(&dir))
                }
                None => (None, None),
            };
            let Some(rev) = rev.filter(|r| !r.is_empty()) else {
                missing.push(format!("{name}: src/{rel}"));
                continue;
            };
            if let Some(actual) = actual_url.filter(|u| !same_git_url(u, &repo.url)) {
                notes.push(msg!(
                    "lock.origin_differs",
                    workspace = name,
                    name = rel,
                    actual = actual,
                    url = repo.url
                ));
            }
            out.push(LockedSource {
                workspace: name.clone(),
                name: rel.clone(),
                kind: repo.kind.clone(),
                url: repo.url.clone(),
                rev,
                version: repo.version.clone(),
            });
        }
    }
    if !missing.is_empty() {
        return Err(Error::Host(msg!(
            "lock.sources_not_imported",
            count = missing.len(),
            list = missing.join(", ")
        )));
    }
    Ok(out)
}

/// `vcs export --exact src` in `ws_path`, parsed. `None` (with a note) when
/// the command fails: the caller falls back to git per repository.
fn vcs_export(
    host: &Host,
    ws_path: &Path,
    workspace: &str,
    notes: &mut Vec<Message>,
) -> Option<ReposFile> {
    let vcs = host.tool("vcs")?;
    let cmd = Vcstool.export_exact(ws_path);
    let out = process::Command::new(vcs)
        .args(&cmd.args)
        .current_dir(ws_path)
        .stdin(process::Stdio::null())
        .stderr(process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        notes.push(msg!(
            "lock.vcs_export_failed",
            workspace = workspace,
            status = out.status
        ));
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    match ReposFile::parse(&text, &ws_path.join("src")) {
        Ok(r) => Some(r),
        Err(e) => {
            notes.push(msg!(
                "lock.vcs_export_failed",
                workspace = workspace,
                status = e
            ));
            None
        }
    }
}

// ---------------------------------------------------------------- apt

/// Every installed `ros-<distro>-*` package plus the base metapackage, the
/// platform's extra apt packages and the tooling, with dpkg versions.
fn resolve_apt(ctx: &Context, notes: &mut Vec<Message>) -> Result<Vec<LockedApt>> {
    let m = ctx.manifest;
    let host = ctx.host;
    let prefix = format!("ros-{}-", m.project.ros_distro);
    let mut names: Vec<String> = host
        .packages
        .installed_with_prefix(&prefix)
        .ok_or_else(|| Error::Host(msg!("lock.dpkg_unavailable")))?;
    let mut wanted = vec![m.ros_base_package()];
    wanted.extend(m.effective_apt(ctx.platform));
    wanted.extend(TOOLING.iter().map(|s| s.to_string()));
    for w in &wanted {
        if !names.contains(w) {
            names.push(w.clone());
        }
    }
    names.sort();
    names.dedup();
    let status = host
        .packages
        .status(&names)
        .ok_or_else(|| Error::Host(msg!("lock.dpkg_unavailable")))?;
    let mut missing: Vec<String> = Vec::new();
    let mut entries: Vec<LockedApt> = Vec::new();
    for name in &names {
        match status.get(name) {
            Some(s) if s.installed => entries.push(LockedApt {
                platform: ctx.platform,
                name: name.clone(),
                version: s.version.clone().unwrap_or_default(),
                sha256: None,
            }),
            _ if wanted.contains(name) => missing.push(name.clone()),
            _ => {}
        }
    }
    if !missing.is_empty() {
        notes.push(msg!("lock.apt_missing", packages = missing.join(" ")));
    }
    if let Some(sums) = apt_cache_sha256(host, &entries) {
        for e in entries.iter_mut() {
            e.sha256 = sums.get(&(e.name.clone(), e.version.clone())).cloned();
        }
    } else if !entries.is_empty() {
        notes.push(msg!("lock.apt_cache_unavailable"));
    }
    lock::sort_apt(&mut entries);
    Ok(entries)
}

/// `apt-cache show <name>=<version> …` → (name, version) → `sha256:<hex>` of
/// the `.deb`, for the entries the local index still lists. Best effort.
fn apt_cache_sha256(
    host: &Host,
    entries: &[LockedApt],
) -> Option<BTreeMap<(String, String), String>> {
    if entries.is_empty() {
        return Some(BTreeMap::new());
    }
    if !matches!(host.packages, PackageDb::DpkgQuery(_)) {
        return None;
    }
    let apt_cache = which::which("apt-cache").ok()?;
    let out = process::Command::new(apt_cache)
        .arg("show")
        .args(entries.iter().map(|e| format!("{}={}", e.name, e.version)))
        .stdin(process::Stdio::null())
        .stderr(process::Stdio::null())
        .output()
        .ok()?;
    Some(parse_apt_cache_show(&String::from_utf8_lossy(&out.stdout)))
}

/// Stanzas of `apt-cache show`: `Package:` / `Version:` / `SHA256:` fields.
pub fn parse_apt_cache_show(text: &str) -> BTreeMap<(String, String), String> {
    let mut out = BTreeMap::new();
    for stanza in text.split("\n\n") {
        let field = |key: &str| {
            stanza
                .lines()
                .find_map(|l| l.strip_prefix(key).map(|v| v.trim().to_string()))
        };
        if let (Some(p), Some(v), Some(s)) =
            (field("Package:"), field("Version:"), field("SHA256:"))
        {
            out.insert((p, v), format!("sha256:{s}"));
        }
    }
    out
}

// ---------------------------------------------------------------- pip

/// Installed versions of the packages named in `[dependencies].pip` only
/// (decision #6: recorded, never installed).
fn resolve_pip(host: &Host, requirements: &[String], notes: &mut Vec<Message>) -> Vec<LockedPip> {
    if requirements.is_empty() {
        return Vec::new();
    }
    let Some(frozen) = host.pip_freeze() else {
        notes.push(msg!("lock.pip_unavailable"));
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut missing = Vec::new();
    for req in requirements {
        let name = pip_requirement_name(req);
        match frozen.get(&name) {
            Some(v) => out.push(LockedPip {
                name,
                version: v.clone(),
            }),
            None => missing.push(name),
        }
    }
    if !missing.is_empty() {
        notes.push(msg!("lock.pip_missing", packages = missing.join(" ")));
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

// ---------------------------------------------------------------- snapshots

/// The snapshot pair that corresponds to "now": newest ROS snapshot serving
/// this codename/arch with a valid signature, and the Ubuntu snapshot at the
/// same instant. Anything that fails is a `None` plus a DEGRADED note.
fn resolve_snapshots(
    ctx: &Context,
    fetch: Option<&dyn Fetch>,
    notes: &mut Vec<Message>,
) -> LockedNative {
    let m = ctx.manifest;
    let ubuntu = m.ubuntu();
    let mut native = LockedNative {
        ubuntu: ubuntu.clone(),
        ros_snapshot: None,
        ubuntu_snapshot: None,
    };
    let Some(fetch) = fetch else {
        notes.push(msg!("lock.network_skipped"));
        return native;
    };
    let Some(codename) = ubuntu_codename(&ubuntu) else {
        notes.push(msg!(
            "lock.ros_snapshot_failed",
            error = msg!("native.apply.no_repo_for_ubuntu", ubuntu = ubuntu)
        ));
        return native;
    };
    let distro = m.project.ros_distro.as_str();
    let arch = ctx.platform.dpkg_arch();
    let ros = match snapshot::discover_ros(fetch, distro, codename, arch) {
        Ok(s) => s,
        Err(e) => {
            notes.push(msg!("lock.ros_snapshot_failed", error = e));
            return native;
        }
    };
    notes.push(msg!(
        "lock.ros_snapshot_recorded",
        date = ros.date,
        base = ros.base,
        signature = describe_signature(&ros.signature)
    ));
    native.ros_snapshot = Some(ros.date.clone());

    let Some(ts) = ros
        .release
        .date
        .as_deref()
        .and_then(snapshot::ubuntu_timestamp_from_date)
    else {
        notes.push(msg!(
            "lock.ubuntu_snapshot_failed",
            error = msg!(
                "snapshot.release_date_unparseable",
                date = ros.release.date.clone().unwrap_or_default()
            )
        ));
        return native;
    };
    match snapshot::discover_ubuntu(
        fetch,
        codename,
        &ts,
        Path::new(snapshot::UBUNTU_ARCHIVE_KEYRING),
    ) {
        Ok(u) => {
            notes.push(msg!(
                "lock.ubuntu_snapshot_recorded",
                timestamp = u.timestamp,
                signature = describe_signature(&u.signature)
            ));
            native.ubuntu_snapshot = Some(u.timestamp);
        }
        Err(e) => notes.push(msg!("lock.ubuntu_snapshot_failed", error = e)),
    }
    native
}

fn describe_signature(sig: &Signature) -> Message {
    match sig {
        Signature::Verified => msg!("snapshot.sig_verified"),
        Signature::Skipped(why) => msg!("snapshot.sig_skipped", reason = why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apt_cache_show_stanzas_map_to_sha256() {
        let text = "Package: ros-jazzy-rclcpp\nVersion: 28.1.5-1noble.20250901.120000\nSHA256: abc123\nDescription: x\n\nPackage: curl\nVersion: 8.5.0-2ubuntu10.6\nFilename: pool/main/c/curl.deb\nSHA256: def456\n\n";
        let m = parse_apt_cache_show(text);
        assert_eq!(
            m[&(
                "ros-jazzy-rclcpp".to_string(),
                "28.1.5-1noble.20250901.120000".to_string()
            )],
            "sha256:abc123"
        );
        assert_eq!(m.len(), 2);
        assert!(parse_apt_cache_show("").is_empty());
    }
}
