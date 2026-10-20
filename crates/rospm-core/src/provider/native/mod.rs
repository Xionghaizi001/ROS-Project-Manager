//! `native` provider: ROS installed from apt on the host.
//!
//! This is the P1 provider. `apply` starts from a bare Ubuntu: it configures
//! the ROS apt source (keyring + `sources.list.d`, the same mechanism P2 reuses
//! to switch to snapshot repositories) before installing the base metapackage.
//! `--locked` snapshot switching arrives in P2 and is reported as a
//! degradation until then.
//!
//! P2 pieces live in submodules: [`lock`] (`rospm lock`), [`verify`]
//! (`rospm verify`), [`deps`] (`rospm deps`).

pub mod deps;
pub mod lock;
pub mod verify;

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::lock::{Lock, Resolution};
use crate::manifest::{Manifest, ProviderKind};
use crate::message::Message;
use crate::msg;
use crate::package_xml::WorkspacePackages;
use crate::plan::{shell_quote, Command, Plan};
use crate::platform::ubuntu_codename;
use crate::probe::{
    self, EnvReport, Status, DEFAULT_LOCALE_FILE, NATIVE_CORE_TOOLS, ROSDEP_DEFAULT_LIST,
    ROS_APT_KEYRING, ROS_APT_LIST, ROS_APT_NEEDLE, ROS_KEY_FINGERPRINT,
};
use crate::provider::{Context, Provider, Shell};
use crate::repos::{same_git_url, ReposFile};
use crate::scm::{Scm, Vcstool};
use crate::snapshot::Fetch;

pub struct Native;

fn ros_setup_file(manifest: &Manifest, shell: Shell) -> String {
    format!("/opt/ros/{}/setup.{}", manifest.project.ros_distro, shell)
}

fn ws_setup_file(root: &Path, ws_path: &Path, shell: Shell) -> PathBuf {
    root.join(ws_path)
        .join("install")
        .join(format!("setup.{shell}"))
}

/// `source` lines for the ROS base and the given workspaces, guarded so a
/// not-yet-built workspace is skipped instead of aborting.
fn source_lines(ctx: &Context, workspaces: &[String], shell: Shell) -> Vec<String> {
    let mut lines = vec![format!(
        "source {}",
        shell_quote(&ros_setup_file(ctx.manifest, shell))
    )];
    for name in workspaces {
        let ws = &ctx.manifest.workspaces[name];
        let setup = ws_setup_file(ctx.root, &ws.path, shell);
        let q = shell_quote(&setup.to_string_lossy());
        lines.push(format!("[ -f {q} ] && source {q}"));
    }
    lines
}

/// Whether the snapshot `ros2.list` must be (re)written: it is rospm's own
/// file (or absent) and does not already point at `base` with a usable keyring.
/// Returns `(is_own_file, is_foreign_file, need_source)`.
fn ros_snapshot_source_state(ctx: &Context, base: &str) -> (bool, bool, bool) {
    let sources = &ctx.host.apt_sources;
    let is_own_file = sources.ros2.as_deref() == Some(Path::new(ROS_APT_LIST));
    let is_foreign_file = sources.ros2.is_some() && !is_own_file;
    let already_current = is_own_file && sources.ros2_snapshot.as_deref() == Some(base);
    let keyring_ok = sources.snapshot_keyring && !sources.snapshot_keyring_armored;
    let need_source = !is_foreign_file && !(already_current && keyring_ok);
    (is_own_file, is_foreign_file, need_source)
}

/// rospm-owned apt source files that `plan_apply` is about to rewrite, and
/// so can be removed before the initial `apt-get update` without losing
/// anything. Foreign files and files that stay as they are never qualify.
fn stale_own_sources(ctx: &Context, locked: Option<&crate::lock::LockedNative>) -> Vec<String> {
    let host = ctx.host;
    let mut out = Vec::new();
    let own_ros = host.apt_sources.ros2.as_deref() == Some(Path::new(ROS_APT_LIST));
    let ros_date = locked.and_then(|n| n.ros_snapshot.as_deref());
    let rewrite_ros = match ros_date {
        Some(date) => {
            let base =
                crate::snapshot::ros_snapshot_url(ctx.manifest.project.ros_distro.as_str(), date);
            let (is_own, _, need_source) = ros_snapshot_source_state(ctx, &base);
            is_own && need_source
        }
        None => own_ros && host.apt_sources.ros2_mirror != Some(ctx.mirror.name),
    };
    if rewrite_ros {
        out.push(ROS_APT_LIST.to_string());
    }
    if let (Some(current), Some(ts)) = (
        host.apt_sources.ubuntu_snapshot.as_deref(),
        locked.and_then(|n| n.ubuntu_snapshot.as_deref()),
    ) {
        if current != crate::snapshot::ubuntu_snapshot_url(ts) {
            out.push(probe::UBUNTU_SNAPSHOT_LIST.to_string());
        }
    }
    out
}

