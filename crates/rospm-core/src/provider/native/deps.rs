//! `rospm deps check` / `rospm deps install` for the native provider
//! (spec §4, roadmap P2): rosdep keys of every workspace versus what is
//! installed; `install` hands the work to rosdep.
//!
//! `probe` shells out to `rosdep check` / `rosdep resolve` directly (unlike
//! the other native commands it does not build a [`Plan`]: it only reads).
//! Its parsing of rosdep's stdout/stderr is split into pure functions so it
//! can be unit-tested with canned text: this host has `rosdep` installed but
//! no ROS distro registered, so real invocations of `resolve`/`check` for ROS
//! keys fail for lack of a `rosdep update` cache. That must surface as
//! `Status::Unknown`, never as a panic or a hard error — `deps check` always
//! exits 0.

use std::collections::BTreeSet;
use std::process::{Command as OsCommand, Output};

use serde::Serialize;

use crate::error::Result;
use crate::message::Message;
use crate::msg;
use crate::package_xml::WorkspacePackages;
use crate::plan::{Command, Plan};
use crate::probe::{EnvReport, Status, ROSDEP_DEFAULT_LIST};
use crate::provider::Context;

/// Package/key counts for one workspace: the header lines of `deps check`.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceDeps {
    pub name: String,
    pub packages: usize,
    pub keys: usize,
}

/// `rospm deps check`'s result. Dependency keys declared across every
/// workspace's `package.xml`, split into "internal" (provided by a package
/// anywhere in the project), "external" (left to rosdep) and "skipped"
/// (`dependencies.rosdep.skip_keys`); `environment` reports what is actually
/// resolved/installed for the external keys.
#[derive(Debug, Clone, Serialize)]
pub struct DepsReport {
    pub workspaces: Vec<WorkspaceDeps>,
    pub internal: BTreeSet<String>,
    pub external: BTreeSet<String>,
    pub skipped: BTreeSet<String>,
    pub environment: EnvReport,
}

/// Outcome of parsing `rosdep check --from-paths <src> --ignore-src
/// --rosdistro <d>` (spec §4 `deps check`). Pure function over the command's
/// captured stdout/stderr so it can be unit-tested without a working rosdep
/// index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckOutcome {
    /// True when rosdep printed "All system dependencies have been satisfied".
    pub satisfied: bool,
    /// `(installer, package)` pairs from the "not been satisfied" block.
    pub missing: Vec<(String, String)>,
    /// Keys rosdep could not resolve at all (the "could not have their
    /// rosdep keys resolved" error block).
    pub unresolved_keys: Vec<String>,
}

/// Parse `rosdep check`'s stdout/stderr.
pub fn parse_rosdep_check(stdout: &str, stderr: &str) -> CheckOutcome {
    let mut out = CheckOutcome::default();

    if stdout.contains("All system dependencies have been satisfied") {
        out.satisfied = true;
    }

    let mut in_missing = false;
    for line in stdout.lines() {
        if line.contains("System dependencies have not been satisfied") {
            in_missing = true;
            continue;
        }
        if !in_missing {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            in_missing = false;
            continue;
        }
        match trimmed
            .split_once('\t')
            .or_else(|| trimmed.split_once(char::is_whitespace))
        {
            Some((installer, pkg)) => {
                out.missing
                    .push((installer.trim().to_string(), pkg.trim().to_string()));
            }
            None => in_missing = false,
        }
    }

    for text in [stdout, stderr] {
        let mut in_error = false;
        for line in text.lines() {
            if line.contains("could not have their rosdep keys resolved") {
                in_error = true;
                continue;
            }
            if !in_error {
                continue;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                in_error = false;
                continue;
            }
            // An explanatory line such as "to arbitrary os-specific keys:".
            if trimmed.ends_with(':') {
                continue;
            }
            if line.starts_with(char::is_whitespace) || trimmed.starts_with('-') {
                out.unresolved_keys
                    .push(trimmed.trim_start_matches('-').trim().to_string());
            } else {
                in_error = false;
            }
        }
    }

    out
}

