//! `rospm verify` for the native provider (spec §4, roadmap P2).
//!
//! Compares a [`Lock`] with the host: source revisions, apt versions, pip
//! versions, manifest hash. Read-only; produces an [`EnvReport`] like `check`.
//!
//! The manifest-hash row needs the raw `rospm.toml` text, which only the CLI
//! has on hand (spec §4.2: core never touches files the caller resolved), so
//! the CLI's `commands::verify` prepends it. Everything else — provider kind,
//! OS/snapshot dates, source revisions, apt, pip — is built here.

use std::collections::BTreeMap;

use crate::error::Result;
use crate::lock::{Lock, LockedApt};
use crate::message::Message;
use crate::msg;
use crate::probe::{self, EnvReport, Status};
use crate::provider::Context;
use crate::repos::same_git_url;

pub fn verify(ctx: &Context, lock: &Lock) -> Result<EnvReport> {
    let mut r = EnvReport::default();
    let m = ctx.manifest;
    let host = ctx.host;

    // ---- provider kind: the lock must have been resolved for the same provider ----
    let want = m.environment.provider;
    let have = lock.provider.kind;
    r.push(
        "manifest",
        msg!("verify.provider.subject"),
        want.to_string(),
        have.to_string(),
        if have == want {
            Status::Ok
        } else {
            Status::Mismatch
        },
    );

    // ---- os + snapshot dates (native lock only; nothing to check for pixi/docker) ----
    if let Some(native) = &lock.provider.native {
        match &host.os {
            Some(os) if os.is_ubuntu() => {
                let have_ver = os.version_id.clone().unwrap_or_else(|| "?".into());
                let status = if have_ver == native.ubuntu {
                    Status::Ok
                } else {
                    Status::Mismatch
                };
                r.push(
                    "os",
                    Message::text("ubuntu"),
                    native.ubuntu.clone(),
                    have_ver,
                    status,
                );
            }
            Some(os) => r.push(
                "os",
                Message::text("ubuntu"),
                native.ubuntu.clone(),
                os.pretty_name
                    .clone()
                    .map(Message::text)
                    .unwrap_or_else(|| msg!("probe.os.not_ubuntu")),
                Status::Mismatch,
            ),
            None => r.push(
                "os",
                Message::text("ubuntu"),
                native.ubuntu.clone(),
                msg!("probe.os.unreadable"),
                Status::Unknown,
            ),
        }
        // These are informational: verify does not check what apt is currently
        // pointed at (that is `check`'s job), only what the lock recorded.
        r.push(
            "os",
            msg!("verify.snapshot.ros_subject"),
            Message::text("-"),
            match &native.ros_snapshot {
                Some(d) => Message::text(d.clone()),
                None => msg!("verify.snapshot.degraded"),
            },
            Status::Info,
        );
        r.push(
            "os",
            msg!("verify.snapshot.ubuntu_subject"),
            Message::text("-"),
            match &native.ubuntu_snapshot {
                Some(d) => Message::text(d.clone()),
                None => msg!("verify.snapshot.degraded"),
            },
            Status::Info,
        );
    }

    // ---- sources: locked git revisions vs. what is actually checked out ----
    for src in &lock.sources {
        let subject = format!("{}: src/{}", src.workspace, src.name);
        let Some(ws) = m.workspaces.get(&src.workspace) else {
            r.push(
                "source",
                subject,
                src.url.clone(),
                msg!(
                    "verify.source.workspace_missing",
                    workspace = &src.workspace
                ),
                Status::Mismatch,
            );
            continue;
        };
        if src.kind != "git" {
            r.push(
                "source",
                subject,
                src.url.clone(),
                msg!("verify.source.not_verified_other", kind = &src.kind),
                Status::Info,
            );
            continue;
        }
        let expected = format!("{} @ {}", src.url, short(&src.rev));
        let dir = ctx.root.join(&ws.path).join("src").join(&src.name);
        if !dir.is_dir() {
            r.push(
                "source",
                subject,
                expected,
                msg!("probe.source.not_imported"),
                Status::Missing,
            );
            continue;
        }
        if !probe::is_git_checkout(&dir) {
            r.push(
                "source",
                subject,
                expected,
                msg!("probe.source.not_a_checkout"),
                Status::Mismatch,
            );
            continue;
        }
        if !host.has_tool("git") {
            r.push(
                "source",
                subject,
                expected,
                msg!("verify.source.git_unavailable"),
                Status::Unknown,
            );
            continue;
        }
        match host.git_head(&dir) {
            None => r.push(
                "source",
                subject.clone(),
                expected,
                msg!("verify.source.head_unknown"),
                Status::Unknown,
            ),
            Some(head) => {
                if head.eq_ignore_ascii_case(&src.rev) {
                    r.push(
                        "source",
                        subject.clone(),
                        expected,
                        msg!("verify.source.matches", head = short(&head)),
                        Status::Ok,
                    );
                } else {
                    r.push(
                        "source",
                        subject.clone(),
                        expected,
                        msg!(
                            "verify.source.rev_mismatch",
                            head = short(&head),
                            expected = short(&src.rev)
                        ),
                        Status::Mismatch,
                    );
                }
            }
        }
        if let Some(url) = host.git_remote_url(&dir) {
            if !same_git_url(&url, &src.url) {
                r.push(
                    "source",
                    subject,
                    src.url.clone(),
                    msg!("probe.source.origin_differs", url = url),
                    Status::Mismatch,
                );
            }
        }
    }

    // ---- apt: only entries for the host platform are checkable here ----
    let mut for_host: Vec<&LockedApt> = Vec::new();
    let mut other_platforms: BTreeMap<String, usize> = BTreeMap::new();
    for entry in &lock.apt {
        if entry.platform == ctx.platform {
            for_host.push(entry);
        } else {
            *other_platforms
                .entry(entry.platform.to_string())
                .or_insert(0) += 1;
        }
    }
    for (platform, count) in &other_platforms {
        r.push(
            "apt",
            Message::text(platform.clone()),
            Message::text("-"),
            msg!(
                "verify.apt.other_platform",
                count = *count,
                platform = platform
            ),
            Status::Info,
        );
    }
    // `apply --locked` refuses a lock without the base package for this
    // platform, so verify must not call such a lock ready either.
    let ros_base = m.ros_base_package();
    if !for_host.iter().any(|e| e.name == ros_base) {
        r.push(
            "ros",
            ros_base.clone(),
            msg!("verify.apt.base_not_locked_expected"),
            msg!("verify.apt.base_not_locked", platform = ctx.platform),
            Status::Missing,
        );
    }
    if for_host.is_empty() {
        r.push(
            "apt",
            Message::text("apt"),
            Message::text("-"),
            msg!("verify.apt.none_for_host", platform = ctx.platform),
            Status::Info,
        );
    } else {
        let names: Vec<String> = for_host.iter().map(|e| e.name.clone()).collect();
        match host.packages.status(&names) {
            None => {
                for entry in &for_host {
                    let area = if entry.name == ros_base { "ros" } else { "apt" };
                    r.push(
                        area,
                        entry.name.clone(),
                        entry.version.clone(),
                        msg!("probe.dpkg_unavailable"),
                        Status::Unknown,
                    );
                }
            }
            Some(status) => {
                for entry in &for_host {
                    let area = if entry.name == ros_base { "ros" } else { "apt" };
                    let st = &status[&entry.name];
                    if !st.installed {
                        r.push(
                            area,
                            entry.name.clone(),
                            entry.version.clone(),
                            msg!("probe.not_installed"),
                            Status::Missing,
                        );
                    } else if st.version.as_deref().is_some_and(|have| {
                        !super::lock::pinned_exactly(m, &entry.name)
                            && crate::debver::compare(have, &entry.version)
                                == std::cmp::Ordering::Greater
                    }) {
                        // `apply --locked` keeps a newer Ubuntu package (spec §3.2).
                        r.push(
                            area,
                            entry.name.clone(),
                            entry.version.clone(),
                            msg!(
                                "verify.apt.newer_kept",
                                version = st.version.clone().unwrap_or_default()
                            ),
                            Status::Info,
                        );
                    } else if st.version.as_deref() != Some(entry.version.as_str()) {
                        r.push(
                            area,
                            entry.name.clone(),
                            entry.version.clone(),
                            st.version.clone().unwrap_or_default(),
                            Status::Mismatch,
                        );
                    } else {
                        r.push(
                            area,
                            entry.name.clone(),
                            entry.version.clone(),
                            st.version.clone().unwrap_or_default(),
                            Status::Ok,
                        );
                    }
                }
            }
        }
    }

    // ---- pip: recorded only, never installed (decision #6), still reported as drift ----
    match host.pip_freeze() {
        None => {
            for entry in &lock.pip {
                r.push(
                    "pip",
                    entry.name.clone(),
                    entry.version.clone(),
                    msg!("verify.pip.unavailable"),
                    Status::Unknown,
                );
            }
        }
        Some(freeze) => {
            for entry in &lock.pip {
                let key = probe::pip_requirement_name(&entry.name);
                match freeze.get(&key) {
                    None => r.push(
                        "pip",
                        entry.name.clone(),
                        entry.version.clone(),
                        msg!("probe.not_installed"),
                        Status::Missing,
                    ),
                    Some(v) if v == &entry.version => r.push(
                        "pip",
                        entry.name.clone(),
                        entry.version.clone(),
                        v.clone(),
                        Status::Ok,
                    ),
                    Some(v) => r.push(
                        "pip",
                        entry.name.clone(),
                        entry.version.clone(),
                        v.clone(),
                        Status::Mismatch,
                    ),
                }
            }
        }
    }

    Ok(r)
}

/// First 12 characters of a git SHA, for display.
fn short(sha: &str) -> String {
    sha.get(..12).unwrap_or(sha).to_string()
}