/// Pin the ROS apt source to a snapshot (`apply --locked`, spec §3.2).
/// Mirrors the P1 rewrite rules: only rospm's own [`ROS_APT_LIST`] is
/// touched — a foreign source file (e.g. `ros2-apt-source`'s `ros2.sources`)
/// is left alone and the plan says so. `--mirror` has no effect on the apt
/// line here: snapshots are not mirrored (rosdistro/rosdep still use it,
/// unaffected by this function).
fn plan_ros_snapshot_source(
    plan: &mut Plan,
    ctx: &Context,
    codename: &str,
    date: &str,
    need_locale: bool,
    need_universe: bool,
) {
    let host = ctx.host;
    let distro = ctx.manifest.project.ros_distro;
    let base = crate::snapshot::ros_snapshot_url(distro.as_str(), date);
    let (is_own_file, is_foreign_file, need_source) = ros_snapshot_source_state(ctx, &base);
    let keyring_ok =
        host.apt_sources.snapshot_keyring && !host.apt_sources.snapshot_keyring_armored;

    if need_locale || need_universe || need_source {
        let mut prereqs: Vec<&str> = Vec::new();
        if need_locale {
            prereqs.push("locales");
        }
        if need_universe {
            prereqs.push("software-properties-common");
        }
        if need_source {
            // gnupg: `gpg --show-keys` verifies the downloaded key's fingerprint.
            prereqs.extend(["curl", "ca-certificates", "gnupg"]);
        }
        if !prereqs.is_empty() {
            plan.run(
                Command::new("apt-get")
                    .args(["install", "-y"])
                    .args(prereqs)
                    .sudo(),
            );
        }
    }
    if need_locale {
        plan.run(
            Command::new("locale-gen")
                .args(["en_US", "en_US.UTF-8"])
                .sudo(),
        );
        // Set LANG only, never LC_ALL (it would pin every category for
        // every user). If the file's non-UTF-8 value came from LC_ALL /
        // LC_CTYPE, drop that variable (bare name = remove) so LANG applies.
        let mut update = Command::new("update-locale").arg("LANG=en_US.UTF-8");
        if let Some(var) = host.locale.system_var.filter(|v| *v != "LANG") {
            update = update.arg(var);
        }
        plan.run(update.sudo());
        let from = match (&host.locale.system_var, &host.locale.system) {
            (Some(var), Some(value)) => format!("{var}={value}"),
            _ if host.locale.system_file => "POSIX".to_string(),
            _ => msg!("native.apply.locale_no_file").to_string(),
        };
        plan.note(msg!(
            "native.apply.locale_switched",
            file = DEFAULT_LOCALE_FILE,
            from = from
        ));
    } else if !host.locale.process_utf8() {
        plan.note(msg!(
            "native.apply.shell_locale_not_utf8",
            value = host.locale.process.as_deref().unwrap_or("POSIX"),
            file = DEFAULT_LOCALE_FILE,
            system = host.locale.system.as_deref().unwrap_or("POSIX")
        ));
    }
    if need_universe {
        plan.run(
            Command::new("add-apt-repository")
                .args(["-y", "universe"])
                .sudo(),
        );
    }

    if is_foreign_file {
        let file = host.apt_sources.ros2.as_ref().unwrap();
        plan.note(msg!(
            "native.apply.source_foreign",
            name = "snapshot",
            file = file.display()
        ));
        plan.note(msg!(
            "native.apply.locked_snapshot_foreign",
            file = file.display()
        ));
    } else if need_source {
        if is_own_file {
            plan.note(msg!(
                "native.apply.locked_snapshot_rewrite",
                file = ROS_APT_LIST,
                to = base
            ));
        }
        if !keyring_ok {
            plan.fetch_key(
                crate::snapshot::keyserver_key_url(crate::snapshot::ROS_SNAPSHOT_KEY_FINGERPRINT),
                crate::snapshot::ROS_SNAPSHOT_KEYRING,
                crate::snapshot::ROS_SNAPSHOT_KEY_FINGERPRINT,
            );
        }
        plan.write_file_sudo(
            ROS_APT_LIST,
            format!(
                "deb [arch={} signed-by={}] {base} {codename} main\n",
                ctx.platform.dpkg_arch(),
                crate::snapshot::ROS_SNAPSHOT_KEYRING,
            ),
        );
        plan.run(Command::new("apt-get").arg("update").sudo());
    } else {
        plan.note(msg!(
            "native.apply.source_already_configured",
            file = ROS_APT_LIST,
            source = base
        ));
    }
}

/// Write rospm's own Ubuntu snapshot source file (`apply --locked`, spec
/// §3.2): a *separate* file from Ubuntu's own `sources.list` / `ubuntu.sources`,
/// which rospm never touches (decision #17). The snapshot only makes the
/// pinned versions installable; [`install_pinned_apt`] installs them explicitly.
/// `timestamp` is `None` when the lock has no Ubuntu snapshot recorded
/// (never locked, or degraded at lock/apply time): the caller still gets a
/// working host, just not a pinned one, and the plan says so.
fn plan_ubuntu_snapshot_source(
    plan: &mut Plan,
    ctx: &Context,
    codename: &str,
    timestamp: Option<&str>,
) {
    let Some(ts) = timestamp else {
        plan.note(msg!("native.apply.locked_degraded", what = "Ubuntu apt"));
        return;
    };
    let host = ctx.host;
    let base = crate::snapshot::ubuntu_snapshot_url(ts);
    if host.apt_sources.ubuntu_snapshot.as_deref() == Some(base.as_str()) {
        plan.note(msg!(
            "native.apply.source_already_configured",
            file = probe::UBUNTU_SNAPSHOT_LIST,
            source = base
        ));
        return;
    }
    let arch = ctx.platform.dpkg_arch();
    let keyring = crate::snapshot::UBUNTU_ARCHIVE_KEYRING;
    let mut contents = String::new();
    for suite in [
        codename.to_string(),
        format!("{codename}-updates"),
        format!("{codename}-security"),
    ] {
        contents.push_str(&format!(
            "deb [arch={arch} signed-by={keyring}] {base} {suite} main restricted universe multiverse\n"
        ));
    }
    if !host.apt_sources.ubuntu_archive_keyring {
        plan.note(msg!(
            "native.apply.locked_ubuntu_keyring_missing",
            keyring = keyring,
            file = probe::UBUNTU_SNAPSHOT_LIST
        ));
    }
    plan.note(msg!(
        "native.apply.locked_ubuntu_snapshot_write",
        file = probe::UBUNTU_SNAPSHOT_LIST
    ));
    plan.write_file_sudo(probe::UBUNTU_SNAPSHOT_LIST, contents);
    // Without this the pinned install below only sees the indexes fetched
    // before the file existed, i.e. the live archive, which no longer carries
    // older `-updates` / `-security` versions (found in the 2026-09-26 run).
    plan.run(Command::new("apt-get").arg("update").sudo());
}

