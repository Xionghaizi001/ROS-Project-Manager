//! `Provider` implementation for the native apt provider.

use super::apply::*;
use super::*;

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
            plan_prerequisites(&mut plan, host, need_locale, need_universe, need_source);
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
                    .args(TOOLING)
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
            if !plan_rosdep_mirror_list(&mut plan, host, &mirror) && !host.rosdep.initialized {
                plan.run(Command::new("rosdep").arg("init").sudo());
            }
            plan.run(rosdep_update(distro.as_str(), &mirror));
            if locked_native.is_some() {
                plan.note(msg!("native.apply.locked_rosdep_skipped"));
            } else {
                for name in &order {
                    let ws = &m.workspaces[name];
                    plan.run(rosdep_install(m, &ctx.root.join(&ws.path)));
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
                if !keep {
                    remove_snapshot_leftovers(&mut plan, host);
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
                // The foreign source uses neither of rospm's snapshot files.
                remove_snapshot_leftovers(&mut plan, host);
            }
        }
        Ok(plan)
    }

    fn plan_build(&self, ctx: &Context, only: Option<&str>, verify: bool) -> Result<Plan> {
        let m = ctx.manifest;
        let venv = crate::venv::selected(ctx)?;
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
        let conda = match m.build.conda.as_ref().filter(|_| venv.is_none()) {
            Some(conda) => {
                let step = crate::conda::CondaStep::new(
                    conda,
                    ctx.root,
                    &ctx.host.conda,
                    ctx.platform,
                    false,
                    None,
                )?;
                let build_dirs = targets
                    .iter()
                    .filter_map(|n| {
                        existing_base(colcon_bases(
                            ctx.root,
                            &m.workspaces[n],
                            "build-base",
                            "build",
                        ))
                    })
                    .collect();
                let ros = source_lines(ctx, &[], Shell::Bash, false).remove(0);
                let step = step.for_build(ros, build_dirs);
                plan.steps
                    .push(crate::plan::Step::Conda(Box::new(step.clone())));
                Some(step)
            }
            None => None,
        };
        plan.log = Some(ctx.root.join(BUILD_LOG));
        let deactivate = match (&conda, &ctx.host.conda.active) {
            (None, Some(active)) if venv.is_none() => {
                plan.note(msg!("native.build.conda_active", prefix = active.display()));
                Some((
                    msg!("native.build.conda_deactivated", prefix = active.display()).to_string(),
                    msg!("native.build.conda_stale", prefix = active.display()).to_string(),
                ))
            }
            _ => None,
        };
        let python = conda.as_ref().map_or_else(
            || {
                venv.as_ref()
                    .map_or("system".into(), |p| format!("venv {}", p.display()))
            },
            |c| format!("conda {}", c.label()),
        );
        for name in &targets {
            let ws = &m.workspaces[name];
            let ws_path = ctx.root.join(&ws.path);
            let underlays = m.transitive_underlays(name)?;

            let mut args = ws.colcon.build_args.clone();
            // `--log-base` is an option of colcon itself: `colcon build --log-base` is rejected.
            let log_base = take_option(&mut args, "--log-base");
            let mut cmd = Command::new("colcon").interruptible();
            if let Some(base) = &log_base {
                cmd = cmd.args(["--log-base", base]);
            }
            cmd = cmd.arg("build");
            if !ws.colcon.mixins.is_empty() {
                cmd = cmd.arg("--mixin").args(ws.colcon.mixins.iter().cloned());
            }
            cmd = cmd.args(args).cwd(&ws_path);
            if let Some(defaults) = &ws.colcon.defaults {
                cmd = cmd.env(
                    "COLCON_DEFAULTS_FILE",
                    ctx.root.join(defaults).to_string_lossy(),
                );
            }
            if let Some(path) = &venv {
                cmd = cmd.preamble(crate::venv::activation(path)?.trim_end());
            }
            for line in source_lines(ctx, &underlays, Shell::Bash, true) {
                cmd = cmd.preamble(line);
            }
            if let Some((note, stale)) = &deactivate {
                let base = existing_base(colcon_bases(ctx.root, ws, "build-base", "build"));
                cmd = cmd.preamble(deactivate_unusable_conda(note, stale, base.as_deref()));
            }
            if let Some(path) = &venv {
                cmd = cmd.preamble(crate::venv::check(path).trim_end());
                cmd.program = path.join("bin/python").to_string_lossy().into_owned();
                cmd.args.splice(
                    0..0,
                    [
                        "-s".into(),
                        "-c".into(),
                        "from colcon_core.command import main; raise SystemExit(main())".into(),
                    ],
                );
                let defines: Vec<String> = ["Python3", "Python", "PYTHON"]
                    .iter()
                    .map(|name| {
                        format!("-D{name}_EXECUTABLE={}", path.join("bin/python").display())
                    })
                    .collect();
                if let Some(i) = cmd.args.iter().rposition(|a| a == "--cmake-args") {
                    let end = cmd
                        .args
                        .iter()
                        .enumerate()
                        .skip(i + 1)
                        .find(|(_, arg)| arg.starts_with("--"))
                        .map_or(cmd.args.len(), |(j, _)| j);
                    cmd.args.splice(end..end, defines);
                } else {
                    let defaults = ws
                        .colcon
                        .defaults
                        .as_ref()
                        .map(|f| ctx.root.join(f))
                        .or_else(colcon_defaults_file);
                    cmd.args.push("--cmake-args".into());
                    cmd.args
                        .extend(defaults.map(|f| default_cmake_args(&f)).unwrap_or_default());
                    cmd.args.extend(defines);
                }
            }
            if let Some(conda) = &conda {
                // colcon keeps only the last `--cmake-args`, so join the
                // user's (command line, else defaults file) instead of adding one.
                // The last group is the one colcon keeps (command line after the manifest).
                match cmd.args.iter().rposition(|a| a == "--cmake-args") {
                    Some(i) => {
                        cmd.args
                            .splice(i + 1..i + 1, PYTHON_DEFINES.map(String::from));
                    }
                    None => {
                        let defaults = ws
                            .colcon
                            .defaults
                            .as_ref()
                            .map(|f| ctx.root.join(f))
                            .or_else(colcon_defaults_file);
                        cmd.args.push("--cmake-args".into());
                        cmd.args
                            .extend(defaults.map(|f| default_cmake_args(&f)).unwrap_or_default());
                        cmd.args.extend(PYTHON_DEFINES.map(String::from));
                    }
                }
                // Activate Conda before sourcing ROS/underlays. Do not let a
                // caller's shell startup files select another environment.
                let cwd = cmd.cwd.take().expect("workspace directory is set");
                let mut script = format!("set -e\n{}", cmd.render());
                // `$CONDA_PREFIX` is set by `conda run`; let bash expand it.
                for define in PYTHON_DEFINES {
                    script = script.replace(&shell_quote(define), &format!("\"{define}\""));
                }
                cmd = conda.command();
                cmd = cmd
                    .args(["bash", "--noprofile", "--norc", "-c", &script])
                    .cwd(cwd);
            }
            // colcon's order: `--log-base`, `$COLCON_LOG_PATH`, `log`, from the workspace.
            let log_dir = log_base
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("COLCON_LOG_PATH").map(PathBuf::from))
                .map_or_else(|| ws_path.join("log"), |b| ws_path.join(b));
            let packages = log_dir.join("latest_build/<package>/stdout_stderr.log");
            plan.run(cmd.log(format!(
                "workspace: {name}\npython: {python}\npackage logs: {}\n",
                packages.display()
            )));
        }
        if let Some(conda) = &conda {
            plan.note(msg!("conda.shebangs"));
            plan.run(
                conda.pin_shebangs(
                    targets.iter().map(|n| {
                        colcon_bases(ctx.root, &m.workspaces[n], "install-base", "install")
                    }),
                ),
            );
        }
        if verify {
            // Same environment `rospm env` gives the user, so a passing check
            // means the documented workflow works too.
            let env = self.env_script(ctx, Shell::Bash)?;
            for check in &m.build.verify {
                let script = match &deactivate {
                    Some((note, stale)) => format!(
                        "set -e\n{env}{}\n{check}",
                        deactivate_unusable_conda(note, stale, None)
                    ),
                    None => format!("set -e\n{env}{check}"),
                };
                let cmd = match &conda {
                    Some(conda) => conda.command().arg("bash"),
                    None => Command::new("bash"),
                };
                plan.run(
                    cmd.args(["--noprofile", "--norc", "-c", &script])
                        .cwd(ctx.root)
                        .log(format!(
                            "verify: {}\npython: {python}\n",
                            check.replace('\n', "; ")
                        )),
                );
            }
        }
        Ok(plan)
    }

    fn env_script(&self, ctx: &Context, shell: Shell) -> Result<String> {
        let m = ctx.manifest;
        let venv = crate::venv::selected(ctx)?;
        let order = m.build_order()?;
        let mut out = format!(
            "# generated by rospm for `{}` — source this file, do not edit\n",
            m.project.name
        );
        // Conda first, as in build: ROS and the overlays are sourced into it.
        if let Some(path) = &venv {
            out.push_str(&crate::venv::activation(path)?);
        } else if let Some(conda) = &m.build.conda {
            let step = crate::conda::CondaStep::new(
                conda,
                ctx.root,
                &ctx.host.conda,
                ctx.platform,
                false,
                None,
            )?;
            out.push_str(&step.activation(shell));
        }
        for line in source_lines(ctx, &order, shell, false) {
            out.push_str(&line);
            if venv.is_some() {
                out.push_str(" || { return 1 2>/dev/null || exit 1; }");
            }
            out.push('\n');
        }
        if let Some(path) = &venv {
            out.push_str(&crate::venv::check(path));
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
        self.probe_env_impl(ctx)
    }

    fn probe_deps(&self, ctx: &Context) -> Result<deps::DepsReport> {
        deps::probe(ctx)
    }

    fn plan_deps_install(&self, ctx: &Context) -> Result<Plan> {
        deps::plan_install(ctx)
    }
}
