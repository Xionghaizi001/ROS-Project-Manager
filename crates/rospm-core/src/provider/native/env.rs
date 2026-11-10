//! Environment probing for the native provider.

use super::*;

impl Native {
    pub(super) fn probe_env_impl(&self, ctx: &Context) -> Result<EnvReport> {
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

            // Match colcon discovery from the workspace root.
            let pkgs = WorkspacePackages::scan(&ws_path);
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
                    format!("{name}: {}", ws.path.display()),
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
                    format!("{name}: {}", ws.path.display()),
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
            let built = colcon_bases(ctx.root, ws, "install-base", "install")
                .iter()
                .any(|b| b.join("setup.bash").is_file());
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

        r.items.extend(crate::venv::probe(ctx).items);
        Ok(r)
    }
}
