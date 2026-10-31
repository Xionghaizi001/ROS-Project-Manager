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

mod apply;
mod env;
mod provider_impl;

#[cfg(test)]
mod tests;

pub(crate) use apply::{plan_rosdep_mirror_list, rosdep_install, rosdep_update};

/// Tooling `apply` installs next to the base metapackage. `lock` records it
/// so `apply --locked` replays it; `repository check` looks it up.
pub const TOOLING: [&str; 3] = [
    "python3-rosdep",
    "python3-vcstool",
    "python3-colcon-common-extensions",
];

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::lock::{Lock, Resolution};
use crate::manifest::{Manifest, ProviderKind, Workspace};
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

/// colcon's `--build-base` / `--install-base` (`key` without dashes) for a
/// workspace, as candidates in lookup order: the configured base (the
/// manifest's `build_args`, else the colcon defaults file), then colcon's
/// `default`. Relative values are taken from the workspace directory, where
/// colcon runs. Callers use the first that exists.
// shortcut: a base set through a mixin is not seen; name it in build_args or the defaults file.
pub fn colcon_bases(root: &Path, ws: &Workspace, key: &str, default: &str) -> Vec<PathBuf> {
    let flag = format!("--{key}");
    let args = &ws.colcon.build_args;
    // argparse keeps the last occurrence.
    let configured = args
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, a)| match a.strip_prefix(&flag) {
            Some("") => args.get(i + 1).cloned(),
            Some(rest) => rest.strip_prefix('=').map(String::from),
            None => None,
        })
        .or_else(|| {
            let file = ws
                .colcon
                .defaults
                .as_ref()
                .map(|f| root.join(f))
                .or_else(colcon_defaults_file)?;
            defaults_value(&file, key)
        });
    let ws_path = root.join(&ws.path);
    let mut bases: Vec<PathBuf> = configured.map(|c| ws_path.join(c)).into_iter().collect();
    let default = ws_path.join(default);
    if !bases.contains(&default) {
        bases.push(default);
    }
    bases
}

/// Removes every `flag VALUE` / `flag=VALUE` from `args`; returns the last value.
fn take_option(args: &mut Vec<String>, flag: &str) -> Option<String> {
    let mut value = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].strip_prefix(flag) {
            Some("") if i + 1 < args.len() => {
                value = Some(args.remove(i + 1));
                args.remove(i);
            }
            Some(rest) if rest.starts_with('=') => {
                value = Some(rest[1..].to_string());
                args.remove(i);
            }
            _ => i += 1,
        }
    }
    value
}

/// The first of `bases` that is a directory now.
pub fn existing_base(bases: Vec<PathBuf>) -> Option<PathBuf> {
    bases.into_iter().find(|b| b.is_dir())
}

/// `source` lines for the ROS base and the given workspaces. Each workspace
/// is looked up at run time in its install base candidates, so one built
/// earlier in the same plan is found. A workspace built in neither is
/// skipped, or stops the command when `required` (underlays of a build).
fn source_lines(ctx: &Context, workspaces: &[String], shell: Shell, required: bool) -> Vec<String> {
    let mut lines = vec![format!(
        "source {}",
        shell_quote(&ros_setup_file(ctx.manifest, shell))
    )];
    for name in workspaces {
        let ws = &ctx.manifest.workspaces[name];
        let bases = colcon_bases(ctx.root, ws, "install-base", "install");
        let mut line = String::new();
        for (i, base) in bases.iter().enumerate() {
            let q = shell_quote(&base.join(format!("setup.{shell}")).to_string_lossy());
            let branch = if i == 0 { "if" } else { "elif" };
            line.push_str(&format!("{branch} [ -f {q} ]; then source {q}; "));
        }
        if required {
            let searched: Vec<String> = bases.iter().map(|b| b.display().to_string()).collect();
            let error = msg!(
                "native.build.underlay_missing",
                name = name,
                paths = searched.join(", ")
            );
            line.push_str(&format!(
                "else echo {} >&2; exit 1; ",
                shell_quote(&error.to_string())
            ));
        }
        line.push_str("fi");
        lines.push(line);
    }
    lines
}

