//! `native` provider: ROS installed from apt on the host.
//!
//! This is the P1 provider. `apply` starts from a bare Ubuntu: it configures
//! the ROS apt source (keyring + `sources.list.d`, the same mechanism P2 reuses
//! to switch to snapshot repositories) before installing the base metapackage.
//! `--locked` snapshot switching arrives in P2 and is reported as a
//! degradation until then.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::manifest::{Manifest, ProviderKind};
use crate::package_xml::WorkspacePackages;
use crate::plan::{shell_quote, Command, Plan};
use crate::platform::ubuntu_codename;
use crate::probe::{
    self, EnvReport, Status, NATIVE_CORE_TOOLS, ROSDEP_DEFAULT_LIST, ROS_APT_KEYRING, ROS_APT_LIST,
    ROS_APT_NEEDLE, ROS_KEY_FINGERPRINT,
};
use crate::provider::{Context, Provider, Shell};
use crate::repos::{same_git_url, ReposFile};
use crate::scm::{Scm, Vcstool};

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

impl Provider for Native {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Native
    }

    fn plan_apply(&self, ctx: &Context) -> Result<Plan> {
        let m = ctx.manifest;
        let host = ctx.host;
        let distro = m.project.ros_distro;
        let mut plan = Plan::new(format!(
            "apply `{}` (native, {}, {})",
            m.project.name, distro, ctx.platform
        ));

        if ctx.locked {
            plan.note(
                "--locked: snapshot repositories are not implemented yet (roadmap P2); \
                 DEGRADED to live repositories",
            );
        }
        if ctx.offline {
            return Err(Error::NotImplemented {
                feature: "offline apply",
                phase: "P6",
            });
        }

        // 0. Refuse to install onto the wrong Ubuntu: ROS binaries exist for
        // exactly one release per distro, and rospm never changes the OS.
        let ubuntu = m.ubuntu();
        let codename = ubuntu_codename(&ubuntu).ok_or_else(|| {
            Error::Host(format!(
                "no ROS apt repository for Ubuntu {ubuntu} (environment.native.ubuntu); \
                 supported: 22.04 (humble), 24.04 (jazzy, rolling)"
            ))
        })?;
        match host.ubuntu_matches(&ubuntu) {
            Some(true) => {}
            Some(false) => {
                let have = host
                    .os
                    .as_ref()
                    .and_then(|os| os.pretty_name.clone())
                    .unwrap_or_else(|| "unknown OS".into());
                return Err(Error::Host(format!(
                    "manifest needs Ubuntu {ubuntu} for ROS {distro}, this host is {have}; \
                     rospm does not install or upgrade Ubuntu itself"
                )));
            }
            None => {
                plan.note(format!(
                    "/etc/os-release is unreadable; assuming Ubuntu {ubuntu} ({codename})"
                ));
            }
        }

        // 1. Bootstrap the ROS apt source (official install guide, minus the
        // steps this host already satisfies). URLs come from the chosen
        // source set (decision #17); the key is verified either way.
        let mirror = ctx.mirror;
        if !mirror.is_official() {
            plan.note(format!(
                "sources: {} mirror ({}); the apt signing key is still verified against the built-in fingerprint",
                mirror.name, mirror.label
            ));
        }
        let need_locale = !host.locale_utf8;
        let need_universe = host.apt_sources.universe != Some(true);
        // Rewrite rospm's own source file when a different source set was
        // chosen; a file written by something else (ros2-apt-source) is left alone.
        let rewrite_source = host
            .apt_sources
            .ros2
            .as_deref()
            .is_some_and(|f| f == Path::new(ROS_APT_LIST))
            && host.apt_sources.ros2_mirror != Some(mirror.name);
        let need_source = host.apt_sources.ros2.is_none() || rewrite_source;
        plan.run(Command::new("apt-get").arg("update").sudo());
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
            plan.run(
                Command::new("update-locale")
                    .args(["LC_ALL=en_US.UTF-8", "LANG=en_US.UTF-8"])
                    .sudo(),
            );
            plan.note(
                "locale switched to en_US.UTF-8 system-wide; run `export LANG=en_US.UTF-8` \
                 in the current shell or log in again",
            );
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
                plan.note(format!(
                    "{ROS_APT_LIST} currently points at the {} sources; rewriting it for {}",
                    host.apt_sources.ros2_mirror.unwrap_or("unknown"),
                    mirror.name
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
            plan.note(format!(
                "ROS apt source already configured in {} ({})",
                file.display(),
                host.apt_sources.ros2_mirror.unwrap_or("unknown source")
            ));
            if host.apt_sources.ros2_mirror != Some(mirror.name) {
                plan.note(format!(
                    "--mirror {} not applied to the apt source: {} was not written by rospm; \
                     edit it by hand if it should use another server",
                    mirror.name,
                    file.display()
                ));
            }
        }

        // 2. ROS base + tooling.
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

        // 3. Sources, in overlay order.
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
        }

        // 4. rosdep, per workspace, in overlay order.
        if m.dependencies.rosdep.enabled {
            if mirror.mirrors_rosdistro() {
                // `rosdep init` would write GitHub URLs; write the same file
                // with the mirror's instead (and overwrite a GitHub one, which
                // is what fails when the mirror is needed).
                if host.rosdep.initialized {
                    plan.note(format!(
                        "{ROSDEP_DEFAULT_LIST} exists; rewriting it for the {} mirror",
                        mirror.name
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

        // 5. Extra apt packages after platform overrides.
        let apt = m.effective_apt(ctx.platform);
        if !apt.is_empty() {
            plan.run(
                Command::new("apt-get")
                    .args(["install", "-y"])
                    .args(apt)
                    .sudo(),
            );
        }

        // 6. pip is recorded only (decision #6).
        if !m.dependencies.pip.is_empty() {
            plan.note(format!(
                "pip dependencies are recorded but not installed by rospm (decision #6): {}",
                m.dependencies.pip.join(", ")
            ));
        }

        Ok(plan)
    }

    fn plan_uninstall(&self, ctx: &Context, keep_source: bool) -> Result<Plan> {
        let m = ctx.manifest;
        let host = ctx.host;
        let distro = m.project.ros_distro;
        let mut plan = Plan::new(format!(
            "uninstall ROS {distro} (native) for `{}`",
            m.project.name
        ));

        // Name every package explicitly so the plan says exactly what goes;
        // apt's pattern matching is not something to review at a glance.
        let prefix = format!("ros-{distro}-");
        let packages = host
            .packages
            .installed_with_prefix(&prefix)
            .ok_or_else(|| {
                Error::Host(
                    "dpkg-query is unavailable, cannot list the installed ROS packages".into(),
                )
            })?;
        if packages.is_empty() {
            plan.note(format!(
                "no {prefix}* packages are installed; nothing to remove"
            ));
        } else {
            plan.note(format!(
                "removing {} {prefix}* packages (listed below)",
                packages.len()
            ));
            plan.run(
                Command::new("apt-get")
                    .args(["remove", "-y"])
                    .args(packages)
                    .sudo(),
            );
            plan.run(Command::new("apt-get").args(["autoremove", "-y"]).sudo());
        }
        plan.note(
            "python3-rosdep, python3-vcstool and python3-colcon-common-extensions are left \
             installed (shared tooling); remove them by hand if unwanted",
        );
        if host.has_ros_distro(distro.as_str()) {
            plan.note(format!(
                "files apt does not own may remain under {}; remove the directory by hand \
                 after checking it",
                host.ros_setup_file(distro.as_str())
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
            (None, _) => {
                plan.note("no ROS apt source configured; nothing to remove");
            }
            (Some(_), true) => {
                plan.note("--keep-source: ROS apt source left in place");
            }
            (Some(file), false) if !others.is_empty() => {
                plan.note(format!(
                    "ROS apt source {} kept: other ROS installs remain under /opt/ros ({})",
                    file.display(),
                    others
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
                plan.run(rm);
                plan.run(Command::new("apt-get").arg("update").sudo());
            }
            (Some(file), false) => {
                plan.note(format!(
                    "ROS apt source {} was not written by rospm (e.g. the ros2-apt-source package); \
                     left in place",
                    file.display()
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

        let mut plan = Plan::new(format!(
            "build `{}` ({})",
            m.project.name,
            targets.join(" -> ")
        ));
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
                "host",
                listed.join(" | "),
                p.to_string(),
                Status::Ok,
            ),
            Some(p) => r.push(
                "platform",
                "host",
                listed.join(" | "),
                p.to_string(),
                Status::Mismatch,
            ),
            None => r.push(
                "platform",
                "host",
                listed.join(" | "),
                "unsupported",
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
                r.push("os", "ubuntu", &want_ubuntu, have, status);
            }
            Some(os) => r.push(
                "os",
                "ubuntu",
                &want_ubuntu,
                os.pretty_name
                    .clone()
                    .unwrap_or_else(|| "not Ubuntu".into()),
                Status::Mismatch,
            ),
            None => r.push(
                "os",
                "ubuntu",
                &want_ubuntu,
                "unreadable /etc/os-release",
                Status::Unknown,
            ),
        }

        // Prerequisites the official install guide sets up before ROS itself.
        let (s, actual) = if host.locale_utf8 {
            (Status::Ok, "UTF-8".to_string())
        } else {
            (
                Status::Missing,
                "not UTF-8 (`rospm apply` sets en_US.UTF-8)".to_string(),
            )
        };
        r.push("os", "locale", "UTF-8", actual, s);
        let (s, actual) = match host.apt_sources.universe {
            Some(true) => (Status::Ok, "enabled".to_string()),
            Some(false) => (
                Status::Missing,
                "not enabled (`rospm apply` runs add-apt-repository universe)".to_string(),
            ),
            None => (Status::Unknown, "no apt source files readable".to_string()),
        };
        r.push("apt", "universe component", "enabled", actual, s);
        let (s, actual) = match &host.apt_sources.ros2 {
            Some(file) => (
                Status::Ok,
                format!(
                    "{} ({})",
                    file.display(),
                    host.apt_sources.ros2_mirror.unwrap_or("unknown source")
                ),
            ),
            None => (
                Status::Missing,
                format!("not configured (`rospm apply` writes {ROS_APT_LIST})"),
            ),
        };
        r.push(
            "apt",
            "ROS apt source",
            format!("{ROS_APT_NEEDLE} or a known mirror"),
            actual,
            s,
        );

        // Tools apply/build shell out to.
        for tool in NATIVE_CORE_TOOLS.iter().chain(["apt-get"].iter()) {
            match host.tool(tool) {
                Some(p) => r.push(
                    "tool",
                    *tool,
                    "present",
                    p.display().to_string(),
                    Status::Ok,
                ),
                None => r.push(
                    "tool",
                    *tool,
                    "present",
                    "not found in PATH",
                    Status::Missing,
                ),
            }
        }

        // `colcon mixin` is only needed (and only probed) when a workspace names mixins.
        let uses_mixins = m.workspaces.values().any(|ws| !ws.colcon.mixins.is_empty());
        if uses_mixins {
            match host.colcon_mixin_available() {
                Some(true) => r.push("tool", "colcon mixin", "present", "present", Status::Ok),
                Some(false) => r.push(
                    "tool",
                    "colcon mixin",
                    "present",
                    "not available (`sudo apt-get install python3-colcon-mixin`, then \
                     `colcon mixin add default <index url>` and `colcon mixin update`)",
                    Status::Missing,
                ),
                None => r.push(
                    "tool",
                    "colcon mixin",
                    "present",
                    "unknown (colcon not found)",
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
                "present",
                Status::Ok,
            );
        } else {
            let others = if host.ros_distros.is_empty() {
                "no ROS install under /opt/ros".to_string()
            } else {
                format!("only: {}", host.ros_distros.join(", "))
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
                            "installed",
                            s.version.clone().unwrap_or_default(),
                            Status::Ok,
                        );
                    } else {
                        r.push(area, name, "installed", "not installed", Status::Missing);
                    }
                }
            }
            None => {
                for (i, name) in wanted.iter().enumerate() {
                    let area = if i == 0 { "ros" } else { "apt" };
                    r.push(
                        area,
                        name,
                        "installed",
                        "dpkg-query unavailable",
                        Status::Unknown,
                    );
                }
            }
        }

        // rosdep state.
        if m.dependencies.rosdep.enabled {
            let (s, actual) = if host.rosdep.initialized {
                (Status::Ok, "present".to_string())
            } else {
                (
                    Status::Missing,
                    "absent (run `sudo rosdep init`)".to_string(),
                )
            };
            r.push("rosdep", "rosdep init", ROSDEP_DEFAULT_LIST, actual, s);
            let (s, actual) = if host.rosdep.updated {
                (Status::Ok, "present".to_string())
            } else {
                (Status::Missing, "absent (run `rosdep update`)".to_string())
            };
            r.push(
                "rosdep",
                "rosdep update",
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
                        "vcstool .repos file",
                        "file not found",
                        Status::Missing,
                    );
                } else {
                    match ReposFile::load(&file) {
                        Err(e) => r.push(
                            "source",
                            format!("{name}: {}", sources.display()),
                            "vcstool .repos file",
                            format!("unparseable: {e}"),
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
                                        "not imported",
                                        Status::Missing,
                                    );
                                } else if repo.kind == "git" {
                                    if !probe::is_git_checkout(&dir) {
                                        r.push(
                                            "source",
                                            subject,
                                            expected,
                                            "directory exists but is not a git checkout",
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
                                                    format!("imported @ {head}"),
                                                    Status::Ok,
                                                );
                                            }
                                            Some(url) => r.push(
                                                "source",
                                                subject,
                                                expected,
                                                format!("origin is {url}"),
                                                Status::Mismatch,
                                            ),
                                            None if host.has_tool("git") => r.push(
                                                "source",
                                                subject,
                                                expected,
                                                "git checkout without an `origin` remote",
                                                Status::Mismatch,
                                            ),
                                            None => r.push(
                                                "source",
                                                subject,
                                                expected,
                                                "imported (origin not verified: git missing)",
                                                Status::Ok,
                                            ),
                                        }
                                    }
                                } else {
                                    r.push(
                                        "source",
                                        subject,
                                        expected,
                                        format!("imported ({} checkout not verified)", repo.kind),
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
                "packages from .repos"
            } else {
                "local packages"
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
                    "no package.xml found",
                    status,
                );
            } else {
                r.push(
                    "workspace",
                    format!("{name}: src/"),
                    expected_pkgs,
                    format!("{n} packages"),
                    Status::Ok,
                );
            }
            for (file, err) in &pkgs.errors {
                r.push(
                    "workspace",
                    format!("{name}: {}", file.display()),
                    "valid package.xml",
                    err.clone(),
                    Status::Mismatch,
                );
            }
            if let Some(defaults) = &ws.colcon.defaults {
                let f = ctx.root.join(defaults);
                let (s, actual) = if f.is_file() {
                    (Status::Ok, "present")
                } else {
                    (Status::Missing, "file not found")
                };
                r.push(
                    "workspace",
                    format!("{name}: colcon defaults"),
                    defaults.display().to_string(),
                    actual,
                    s,
                );
            }
            let built = ws_path.join("install").join("setup.bash").is_file();
            r.push(
                "workspace",
                format!("{name}: {}", ws.path.display()),
                "built (install/setup.bash)",
                if built {
                    "built"
                } else {
                    "not built yet (run `rospm build`)"
                },
                Status::Info,
            );
        }

        Ok(r)
    }
}