/// Install every pinned apt package for `platform`, exactly and explicitly
/// (`apply --locked`, spec §3.2 point 4): the lock lists the resolved
/// versions, so this replaces both the base-package install and the extra
/// `dependencies.apt` install of the non-locked plan. `--allow-downgrades`:
/// a lock generated earlier may pin an older version than the latest
/// available. Chunked so no single `apt-get install` line names more than
/// 100 packages. Ubuntu packages installed at a newer version are kept
/// (see [`lock::pinned_exactly`]).
fn install_pinned_apt(
    plan: &mut Plan,
    host: &crate::probe::Host,
    m: &Manifest,
    lock: &Lock,
    platform: crate::platform::Platform,
) -> Result<()> {
    let entries: Vec<&crate::lock::LockedApt> =
        lock.apt.iter().filter(|a| a.platform == platform).collect();
    if entries.is_empty() {
        return Err(Error::Host(msg!(
            "native.apply.locked_no_apt_for_platform",
            platform = platform
        )));
    }
    let base_package = m.ros_base_package();
    if !entries.iter().any(|a| a.name == base_package) {
        return Err(Error::Host(msg!(
            "native.apply.locked_base_missing",
            package = base_package
        )));
    }
    // An Ubuntu package the host already has at a newer version is kept:
    // pinning it back would need its exact libraries too, which the lock does
    // not record, and apt fails the whole command (2026-09-26: `curl`).
    let names: Vec<String> = entries.iter().map(|a| a.name.clone()).collect();
    let installed = host.packages.status(&names).unwrap_or_default();
    let mut kept: Vec<String> = Vec::new();
    let mut specs: Vec<String> = Vec::new();
    for a in &entries {
        let newer = installed
            .get(&a.name)
            .filter(|st| st.installed)
            .and_then(|st| st.version.as_deref())
            .filter(|have| crate::debver::compare(have, &a.version) == std::cmp::Ordering::Greater);
        match newer {
            Some(have) if !self::lock::pinned_exactly(m, &a.name) => {
                kept.push(format!("{} {have} > {}", a.name, a.version));
            }
            _ => specs.push(format!("{}={}", a.name, a.version)),
        }
    }
    if !kept.is_empty() {
        plan.note(msg!(
            "native.apply.locked_kept_newer",
            count = kept.len(),
            packages = kept.join(", ")
        ));
    }
    for chunk in specs.chunks(100) {
        plan.run(
            Command::new("apt-get")
                .args(["install", "-y", "--allow-downgrades"])
                .args(chunk.iter().cloned())
                .sudo(),
        );
    }
    Ok(())
}

/// Pin every git [`crate::lock::LockedSource`] of `workspace` to its
/// recorded revision after `vcs import` has fetched the branch (`apply
/// --locked`, spec §3.2 point 4). Non-git sources are not pinned — vcstool's
/// other VCS backends have no equivalent of a detached-HEAD checkout, and
/// rospm records them for `verify` only (roadmap).
fn pin_git_sources(plan: &mut Plan, ws_path: &Path, lock: &Lock, workspace: &str) {
    for src in lock.sources.iter().filter(|s| s.workspace == workspace) {
        if src.kind == "git" {
            let repo = ws_path.join("src").join(&src.name);
            plan.run(
                Command::new("git")
                    .args(["checkout", "--detach", src.rev.as_str()])
                    .cwd(repo),
            );
        } else {
            plan.note(msg!(
                "native.apply.locked_non_git_source_not_pinned",
                workspace = workspace,
                name = src.name.as_str(),
                kind = src.kind.as_str()
            ));
        }
    }
}

