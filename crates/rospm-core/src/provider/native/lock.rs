//! `rospm lock` for the native provider (spec §3, roadmap P2).
//!
//! Resolves what the host currently has — source checkouts, installed apt
//! versions, pip versions, the snapshot dates the apt state corresponds to —
//! into a [`Lock`]. Read-only on the system. Everything that could not be
//! recorded is a note in the [`Resolution`], never a silent omission
//! (derived principle 4).

use std::collections::{BTreeMap, BTreeSet};
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

/// Packages `apply --locked` installs at exactly the locked version, even
/// when that is a downgrade: the ROS layer (`ros-<distro>-*`, the base
/// package) and the tooling from the ROS repository. Everything else is an
/// Ubuntu package whose installed version may legitimately be newer (security
/// updates; pinning it would also pin its libraries, see spec §3.2), so a
/// newer installed version is kept and `verify` reports it as info.
pub(crate) fn pinned_exactly(m: &crate::manifest::Manifest, name: &str) -> bool {
    name.starts_with(&format!("ros-{}-", m.project.ros_distro))
        || name == m.ros_base_package()
        || TOOLING.contains(&name)
}

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

    // The apt layer is this host's dpkg state: it only describes the manifest's
    // Ubuntu release if the host runs it (same rule as `apply`).
    let ubuntu = m.ubuntu();
    match host.ubuntu_matches(&ubuntu) {
        Some(true) => {}
        Some(false) => {
            let have = host
                .os
                .as_ref()
                .and_then(|os| os.pretty_name.clone())
                .unwrap_or_else(|| msg!("native.unknown_os").to_string());
            return Err(Error::Host(msg!(
                "native.apply.wrong_ubuntu",
                ubuntu = ubuntu,
                distro = m.project.ros_distro,
                have = have
            )));
        }
        None => notes.push(msg!("lock.os_release_unreadable", ubuntu = ubuntu)),
    }

    let sources = resolve_sources(ctx, &mut notes)?;
    let apt = resolve_apt(ctx, &mut notes)?;
    let pip = resolve_pip(host, &m.dependencies.pip, &mut notes);
    let native = resolve_snapshots(ctx, fetch, &apt, &mut notes);

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
    let apt = m.resolve_apt(ctx.platform);
    notes.extend(apt.replacements.iter().map(|r| r.notice(ctx.platform)));
    wanted.extend(apt.packages);
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

/// The snapshot pair that serves what this host has installed (spec §3.3):
///
/// - ROS: the newest of the recent snapshots (signature verified) whose
///   index carries every locked `ros-<distro>-*` version. None of them does
///   when the host installed from the live repository after the newest
///   snapshot; the lock then records no ROS snapshot (DEGRADED, with the
///   reason) instead of one `apply --locked` could not install from.
/// - Ubuntu: the instant the host's dpkg state last changed (falls back to
///   the ROS snapshot's `Release` date when unknown). The remaining locked
///   packages are looked up in its indexes; any that are missing are listed.
///
/// Anything that fails is a `None` plus a DEGRADED note.
fn resolve_snapshots(
    ctx: &Context,
    fetch: Option<&dyn Fetch>,
    apt: &[LockedApt],
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
    let ros_prefix = format!("ros-{distro}-");
    let pins: Vec<(String, String)> = apt
        .iter()
        .filter(|a| a.platform == ctx.platform)
        .map(|a| (a.name.clone(), a.version.clone()))
        .collect();
    let ros_pins: Vec<(String, String)> = pins
        .iter()
        .filter(|(n, _)| n.starts_with(&ros_prefix))
        .cloned()
        .collect();

    // ---- ROS: newest verified snapshot that carries the installed versions ----
    let mut ros_index: BTreeSet<(String, String)> = BTreeSet::new();
    let found = snapshot::discover_ros(fetch, distro, codename, arch, &mut |s| {
        let url = snapshot::packages_url(&s.base, codename, "main", arch);
        let index = snapshot::fetch_packages(fetch, &url)?;
        let missing = missing_from(&ros_pins, &index);
        if !missing.is_empty() {
            return Err(msg!(
                "lock.snapshot_lacks_versions",
                count = missing.len(),
                base = &s.base,
                examples = examples(&missing)
            ));
        }
        ros_index = index;
        Ok(())
    });
    let ros = match found {
        Ok(s) => {
            notes.push(msg!(
                "lock.ros_snapshot_recorded",
                date = s.date,
                base = s.base,
                signature = describe_signature(&s.signature)
            ));
            native.ros_snapshot = Some(s.date.clone());
            Some(s)
        }
        Err(e) => {
            notes.push(msg!("lock.ros_snapshot_failed", error = e));
            None
        }
    };

    // ---- Ubuntu: the archive as of the host's last dpkg change ----
    let ts = ctx
        .host
        .dpkg_changed_at
        .map(snapshot::ubuntu_timestamp_from_unix)
        .or_else(|| {
            ros.as_ref()
                .and_then(|r| r.release.date.as_deref())
                .and_then(snapshot::ubuntu_timestamp_from_date)
        });
    let Some(ts) = ts else {
        notes.push(msg!(
            "lock.ubuntu_snapshot_failed",
            error = msg!("lock.ubuntu_timestamp_unknown")
        ));
        return native;
    };
    let base = match snapshot::discover_ubuntu(
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
            u.base
        }
        Err(e) => {
            notes.push(msg!("lock.ubuntu_snapshot_failed", error = e));
            return native;
        }
    };

    // ---- every other locked package must be in one of the two snapshots ----
    // ROS packages were settled above (either all present or no ROS snapshot).
    let mut rest: Vec<(String, String)> = pins
        .into_iter()
        .filter(|(n, _)| !n.starts_with(&ros_prefix))
        .filter(|p| !ros_index.contains(p))
        .collect();
    if let Err((url, e)) = retain_missing_from_ubuntu(fetch, &base, codename, arch, &mut rest) {
        notes.push(msg!("lock.snapshot_check_failed", url = url, error = e));
        return native;
    }
    if !rest.is_empty() {
        notes.push(msg!(
            "lock.not_in_snapshots",
            count = rest.len(),
            examples = examples(&rest)
        ));
    }
    native
}

