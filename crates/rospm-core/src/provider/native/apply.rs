//! Planning helpers for native provider application.

use super::*;

pub(super) fn plan_prerequisites(
    plan: &mut Plan,
    host: &probe::Host,
    need_locale: bool,
    need_universe: bool,
    need_source: bool,
) {
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
}

/// With a mirror that carries rosdistro, write rosdep's default sources list
/// pointing at it and return true: `rosdep init` would write GitHub URLs (and
/// a GitHub one is what fails when the mirror is needed, so it is overwritten).
pub(crate) fn plan_rosdep_mirror_list(
    plan: &mut Plan,
    host: &probe::Host,
    mirror: &crate::mirror::Mirror,
) -> bool {
    if !mirror.mirrors_rosdistro() {
        return false;
    }
    if host.rosdep.initialized {
        plan.note(msg!(
            "native.apply.rosdep_list_rewrite",
            file = ROSDEP_DEFAULT_LIST,
            name = mirror.name
        ));
    }
    plan.write_file_sudo(ROSDEP_DEFAULT_LIST, mirror.rosdep_sources_list());
    true
}

/// `rosdep update` for `distro`, against the mirror's rosdistro index when
/// it has one.
pub(crate) fn rosdep_update(distro: &str, mirror: &crate::mirror::Mirror) -> Command {
    let update = Command::new("rosdep").args(["update", "--rosdistro", distro]);
    if mirror.mirrors_rosdistro() {
        update.env("ROSDISTRO_INDEX_URL", mirror.rosdistro_index_url())
    } else {
        update
    }
}

/// `rosdep install` for one workspace, honouring the manifest's skip keys.
pub(crate) fn rosdep_install(m: &Manifest, ws_path: &Path) -> Command {
    let mut cmd = Command::new("rosdep")
        .args(["install", "--from-paths", "src", "--ignore-src", "-y"])
        .arg("--rosdistro")
        .arg(m.project.ros_distro.as_str())
        .cwd(ws_path);
    if !m.dependencies.rosdep.skip_keys.is_empty() {
        cmd = cmd
            .arg("--skip-keys")
            .arg(m.dependencies.rosdep.skip_keys.join(" "));
    }
    cmd
}

/// Pin the ROS apt source to a snapshot (`apply --locked`, spec §3.2).
/// Mirrors the P1 rewrite rules: only rospm's own [`ROS_APT_LIST`] is
/// touched — a foreign source file (e.g. `ros2-apt-source`'s `ros2.sources`)
/// is left alone and the plan says so. `--mirror` has no effect on the apt
/// line here: snapshots are not mirrored (rosdistro/rosdep still use it,
/// unaffected by this function).
pub(super) fn plan_ros_snapshot_source(
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

    plan_prerequisites(plan, host, need_locale, need_universe, need_source);

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
pub(super) fn plan_ubuntu_snapshot_source(
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
pub(super) fn install_pinned_apt(
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
pub(super) fn pin_git_sources(plan: &mut Plan, ws_path: &Path, lock: &Lock, workspace: &str) {
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