/// Parse `rosdep resolve <key> --rosdistro <d>`'s stdout: the apt package
/// names from the `#apt` installer block. Empty when the key resolves to a
/// non-apt installer, or there is no installer block at all.
pub fn parse_rosdep_resolve(stdout: &str) -> Vec<String> {
    let mut lines = stdout.lines();
    while let Some(line) = lines.next() {
        if line.trim() == "#apt" {
            for next in lines.by_ref() {
                let trimmed = next.trim();
                if trimmed.is_empty() {
                    continue;
                }
                return trimmed.split_whitespace().map(str::to_string).collect();
            }
        }
    }
    Vec::new()
}

/// `rospm deps check` (spec §4): scan every workspace, classify dependency
/// keys, and probe the external ones through rosdep + the package database.
pub fn probe(ctx: &Context) -> Result<DepsReport> {
    let m = ctx.manifest;
    let order = m.build_order()?;

    // Scan every workspace before classifying anything: "internal" means
    // "provided anywhere in the project" (spec §4), not just within the same
    // workspace, so `WorkspacePackages::external_keys` (which is scoped to a
    // single workspace) cannot be used directly here.
    let mut scans: Vec<(String, WorkspacePackages)> = Vec::new();
    for name in &order {
        let ws = &m.workspaces[name];
        let dir = ctx.root.join(&ws.path).join("src");
        scans.push((name.clone(), WorkspacePackages::scan(&dir)));
    }
    let mut all_names: BTreeSet<String> = BTreeSet::new();
    for (_, pkgs) in &scans {
        all_names.extend(pkgs.names().into_iter().map(str::to_string));
    }

    let mut workspaces = Vec::new();
    let mut internal: BTreeSet<String> = BTreeSet::new();
    let mut external_candidate: BTreeSet<String> = BTreeSet::new();
    for (name, pkgs) in &scans {
        let mut ws_keys: BTreeSet<String> = BTreeSet::new();
        for p in &pkgs.packages {
            for d in &p.depends {
                ws_keys.insert(d.name.clone());
            }
        }
        for key in &ws_keys {
            if all_names.contains(key) {
                internal.insert(key.clone());
            } else {
                external_candidate.insert(key.clone());
            }
        }
        workspaces.push(WorkspaceDeps {
            name: name.clone(),
            packages: pkgs.packages.len(),
            keys: ws_keys.len(),
        });
    }

    let skip: BTreeSet<String> = m.dependencies.rosdep.skip_keys.iter().cloned().collect();
    let skipped: BTreeSet<String> = external_candidate.intersection(&skip).cloned().collect();
    let external: BTreeSet<String> = external_candidate.difference(&skip).cloned().collect();

    let mut environment = if !m.dependencies.rosdep.enabled {
        let mut r = EnvReport::default();
        r.push(
            "deps",
            Message::text("rosdep"),
            msg!("deps.report.rosdep_required"),
            msg!("deps.report.rosdep_disabled"),
            Status::Info,
        );
        r
    } else if !ctx.host.has_tool("rosdep") {
        let mut r = EnvReport::default();
        r.push(
            "tool",
            Message::text("rosdep"),
            msg!("deps.report.rosdep_required"),
            msg!("deps.report.rosdep_not_found"),
            Status::Unknown,
        );
        for key in &external {
            r.push(
                "deps",
                key,
                msg!("deps.report.rosdep_key"),
                msg!("deps.report.not_probed"),
                Status::Unknown,
            );
        }
        r
    } else {
        probe_rosdep(ctx, &order, &external, &skip)
    };
    // A package.xml that does not parse contributes no keys: say so, like `check`.
    for (name, pkgs) in &scans {
        for (file, err) in &pkgs.errors {
            environment.push(
                "workspace",
                format!("{name}: {}", file.display()),
                msg!("probe.workspace.valid_package_xml"),
                err.clone(),
                Status::Mismatch,
            );
        }
    }

    Ok(DepsReport {
        workspaces,
        internal,
        external,
        skipped,
        environment,
    })
}