/// Drop from `rest` every pin some index of the Ubuntu snapshot at `base`
/// carries. Suites newest first, `main` first, stopping as soon as nothing
/// is left: `universe` alone is tens of megabytes.
fn retain_missing_from_ubuntu(
    fetch: &dyn Fetch,
    base: &str,
    codename: &str,
    arch: &str,
    rest: &mut Vec<(String, String)>,
) -> std::result::Result<(), (String, Message)> {
    for suite in [
        format!("{codename}-security"),
        format!("{codename}-updates"),
        codename.to_string(),
    ] {
        for component in ["main", "universe", "restricted", "multiverse"] {
            if rest.is_empty() {
                return Ok(());
            }
            let url = snapshot::packages_url(base, &suite, component, arch);
            let index = snapshot::fetch_packages(fetch, &url).map_err(|e| (url, e))?;
            rest.retain(|p| !index.contains(p));
        }
    }
    Ok(())
}

/// Pins whose exact `(name, version)` is not in `index`.
fn missing_from(
    pins: &[(String, String)],
    index: &BTreeSet<(String, String)>,
) -> Vec<(String, String)> {
    pins.iter()
        .filter(|p| !index.contains(*p))
        .cloned()
        .collect()
}

/// `a=1, b=2, c=3, …` — the first few pins, for a one-line note.
fn examples(pins: &[(String, String)]) -> String {
    let mut out: Vec<String> = pins
        .iter()
        .take(3)
        .map(|(n, v)| format!("{n}={v}"))
        .collect();
    if pins.len() > 3 {
        out.push("…".to_string());
    }
    out.join(", ")
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
    use crate::snapshot::Canned;

    #[test]
    fn ubuntu_check_stops_once_every_pin_is_found() {
        let base = "https://snapshot.ubuntu.com/ubuntu/20260911T182146Z";
        let url = |suite: &str, comp: &str| snapshot::packages_url(base, suite, comp, "amd64");
        // Only the first three indexes exist: fetching any later one would
        // be a 404 error, so success proves the search stopped early.
        let fetch = Canned::default()
            .with(
                url("jammy-security", "main"),
                200,
                "Package: curl\nVersion: 2\n",
            )
            .with(url("jammy-security", "universe"), 200, "")
            .with(
                url("jammy-security", "restricted"),
                200,
                "Package: foo\nVersion: 1\n",
            );
        let mut rest = vec![
            ("curl".to_string(), "2".to_string()),
            ("foo".to_string(), "1".to_string()),
        ];
        retain_missing_from_ubuntu(&fetch, base, "jammy", "amd64", &mut rest).unwrap();
        assert!(rest.is_empty());

        // A version no index carries: every index is read, then it remains.
        let mut all = Canned::default();
        for suite in ["jammy-security", "jammy-updates", "jammy"] {
            for comp in ["main", "universe", "restricted", "multiverse"] {
                all = all.with(url(suite, comp), 200, "Package: curl\nVersion: 3\n");
            }
        }
        let mut rest = vec![("curl".to_string(), "1".to_string())];
        retain_missing_from_ubuntu(&all, base, "jammy", "amd64", &mut rest).unwrap();
        assert_eq!(rest, vec![("curl".to_string(), "1".to_string())]);

        // An index that cannot be read is an error naming its URL.
        let mut rest = vec![("curl".to_string(), "1".to_string())];
        let (bad, _) =
            retain_missing_from_ubuntu(&Canned::default(), base, "jammy", "amd64", &mut rest)
                .unwrap_err();
        assert_eq!(bad, url("jammy-security", "main"));
    }

    #[test]
    fn examples_lists_three_then_an_ellipsis() {
        let pins: Vec<(String, String)> =
            (1..=4).map(|i| (format!("p{i}"), format!("{i}"))).collect();
        assert_eq!(examples(&pins[..2]), "p1=1, p2=2");
        assert_eq!(examples(&pins), "p1=1, p2=2, p3=3, …");
    }

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