impl Provider for Native {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Native
    }

    fn plan_apply(&self, ctx: &Context) -> Result<Plan> {
        let m = ctx.manifest;
        let host = ctx.host;
        let distro = m.project.ros_distro;
        let mut plan = Plan::new(msg!(
            "native.apply.title",
            name = m.project.name,
            distro = distro,
            platform = ctx.platform
        ));

        // `--locked` requires a lock (checked again here, defensively — the
        // CLI already refuses before building the context): a missing or
        // mismatched lock is a user error, never a silent degradation
        // (spec §3.2).
        if ctx.locked && ctx.lock.is_none() {
            return Err(Error::Host(msg!("native.apply.locked_no_lock")));
        }
        let locked_native: Option<(&Lock, &crate::lock::LockedNative)> = match ctx.lock {
            Some(lock) if ctx.locked => {
                if lock.provider.kind != ProviderKind::Native {
                    return Err(Error::Host(msg!(
                        "native.apply.locked_provider_mismatch",
                        lock_kind = lock.provider.kind
                    )));
                }
                let native = lock.provider.native.as_ref().ok_or_else(|| {
                    Error::Host(msg!(
                        "native.apply.locked_provider_mismatch",
                        lock_kind = lock.provider.kind
                    ))
                })?;
                Some((lock, native))
            }
            _ => None,
        };

        if ctx.offline {
            return Err(Error::NotImplemented {
                feature: "apply --offline",
                phase: "P6",
            });
        }

        // 0. Refuse to install onto the wrong Ubuntu: ROS binaries exist for
        // exactly one release per distro, and rospm never changes the OS.
        let ubuntu = m.ubuntu();
        let codename = ubuntu_codename(&ubuntu)
            .ok_or_else(|| Error::Host(msg!("native.apply.no_repo_for_ubuntu", ubuntu = ubuntu)))?;
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
                    distro = distro,
                    have = have
                )));
            }
            None => {
                plan.note(msg!(
                    "native.apply.os_release_unreadable",
                    ubuntu = ubuntu,
                    codename = codename
                ));
            }
        }

        if let Some(conda) = &m.build.conda {
            let lock = locked_native.map(|(lock, _)| lock);
            if lock.is_some() && conda.managed.is_none() {
                plan.note(msg!("native.apply.locked_conda_external"));
            }
            plan.steps.push(crate::plan::Step::Conda(Box::new(
                crate::conda::CondaStep::new(
                    conda,
                    ctx.root,
                    &host.conda,
                    ctx.platform,
                    true,
                    lock,
                )?,
            )));
        }

        let others: Vec<_> = host
            .ros_distros
            .iter()
            .filter(|d| d.as_str() != distro.as_str())
            .cloned()
            .collect();
        if !others.is_empty() {
            plan.note(msg!(
                "native.apply.other_distros_kept",
                distros = others.join(", "),
                distro = distro
            ));
        }

        // 1. Bootstrap the ROS apt source (official install guide, minus the
        // steps this host already satisfies), or — under `--locked` — pin it
        // to the recorded snapshot (spec §3.2). URLs come from the chosen
        // source set (decision #17), except for a snapshot, which has no
        // mirrors and ignores `--mirror` for the apt line (rosdistro/rosdep
        // below still use it).
        let mirror = ctx.mirror;
        if !mirror.is_official() {
            plan.note(msg!(
                "native.apply.mirror_in_use",
                name = mirror.name,
                label = mirror.label
            ));
            if locked_native.is_some() {
                plan.note(msg!(
                    "native.apply.locked_mirror_ignored",
                    name = mirror.name
                ));
            }
        }
        // Only the system default (/etc/default/locale) is ever touched; the
        // caller's own `LANG=C` is not a reason to rewrite it.
        let need_locale = host.locale.needs_fix();
        let need_universe = host.apt_sources.universe != Some(true);
        // A run that failed half-way can leave rospm's own source files
        // unusable (2026-09-26: an armoured snapshot keyring); the initial
        // `apt-get update` would then fail before the steps that repair them.
        // Files the plan rewrites below anyway are removed first.
        let stale = stale_own_sources(ctx, locked_native.map(|(_, n)| n));
        if !stale.is_empty() {
            plan.note(msg!(
                "native.apply.stale_sources_removed",
                files = stale.join(" ")
            ));
            plan.run(Command::new("rm").arg("-f").args(stale).sudo());
        }
        plan.run(Command::new("apt-get").arg("update").sudo());

        let ros_snapshot_date = locked_native.and_then(|(_, n)| n.ros_snapshot.as_deref());
        if let Some(date) = ros_snapshot_date {
            plan_ros_snapshot_source(&mut plan, ctx, codename, date, need_locale, need_universe);
        } else {
            if locked_native.is_some() {
                plan.note(msg!("native.apply.locked_degraded", what = "ROS apt"));
            }
            // Rewrite rospm's own source file when a different source set was
            // chosen; a file written by something else (ros2-apt-source) is left alone.
            let rewrite_source = host
                .apt_sources
                .ros2
                .as_deref()
                .is_some_and(|f| f == Path::new(ROS_APT_LIST))
                && host.apt_sources.ros2_mirror != Some(mirror.name);
            let need_source = host.apt_sources.ros2.is_none() || rewrite_source;
            if need_locale || need_universe || need_source {
                let mut prereqs: Vec<&str> = Vec::new();
                if need_locale {
                    prereqs.push("locales");
                }
                if need_universe {
                    prereqs.push("software-properties-common");
                }
                if need_source {
                    // gnupg: `gpg --show-keys` verifies the downloaded key's fingerprint.
                    prereqs.extend(["curl", "ca-certificates", "gnupg"]);
                }
                plan.run(
                    Command::new("apt-get")
                        .args(["install", "-y"])
                        .args(prereqs)
                        .sudo(),
                );
            }
            if need_locale {
                plan.run(
                    Command::new("locale-gen")
                        .args(["en_US", "en_US.UTF-8"])
                        .sudo(),
                );
                // Set LANG only, never LC_ALL (it would pin every category for
                // every user). If the file's non-UTF-8 value came from LC_ALL /
                // LC_CTYPE, drop that variable (bare name = remove) so LANG applies.
                let mut update = Command::new("update-locale").arg("LANG=en_US.UTF-8");
                if let Some(var) = host.locale.system_var.filter(|v| *v != "LANG") {
                    update = update.arg(var);
                }
                plan.run(update.sudo());
                let from = match (&host.locale.system_var, &host.locale.system) {
                    (Some(var), Some(value)) => format!("{var}={value}"),
                    _ if host.locale.system_file => "POSIX".to_string(),
                    _ => msg!("native.apply.locale_no_file").to_string(),
                };
                plan.note(msg!(
                    "native.apply.locale_switched",
                    file = DEFAULT_LOCALE_FILE,
                    from = from
                ));
            } else if !host.locale.process_utf8() {
                plan.note(msg!(
                    "native.apply.shell_locale_not_utf8",
                    value = host.locale.process.as_deref().unwrap_or("POSIX"),
                    file = DEFAULT_LOCALE_FILE,
                    system = host.locale.system.as_deref().unwrap_or("POSIX")
                ));
            }
            if need_universe {
                plan.run(
                    Command::new("add-apt-repository")
                        .args(["-y", "universe"])
                        .sudo(),
                );
            }
            if need_source {
                if rewrite_source {
                    plan.note(msg!(
                        "native.apply.source_rewrite",
                        file = ROS_APT_LIST,
                        from = host.apt_sources.ros2_mirror.unwrap_or("unknown"),
                        to = mirror.name
                    ));
                }
                plan.fetch_key(mirror.ros_key_url(), ROS_APT_KEYRING, ROS_KEY_FINGERPRINT);
                plan.write_file_sudo(
                    ROS_APT_LIST,
                    format!(
                        "deb [arch={} signed-by={ROS_APT_KEYRING}] {} {codename} main\n",
                        ctx.platform.dpkg_arch(),
                        mirror.ros_apt
                    ),
                );
                plan.run(Command::new("apt-get").arg("update").sudo());
            } else if let Some(file) = &host.apt_sources.ros2 {
                plan.note(msg!(
                    "native.apply.source_already_configured",
                    file = file.display(),
                    source = host.apt_sources.ros2_mirror.unwrap_or("unknown")
                ));
                if host.apt_sources.ros2_mirror != Some(mirror.name) {
                    plan.note(msg!(
                        "native.apply.source_foreign",
                        name = mirror.name,
                        file = file.display()
                    ));
                }
            }
        }

        // 1b. Ubuntu snapshot: a separate, rospm-owned file (`--locked`
        // only; never touches Ubuntu's own `sources.list` / `ubuntu.sources`,
        // decision #17). It only makes the pinned versions installable; step 2
        // below installs them explicitly.
        if let Some((_, native)) = locked_native {
            plan_ubuntu_snapshot_source(
                &mut plan,
                ctx,
                codename,
                native.ubuntu_snapshot.as_deref(),
            );
        }

        // 2. ROS base + tooling, or — under `--locked` — every pinned apt
        // package for this platform, installed explicitly and exactly.
        if let Some((lock, _)) = locked_native {
            install_pinned_apt(&mut plan, host, m, lock, ctx.platform)?;
        } else {
            plan.run(
                Command::new("apt-get")
                    .args(["install", "-y"])
                    .arg(m.ros_base_package())
                    .args([
                        "python3-rosdep",
                        "python3-vcstool",
                        "python3-colcon-common-extensions",
                    ])
                    .sudo(),
            );
        }

        // 3. Sources, in overlay order; `--locked` pins every git source to
        // its recorded revision after `vcs import` fetches the branch.
        let order = m.build_order()?;
        let scm = Vcstool;
        for name in &order {
            let ws = &m.workspaces[name];
            let ws_path = ctx.root.join(&ws.path);
            plan.run(
                Command::new("mkdir")
                    .arg("-p")
                    .arg(ws_path.join("src").to_string_lossy()),
            );
            if let Some(sources) = &ws.sources {
                plan.run(scm.import(&ws_path, &ctx.root.join(sources)));
            }
            if let Some((lock, _)) = locked_native {
                pin_git_sources(&mut plan, &ws_path, lock, name);
            }
        }

        // 4. rosdep, per workspace, in overlay order. Under `--locked` the
        // lock already lists the resolved apt packages, so `rosdep install`
        // (which resolves from package.xml again and could disagree with the
        // lock) is skipped; `init`/`update` still run so the tooling keeps
        // working afterwards.
        if m.dependencies.rosdep.enabled {
            if mirror.mirrors_rosdistro() {
                // `rosdep init` would write GitHub URLs; write the same file
                // with the mirror's instead (and overwrite a GitHub one, which
                // is what fails when the mirror is needed).
                if host.rosdep.initialized {
                    plan.note(msg!(
                        "native.apply.rosdep_list_rewrite",
                        file = ROSDEP_DEFAULT_LIST,
                        name = mirror.name
                    ));
                }
                plan.write_file_sudo(ROSDEP_DEFAULT_LIST, mirror.rosdep_sources_list());
            } else if !host.rosdep.initialized {
                plan.run(Command::new("rosdep").arg("init").sudo());
            }
            let mut update = Command::new("rosdep")
                .arg("update")
                .arg("--rosdistro")
                .arg(distro.as_str());
            if mirror.mirrors_rosdistro() {
                update = update.env("ROSDISTRO_INDEX_URL", mirror.rosdistro_index_url());
            }
            plan.run(update);
            if locked_native.is_some() {
                plan.note(msg!("native.apply.locked_rosdep_skipped"));
            } else {
                for name in &order {
                    let ws = &m.workspaces[name];
                    let ws_path = ctx.root.join(&ws.path);
                    let mut cmd = Command::new("rosdep")
                        .args(["install", "--from-paths", "src", "--ignore-src", "-y"])
                        .arg("--rosdistro")
                        .arg(distro.as_str())
                        .cwd(&ws_path);
                    if !m.dependencies.rosdep.skip_keys.is_empty() {
                        cmd = cmd
                            .arg("--skip-keys")
                            .arg(m.dependencies.rosdep.skip_keys.join(" "));
                    }
                    plan.run(cmd);
                }
            }
        }

        // 5. Extra apt packages after platform overrides — already installed
        // as part of the pinned set in step 2 under `--locked`.
        if locked_native.is_none() {
            let apt = m.resolve_apt(ctx.platform);
            for replacement in &apt.replacements {
                plan.note(replacement.notice(ctx.platform));
            }
            if !apt.packages.is_empty() {
                plan.run(
                    Command::new("apt-get")
                        .args(["install", "-y"])
                        .args(apt.packages)
                        .sudo(),
                );
            }
        }

        // 6. pip is recorded only (decision #6).
        if !m.dependencies.pip.is_empty() {
            plan.note(msg!(
                "native.apply.pip_recorded_only",
                packages = m.dependencies.pip.join(", ")
            ));
        }

        Ok(plan)
    }

    fn plan_uninstall(&self, ctx: &Context, keep_source: bool) -> Result<Plan> {
        let m = ctx.manifest;
        let host = ctx.host;
        let distro = m.project.ros_distro;
        let mut plan = Plan::new(msg!(
            "native.uninstall.title",
            distro = distro,
            name = m.project.name
        ));

        // Name every package explicitly so the plan says exactly what goes;
        // apt's pattern matching is not something to review at a glance.
        let prefix = format!("ros-{distro}-");
        let packages = host
            .packages
            .installed_with_prefix(&prefix)
            .ok_or_else(|| Error::Host(msg!("native.uninstall.dpkg_unavailable")))?;
        if packages.is_empty() {
            plan.note(msg!("native.uninstall.nothing_installed", prefix = prefix));
        } else {
            plan.note(msg!(
                "native.uninstall.removing",
                count = packages.len(),
                prefix = prefix
            ));
            plan.run(
                Command::new("apt-get")
                    .args(["remove", "-y"])
                    .args(packages)
                    .sudo(),
            );
            // No autoremove: it would also purge unrelated packages the user never saw in the plan.
            plan.note(msg!("native.uninstall.autoremove_hint"));
        }
        plan.note(msg!("native.uninstall.tooling_kept"));
        if host.has_ros_distro(distro.as_str()) {
            plan.note(msg!(
                "native.uninstall.files_may_remain",
                dir = host
                    .ros_setup_file(distro.as_str())
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ));
        }

        // The apt source is shared by every ROS 2 distro on the machine.
        let others: Vec<&String> = host
            .ros_distros
            .iter()
            .filter(|d| d.as_str() != distro.as_str())
            .collect();
        match (&host.apt_sources.ros2, keep_source) {
            (None, keep) => {
                plan.note(msg!("native.uninstall.no_source"));
                // Leftovers of an interrupted `apply --locked` (only rospm
                // writes these two paths): without this they outlive every
                // later uninstall, since the branch below needs `ros2.list`.
                if !keep {
                    let mut rm = Command::new("rm").arg("-f").sudo();
                    let mut any = false;
                    if host.apt_sources.snapshot_keyring {
                        rm = rm.arg(crate::snapshot::ROS_SNAPSHOT_KEYRING);
                        any = true;
                    }
                    let ubuntu_list = host.apt_sources.ubuntu_snapshot.is_some();
                    if ubuntu_list {
                        rm = rm.arg(probe::UBUNTU_SNAPSHOT_LIST);
                        any = true;
                    }
                    if any {
                        plan.run(rm);
                    }
                    if ubuntu_list {
                        plan.run(Command::new("apt-get").arg("update").sudo());
                    }
                }
            }
            (Some(_), true) => {
                plan.note(msg!("native.uninstall.keep_source"));
            }
            (Some(file), false) if !others.is_empty() => {
                plan.note(msg!(
                    "native.uninstall.source_kept_other_distros",
                    file = file.display(),
                    others = others
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            (Some(file), false) if file.as_path() == Path::new(ROS_APT_LIST) => {
                let mut rm = Command::new("rm").args(["-f", ROS_APT_LIST]).sudo();
                if host.apt_sources.keyring {
                    rm = rm.arg(ROS_APT_KEYRING);
                }
                // `apply --locked` leftovers: the snapshot keyring and rospm's
                // own Ubuntu snapshot file (never Ubuntu's own sources).
                if host.apt_sources.snapshot_keyring {
                    rm = rm.arg(crate::snapshot::ROS_SNAPSHOT_KEYRING);
                }
                if host.apt_sources.ubuntu_snapshot.is_some() {
                    rm = rm.arg(probe::UBUNTU_SNAPSHOT_LIST);
                }
                plan.run(rm);
                plan.run(Command::new("apt-get").arg("update").sudo());
            }
            (Some(file), false) => {
                plan.note(msg!(
                    "native.uninstall.source_not_ours",
                    file = file.display()
                ));
            }
        }
        Ok(plan)
    }

    fn plan_build(&self, ctx: &Context, only: Option<&str>) -> Result<Plan> {
        let m = ctx.manifest;
        let order = m.build_order()?;
        let targets: Vec<String> = match only {
            Some(name) => {
                if !m.workspaces.contains_key(name) {
                    return Err(Error::UnknownWorkspace(name.to_string()));
                }
                vec![name.to_string()]
            }
            None => order,
        };

        let mut plan = Plan::new(msg!(
            "native.build.title",
            name = m.project.name,
            order = targets.join(" -> ")
        ));
        let conda = match &m.build.conda {
            Some(conda) => {
                let step = crate::conda::CondaStep::new(
                    conda,
                    ctx.root,
                    &ctx.host.conda,
                    ctx.platform,
                    false,
                    None,
                )?;
                plan.steps
                    .push(crate::plan::Step::Conda(Box::new(step.clone())));
                Some(step)
            }
            None => None,
        };
        for name in &targets {
            let ws = &m.workspaces[name];
            let ws_path = ctx.root.join(&ws.path);
            let underlays = m.transitive_underlays(name)?;

            let mut cmd = Command::new("colcon").arg("build");
            if !ws.colcon.mixins.is_empty() {
                cmd = cmd.arg("--mixin").args(ws.colcon.mixins.iter().cloned());
            }
            cmd = cmd.args(ws.colcon.build_args.iter().cloned()).cwd(&ws_path);
            if let Some(defaults) = &ws.colcon.defaults {
                cmd = cmd.env(
                    "COLCON_DEFAULTS_FILE",
                    ctx.root.join(defaults).to_string_lossy(),
                );
            }
            for line in source_lines(ctx, &underlays, Shell::Bash) {
                cmd = cmd.preamble(line);
            }
            if let Some(conda) = &conda {
                // Activate Conda before sourcing ROS/underlays. Do not let a
                // caller's shell startup files select another environment.
                let cwd = cmd.cwd.take().expect("workspace directory is set");
                let script = format!("set -e\n{}", cmd.render());
                cmd = conda.command();
                cmd = cmd
                    .args(["bash", "--noprofile", "--norc", "-c", &script])
                    .cwd(cwd);
            }
            plan.run(cmd);
        }
        Ok(plan)
    }

    fn env_script(&self, ctx: &Context, shell: Shell) -> Result<String> {
        let m = ctx.manifest;
        let order = m.build_order()?;
        let mut out = format!(
            "# generated by rospm for `{}` — source this file, do not edit\n",
            m.project.name
        );
        for line in source_lines(ctx, &order, shell) {
            out.push_str(&line);
            out.push('\n');
        }
        for (k, v) in &m.env {
            out.push_str(&format!("export {k}={}\n", shell_quote(v)));
        }
        Ok(out)
    }

    fn resolve_lock(&self, ctx: &Context, fetch: Option<&dyn Fetch>) -> Result<Resolution> {
        lock::resolve(ctx, fetch)
    }

    fn verify(&self, ctx: &Context, lock: &Lock) -> Result<EnvReport> {
        verify::verify(ctx, lock)
    }

    fn probe_env(&self, ctx: &Context) -> Result<EnvReport> {
        let m = ctx.manifest;
        let host = ctx.host;
        let distro = m.project.ros_distro;
        let mut r = EnvReport::default();

        // Platform and OS.
        let listed: Vec<String> = m
            .project
            .platforms
            .iter()
            .map(ToString::to_string)
            .collect();
        match host.platform {
            Some(p) if m.project.platforms.contains(&p) => r.push(
                "platform",
                msg!("probe.subject.host"),
                listed.join(" | "),
                p.to_string(),
                Status::Ok,
            ),
            Some(p) => r.push(
                "platform",
                msg!("probe.subject.host"),
                listed.join(" | "),
                p.to_string(),
                Status::Mismatch,
            ),
            None => r.push(
                "platform",
                msg!("probe.subject.host"),
                listed.join(" | "),
                msg!("probe.platform.unsupported"),
                Status::Mismatch,
            ),
        }
        let want_ubuntu = m.ubuntu();
        match &host.os {
            Some(os) if os.is_ubuntu() => {
                let have = os.version_id.clone().unwrap_or_else(|| "?".into());
                let status = if have == want_ubuntu {
                    Status::Ok
                } else {
                    Status::Mismatch
                };
                r.push("os", Message::text("ubuntu"), &want_ubuntu, have, status);
            }
            Some(os) => r.push(
                "os",
                Message::text("ubuntu"),
                &want_ubuntu,
                os.pretty_name
                    .clone()
                    .map(Message::text)
                    .unwrap_or_else(|| msg!("probe.os.not_ubuntu")),
                Status::Mismatch,
            ),
            None => r.push(
                "os",
                Message::text("ubuntu"),
                &want_ubuntu,
                msg!("probe.os.unreadable"),
                Status::Unknown,
            ),
        }

        // Prerequisites the official install guide sets up before ROS itself.
        // System locale: what `apply` fixes. Without /etc/default/locale the
        // process value stands in for it (bare container).
        let system_value = host.locale.system.as_deref().unwrap_or("POSIX");
        let process_value = host.locale.process.as_deref().unwrap_or("POSIX");
        let (s, actual) = match host.locale.system_utf8() {
            Some(true) => (
                Status::Ok,
                msg!(
                    "probe.locale.system",
                    value = system_value,
                    file = DEFAULT_LOCALE_FILE
                ),
            ),
            Some(false) => (
                Status::Missing,
                msg!(
                    "probe.locale.system_not_utf8",
                    value = system_value,
                    file = DEFAULT_LOCALE_FILE
                ),
            ),
            None if host.locale.process_utf8() => (
                Status::Ok,
                msg!(
                    "probe.locale.no_file",
                    file = DEFAULT_LOCALE_FILE,
                    value = process_value
                ),
            ),
            None => (
                Status::Missing,
                msg!(
                    "probe.locale.no_file_not_utf8",
                    file = DEFAULT_LOCALE_FILE,
                    value = process_value
                ),
            ),
        };
        r.push(
            "os",
            msg!("probe.subject.locale_system"),
            Message::text("UTF-8"),
            actual,
            s,
        );
        // Current shell: informational, `apply` leaves it alone (it is the
        // caller's choice, e.g. `LANG=C` in CI), but ROS tools started from
        // this shell would still see it.
        let (s, actual) = if host.locale.process_utf8() {
            (Status::Ok, Message::text(process_value))
        } else {
            (
                Status::Info,
                msg!("probe.locale.shell_not_utf8", value = process_value),
            )
        };
        r.push(
            "os",
            msg!("probe.subject.locale_shell"),
            Message::text("UTF-8"),
            actual,
            s,
        );
        let (s, actual) = match host.apt_sources.universe {
            Some(true) => (Status::Ok, msg!("probe.universe.enabled")),
            Some(false) => (Status::Missing, msg!("probe.universe.not_enabled")),
            None => (Status::Unknown, msg!("probe.universe.unreadable")),
        };
        r.push(
            "apt",
            msg!("probe.subject.universe"),
            msg!("probe.universe.enabled"),
            actual,
            s,
        );
        let (s, actual) = match &host.apt_sources.ros2 {
            Some(file) => (
                Status::Ok,
                msg!(
                    "probe.ros_source.file",
                    file = file.display(),
                    source = host.apt_sources.ros2_mirror.unwrap_or("unknown")
                ),
            ),
            None => (
                Status::Missing,
                msg!("probe.ros_source.not_configured", file = ROS_APT_LIST),
            ),
        };
        r.push(
            "apt",
            msg!("probe.subject.ros_source"),
            msg!("probe.ros_source.expected", needle = ROS_APT_NEEDLE),
            actual,
            s,
        );

        // Tools apply/build shell out to.
        for tool in NATIVE_CORE_TOOLS.iter().chain(["apt-get"].iter()) {
            match host.tool(tool) {
                Some(p) => r.push(
                    "tool",
                    Message::text(*tool),
                    msg!("probe.present"),
                    p.display().to_string(),
                    Status::Ok,
                ),
                None => r.push(
                    "tool",
                    Message::text(*tool),
                    msg!("probe.present"),
                    msg!("probe.tool.not_in_path"),
                    Status::Missing,
                ),
            }
        }

        // `colcon mixin` is only needed (and only probed) when a workspace names mixins.
        let uses_mixins = m.workspaces.values().any(|ws| !ws.colcon.mixins.is_empty());
        if uses_mixins {
            match host.colcon_mixin_available() {
                Some(true) => r.push(
                    "tool",
                    Message::text("colcon mixin"),
                    msg!("probe.present"),
                    msg!("probe.present"),
                    Status::Ok,
                ),
                Some(false) => r.push(
                    "tool",
                    Message::text("colcon mixin"),
                    msg!("probe.present"),
                    msg!("probe.tool.mixin_unavailable"),
                    Status::Missing,
                ),
                None => r.push(
                    "tool",
                    Message::text("colcon mixin"),
                    msg!("probe.present"),
                    msg!("probe.tool.mixin_unknown"),
                    Status::Unknown,
                ),
            }
        }

        // ROS base install.
        let setup = host.ros_setup_file(distro.as_str());
        if host.has_ros_distro(distro.as_str()) {
            r.push(
                "ros",
                format!("ROS {distro}"),
                setup.display().to_string(),
                msg!("probe.present"),
                Status::Ok,
            );
        } else {
            let others = if host.ros_distros.is_empty() {
                msg!("probe.ros.none_installed")
            } else {
                msg!(
                    "probe.ros.only_others",
                    distros = host.ros_distros.join(", ")
                )
            };
            r.push(
                "ros",
                format!("ROS {distro}"),
                setup.display().to_string(),
                others,
                Status::Missing,
            );
        }

        // Installed packages: base metapackage + extra apt after overrides.
        let mut wanted = vec![m.ros_base_package()];
        wanted.extend(m.effective_apt(ctx.platform));
        match host.packages.status(&wanted) {
            Some(status) => {
                for (i, name) in wanted.iter().enumerate() {
                    let area = if i == 0 { "ros" } else { "apt" };
                    let s = &status[name];
                    if s.installed {
                        r.push(
                            area,
                            name,
                            msg!("probe.installed"),
                            s.version.clone().unwrap_or_default(),
                            Status::Ok,
                        );
                    } else {
                        r.push(
                            area,
                            name,
                            msg!("probe.installed"),
                            msg!("probe.not_installed"),
                            Status::Missing,
                        );
                    }
                }
            }
            None => {
                for (i, name) in wanted.iter().enumerate() {
                    let area = if i == 0 { "ros" } else { "apt" };
                    r.push(
                        area,
                        name,
                        msg!("probe.installed"),
                        msg!("probe.dpkg_unavailable"),
                        Status::Unknown,
                    );
                }
            }
        }

        // rosdep state.
        if m.dependencies.rosdep.enabled {
            let (s, actual) = if host.rosdep.initialized {
                (Status::Ok, msg!("probe.present"))
            } else {
                (Status::Missing, msg!("probe.rosdep.not_initialized"))
            };
            r.push(
                "rosdep",
                Message::text("rosdep init"),
                Message::text(ROSDEP_DEFAULT_LIST),
                actual,
                s,
            );
            let (s, actual) = if host.rosdep.updated {
                (Status::Ok, msg!("probe.present"))
            } else {
                (Status::Missing, msg!("probe.rosdep.not_updated"))
            };
            r.push(
                "rosdep",
                Message::text("rosdep update"),
                host.rosdep.cache_dir.display().to_string(),
                actual,
                s,
            );
        }

        // Sources and workspaces, in overlay order.
        for name in m.build_order()? {
            let ws = &m.workspaces[&name];
            let ws_path = ctx.root.join(&ws.path);
            let src = ws_path.join("src");

            if let Some(sources) = &ws.sources {
                let file = ctx.root.join(sources);
                if !file.is_file() {
                    r.push(
                        "source",
                        format!("{name}: {}", sources.display()),
                        msg!("probe.source.repos_file"),
                        msg!("probe.file_not_found"),
                        Status::Missing,
                    );
                } else {
                    match ReposFile::load(&file) {
                        Err(e) => r.push(
                            "source",
                            format!("{name}: {}", sources.display()),
                            msg!("probe.source.repos_file"),
                            msg!("probe.source.unparseable", error = e),
                            Status::Mismatch,
                        ),
                        Ok(repos) => {
                            for (rel, repo) in &repos.repositories {
                                let dir = src.join(rel);
                                let subject = format!("{name}: src/{rel}");
                                let expected = match &repo.version {
                                    Some(v) => format!("{} @ {v}", repo.url),
                                    None => repo.url.clone(),
                                };
                                if !dir.is_dir() {
                                    r.push(
                                        "source",
                                        subject,
                                        expected,
                                        msg!("probe.source.not_imported"),
                                        Status::Missing,
                                    );
                                } else if repo.kind == "git" {
                                    if !probe::is_git_checkout(&dir) {
                                        r.push(
                                            "source",
                                            subject,
                                            expected,
                                            msg!("probe.source.not_a_checkout"),
                                            Status::Mismatch,
                                        );
                                    } else {
                                        match host.git_remote_url(&dir) {
                                            Some(url) if same_git_url(&url, &repo.url) => {
                                                let head = host.git_head(&dir).unwrap_or_default();
                                                let head =
                                                    head.get(..12).unwrap_or(&head).to_string();
                                                r.push(
                                                    "source",
                                                    subject,
                                                    expected,
                                                    msg!("probe.source.imported_at", head = head),
                                                    Status::Ok,
                                                );
                                            }
                                            Some(url) => r.push(
                                                "source",
                                                subject,
                                                expected,
                                                msg!("probe.source.origin_differs", url = url),
                                                Status::Mismatch,
                                            ),
                                            None if host.has_tool("git") => r.push(
                                                "source",
                                                subject,
                                                expected,
                                                msg!("probe.source.no_origin"),
                                                Status::Mismatch,
                                            ),
                                            None => r.push(
                                                "source",
                                                subject,
                                                expected,
                                                msg!("probe.source.imported_unverified"),
                                                Status::Ok,
                                            ),
                                        }
                                    }
                                } else {
                                    r.push(
                                        "source",
                                        subject,
                                        expected,
                                        msg!("probe.source.imported_other_vcs", kind = repo.kind),
                                        Status::Ok,
                                    );
                                }
                            }
                        }
                    }
                }
            }

            // Packages present in src/ and whether the workspace was ever built.
            let pkgs = WorkspacePackages::scan(&src);
            let n = pkgs.packages.len();
            let expected_pkgs = if ws.sources.is_some() {
                msg!("probe.workspace.packages_from_repos")
            } else {
                msg!("probe.workspace.local_packages")
            };
            if n == 0 {
                let status = if ws.sources.is_some() {
                    Status::Info
                } else {
                    Status::Missing
                };
                r.push(
                    "workspace",
                    format!("{name}: src/"),
                    expected_pkgs,
                    msg!("probe.workspace.no_packages"),
                    status,
                );
            } else {
                let found = if n == 1 {
                    msg!("probe.workspace.one_package")
                } else {
                    msg!("probe.workspace.n_packages", count = n)
                };
                r.push(
                    "workspace",
                    format!("{name}: src/"),
                    expected_pkgs,
                    found,
                    Status::Ok,
                );
            }
            for (file, err) in &pkgs.errors {
                r.push(
                    "workspace",
                    format!("{name}: {}", file.display()),
                    msg!("probe.workspace.valid_package_xml"),
                    err.clone(),
                    Status::Mismatch,
                );
            }
            if let Some(defaults) = &ws.colcon.defaults {
                let f = ctx.root.join(defaults);
                let (s, actual) = if f.is_file() {
                    (Status::Ok, msg!("probe.present"))
                } else {
                    (Status::Missing, msg!("probe.file_not_found"))
                };
                r.push(
                    "workspace",
                    msg!("probe.subject.colcon_defaults", name = name),
                    defaults.display().to_string(),
                    actual,
                    s,
                );
            }
            let built = ws_path.join("install").join("setup.bash").is_file();
            r.push(
                "workspace",
                format!("{name}: {}", ws.path.display()),
                msg!("probe.workspace.expected_built"),
                if built {
                    msg!("probe.workspace.built")
                } else {
                    msg!("probe.workspace.not_built")
                },
                Status::Info,
            );
        }

        Ok(r)
    }

    fn probe_deps(&self, ctx: &Context) -> Result<deps::DepsReport> {
        deps::probe(ctx)
    }

    fn plan_deps_install(&self, ctx: &Context) -> Result<Plan> {
        deps::plan_install(ctx)
    }
}
