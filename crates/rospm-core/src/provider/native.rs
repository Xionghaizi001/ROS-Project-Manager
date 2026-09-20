//! `native` provider: ROS installed from apt on the host.
//!
//! This is the P1 provider. In P0 it already produces complete plans so
//! `apply --dry-run` / `build --dry-run` can be reviewed; `--locked` snapshot
//! switching arrives in P2 and is reported as a degradation until then.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::manifest::{Manifest, ProviderKind};
use crate::plan::{shell_quote, Command, Plan};
use crate::provider::{Context, Provider, Shell};
use crate::scm::{Scm, Vcstool};

pub struct Native;

const ROSDEP_DEFAULT_LIST: &str = "/etc/ros/rosdep/sources.list.d/20-default.list";

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

        // 1. ROS base + tooling.
        plan.run(Command::new("apt-get").arg("update").sudo());
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

        // 2. Sources, in overlay order.
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

        // 3. rosdep, per workspace, in overlay order.
        if m.dependencies.rosdep.enabled {
            if !Path::new(ROSDEP_DEFAULT_LIST).exists() {
                plan.run(Command::new("rosdep").arg("init").sudo());
            }
            plan.run(
                Command::new("rosdep")
                    .arg("update")
                    .arg("--rosdistro")
                    .arg(distro.as_str()),
            );
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

        // 4. Extra apt packages after platform overrides.
        let apt = m.effective_apt(ctx.platform);
        if !apt.is_empty() {
            plan.run(
                Command::new("apt-get")
                    .args(["install", "-y"])
                    .args(apt)
                    .sudo(),
            );
        }

        // 5. pip is recorded only (decision #6).
        if !m.dependencies.pip.is_empty() {
            plan.note(format!(
                "pip dependencies are recorded but not installed by rospm (decision #6): {}",
                m.dependencies.pip.join(", ")
            ));
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
}