fn run(mut cmd: OsCommand) -> Option<Output> {
    cmd.output().ok()
}

/// Shell out to `rosdep check` (per workspace) and `rosdep resolve` (per
/// external key), and cross-check the resolved apt packages against
/// `ctx.host.packages`.
fn probe_rosdep(
    ctx: &Context,
    order: &[String],
    external: &BTreeSet<String>,
    skip: &BTreeSet<String>,
) -> EnvReport {
    let mut r = EnvReport::default();
    let distro = ctx.manifest.project.ros_distro.as_str();
    // Through the probed Host, like every other tool (tests substitute one).
    let rosdep = ctx
        .host
        .tool("rosdep")
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| "rosdep".into());

    // `rosdep check`, per workspace: run for its side information (which
    // keys rosdep flat-out fails to resolve) independently of whether the
    // resolved apt packages happen to already be installed (checked below
    // via `PackageDb`, not by trusting rosdep's own "satisfied" verdict).
    let mut unresolved_from_check: BTreeSet<String> = BTreeSet::new();
    for name in order {
        let ws = &ctx.manifest.workspaces[name];
        let ws_path = ctx.root.join(&ws.path);
        if !ws_path.join("src").is_dir() {
            continue;
        }
        let mut cmd = OsCommand::new(&rosdep);
        cmd.args([
            "check",
            "--from-paths",
            "src",
            "--ignore-src",
            "--rosdistro",
            distro,
        ])
        .current_dir(&ws_path);
        if !skip.is_empty() {
            cmd.arg("--skip-keys")
                .arg(skip.iter().cloned().collect::<Vec<_>>().join(" "));
        }
        if let Some(output) = run(cmd) {
            let outcome = parse_rosdep_check(
                &String::from_utf8_lossy(&output.stdout),
                &String::from_utf8_lossy(&output.stderr),
            );
            unresolved_from_check.extend(outcome.unresolved_keys);
        }
    }

    for key in external {
        if unresolved_from_check.contains(key) {
            r.push(
                "deps",
                key,
                msg!("deps.report.rosdep_key"),
                msg!("deps.report.unresolved"),
                Status::Unknown,
            );
            continue;
        }

        let mut cmd = OsCommand::new(&rosdep);
        cmd.args(["resolve", key.as_str(), "--rosdistro", distro]);
        let Some(output) = run(cmd) else {
            r.push(
                "deps",
                key,
                msg!("deps.report.rosdep_key"),
                msg!("deps.report.rosdep_not_found"),
                Status::Unknown,
            );
            continue;
        };
        if !output.status.success() {
            let reason = String::from_utf8_lossy(&output.stderr)
                .lines()
                .next_back()
                .unwrap_or_default()
                .trim()
                .to_string();
            r.push(
                "deps",
                key,
                msg!("deps.report.rosdep_key"),
                if reason.is_empty() {
                    msg!("deps.report.unresolved")
                } else {
                    msg!("deps.report.unresolved_reason", reason = reason)
                },
                Status::Unknown,
            );
            continue;
        }

        let packages = parse_rosdep_resolve(&String::from_utf8_lossy(&output.stdout));
        if packages.is_empty() {
            r.push(
                "deps",
                key,
                msg!("deps.report.rosdep_key"),
                msg!("deps.report.no_apt_package"),
                Status::Unknown,
            );
            continue;
        }
        let expected = packages.join(" ");
        match ctx.host.packages.status(&packages) {
            None => r.push(
                "deps",
                key,
                expected,
                msg!("probe.dpkg_unavailable"),
                Status::Unknown,
            ),
            Some(map) => {
                let all_installed = packages
                    .iter()
                    .all(|p| map.get(p).is_some_and(|s| s.installed));
                if all_installed {
                    let versions: Vec<String> = packages
                        .iter()
                        .filter_map(|p| map.get(p).and_then(|s| s.version.clone()))
                        .collect();
                    let actual = if versions.is_empty() {
                        msg!("probe.installed")
                    } else {
                        Message::text(versions.join(", "))
                    };
                    r.push("deps", key, expected, actual, Status::Ok);
                } else {
                    r.push(
                        "deps",
                        key,
                        expected,
                        msg!("probe.not_installed"),
                        Status::Missing,
                    );
                }
            }
        }
    }

    r
}