/// The selected Conda interpreter for every CMake Python lookup.
const PYTHON_DEFINES: [&str; 3] = [
    "-DPython3_EXECUTABLE=$CONDA_PREFIX/bin/python",
    "-DPython_EXECUTABLE=$CONDA_PREFIX/bin/python",
    "-DPYTHON_EXECUTABLE=$CONDA_PREFIX/bin/python",
];

/// Output of `build` (and `apply --build`) commands, relative to the project root.
pub const BUILD_LOG: &str = ".rospm/log/build.log";

/// Native builds only: when the calling terminal's Conda environment lacks
/// the modules ROS builds need (its `python3` would win on PATH and in CMake's
/// FindPython, which also reads `$CONDA_PREFIX`), drop it for this command.
/// An environment that can build ROS is left alone. Runs after ROS is sourced.
/// CMake caches under `build_base` still naming the dropped environment
/// would keep using it, so they stop the build, as in a Conda build.
fn deactivate_unusable_conda(note: &str, stale: &str, build_base: Option<&Path>) -> String {
    let caches = match build_base {
        Some(base) => format!("{}/*/CMakeCache.txt", shell_quote(&base.to_string_lossy())),
        None => "/dev/null".into(),
    };
    format!(
        "if [ -n \"${{CONDA_PREFIX:-}}\" ] && ! python3 -c 'import catkin_pkg, em, rclpy._rclpy_pybind11' >/dev/null 2>&1; then \
         root=$(dirname \"$(dirname \"${{CONDA_EXE:-$CONDA_PREFIX/bin/conda}}\")\"); \
         [ -d \"$root/conda-meta\" ] || root=$CONDA_PREFIX; \
         PATH=$(printf '%s' \"$PATH\" | tr : '\\n' | grep -vF -e \"$CONDA_PREFIX/\" -e \"$root/\" | paste -sd:); \
         stale=$(grep -lF -e \"=$CONDA_PREFIX/\" -e \"=$root/\" {} 2>/dev/null || true); \
         unset CONDA_PREFIX CONDA_DEFAULT_ENV CONDA_SHLVL CONDA_PROMPT_MODIFIER CONDA_EXE CONDA_PYTHON_EXE _CE_CONDA _CE_M; \
         hash -r; echo {} >&2; \
         if [ -n \"$stale\" ]; then echo {} >&2; printf '%s\\n' \"$stale\" >&2; exit 1; fi; fi",
        caches,
        shell_quote(note),
        shell_quote(stale)
    )
}

/// The defaults file colcon reads when the manifest names none.
fn colcon_defaults_file() -> Option<PathBuf> {
    if let Some(file) = std::env::var_os("COLCON_DEFAULTS_FILE") {
        return Some(file.into());
    }
    let home = std::env::var_os("COLCON_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".colcon")))?;
    Some(home.join("defaults.yaml"))
}

/// `build.<key>` of a colcon defaults file; `None` when absent or unreadable.
fn defaults_value<T: serde::de::DeserializeOwned>(file: &Path, key: &str) -> Option<T> {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_yaml::from_str::<serde_yaml::Value>(&text).ok())
        .and_then(|v| serde_yaml::from_value(v["build"][key].clone()).ok())
}

/// `build.cmake-args` of a colcon defaults file; empty when absent or unreadable.
fn default_cmake_args(file: &Path) -> Vec<String> {
    defaults_value(file, "cmake-args").unwrap_or_default()
}

/// `rm -f` rospm's own snapshot keyring and Ubuntu snapshot list (only rospm
/// writes these paths) when `ros2.list` is not rospm's: without this they
/// outlive every uninstall, since only the `ros2.list` branch removed them.
fn remove_snapshot_leftovers(plan: &mut Plan, host: &probe::Host) {
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