/// `rospm deps install [--dry-run]` (spec §4): hand the external keys to
/// rosdep. Mirrors `Native::plan_apply`'s rosdep step, except: `rosdep
/// update` only runs when the host has not already run one (`apply` always
/// updates), and a missing `rosdep init` becomes an advisory note pointing at
/// `rospm apply` (which bootstraps it) instead of running `rosdep init` here.
pub fn plan_install(ctx: &Context) -> Result<Plan> {
    let m = ctx.manifest;
    let host = ctx.host;
    let distro = m.project.ros_distro;
    let mut plan = Plan::new(msg!(
        "deps.plan.title",
        name = m.project.name,
        distro = distro
    ));

    if !m.dependencies.rosdep.enabled {
        plan.note(msg!("deps.plan.rosdep_disabled"));
        return Ok(plan);
    }

    let mirror = ctx.mirror;
    let order = m.build_order()?;

    if mirror.mirrors_rosdistro() {
        if host.rosdep.initialized {
            plan.note(msg!(
                "native.apply.rosdep_list_rewrite",
                file = ROSDEP_DEFAULT_LIST,
                name = mirror.name
            ));
        }
        plan.write_file_sudo(ROSDEP_DEFAULT_LIST, mirror.rosdep_sources_list());
    } else if !host.rosdep.initialized {
        plan.note(msg!("deps.plan.rosdep_not_initialized"));
    }

    if !host.rosdep.updated {
        let mut update = Command::new("rosdep")
            .arg("update")
            .arg("--rosdistro")
            .arg(distro.as_str());
        if mirror.mirrors_rosdistro() {
            update = update.env("ROSDISTRO_INDEX_URL", mirror.rosdistro_index_url());
        }
        plan.run(update);
    }

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

    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_satisfied_check_output() {
        let out = parse_rosdep_check(
            "#All required rosdeps installed successfully\nAll system dependencies have been satisfied\n",
            "",
        );
        assert!(out.satisfied);
        assert!(out.missing.is_empty());
        assert!(out.unresolved_keys.is_empty());
    }

    #[test]
    fn parses_unsatisfied_check_output() {
        let stdout =
            "System dependencies have not been satisfied:\napt\tlibopencv-dev\napt\tlibfoo-dev\n";
        let out = parse_rosdep_check(stdout, "");
        assert!(!out.satisfied);
        assert_eq!(
            out.missing,
            vec![
                ("apt".to_string(), "libopencv-dev".to_string()),
                ("apt".to_string(), "libfoo-dev".to_string()),
            ]
        );
    }

    #[test]
    fn parses_unresolvable_key_error() {
        let stderr = "ERROR: the following packages/stacks could not have their rosdep keys resolved\nto arbitrary os-specific keys:\n\tunknown_key\n";
        let out = parse_rosdep_check("", stderr);
        assert!(!out.satisfied);
        assert_eq!(out.unresolved_keys, vec!["unknown_key".to_string()]);
    }

    #[test]
    fn parses_resolve_apt_block() {
        assert_eq!(
            parse_rosdep_resolve("#apt\nros-humble-rclcpp\n"),
            vec!["ros-humble-rclcpp".to_string()]
        );
        assert_eq!(
            parse_rosdep_resolve("#apt\npkg1 pkg2\n"),
            vec!["pkg1".to_string(), "pkg2".to_string()]
        );
    }

    #[test]
    fn resolve_with_no_apt_block_is_empty() {
        assert!(parse_rosdep_resolve("#pip\nnumpy\n").is_empty());
        assert!(parse_rosdep_resolve("").is_empty());
    }
}
