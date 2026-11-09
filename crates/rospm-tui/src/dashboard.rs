//! Project data loaded for the TUI dashboard.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rospm_core::{
    lock::Lock,
    manifest::{Manifest, ProviderKind},
    mirror,
    plan::Plan,
    platform::Platform,
    probe::Host,
    provider::{self, Context as ProviderContext},
};

use crate::Text;

pub(crate) struct Dashboard {
    pub(crate) manifest: PathBuf,
    pub(crate) project: String,
    pub(crate) provider: ProviderKind,
    pub(crate) workspaces: Vec<(String, String, Vec<String>)>,
    pub(crate) conda: String,
    pub(crate) apply_plan: Vec<String>,
    pub(crate) uninstall_plan: Vec<String>,
    pub(crate) check: Vec<String>,
    pub(crate) report: rospm_core::probe::EnvReport,
    pub(crate) verify: Vec<String>,
    pub(crate) deps: Vec<String>,
    pub(crate) deps_report: rospm_core::probe::EnvReport,
    pub(crate) ros_installed: bool,
    pub(crate) diagnostics: Vec<String>,
    pub(crate) compile_packages: Vec<crate::packages::BuildPackage>,
    pub(crate) root: PathBuf,
}

impl Dashboard {
    pub(crate) fn load(path: Option<&Path>, text: &Text) -> Result<Option<Self>> {
        let path = match path {
            Some(path) => path.to_path_buf(),
            None => {
                let cwd = std::env::current_dir().context("failed to read current directory")?;
                match Manifest::find(&cwd) {
                    Some(path) => path,
                    None => return Ok(None),
                }
            }
        };
        let manifest = Manifest::load(&path)
            .with_context(|| format!("failed to load manifest `{}`", path.display()))?;
        let conda = manifest
            .build
            .conda
            .as_ref()
            .map(|c| {
                c.name
                    .as_deref()
                    .map(|n| format!("name: {n}"))
                    .or_else(|| c.prefix.as_ref().map(|p| format!("path: {}", p.display())))
                    .or_else(|| {
                        c.managed
                            .as_ref()
                            .map(|m| format!("managed (Python {})", m.python))
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_else(|| "system".to_string());
        let workspaces = manifest
            .workspaces
            .iter()
            .map(|(name, ws)| {
                (
                    name.clone(),
                    ws.path.display().to_string(),
                    ws.colcon.build_args.clone(),
                )
            })
            .collect::<Vec<_>>();
        let distro = manifest.project.ros_distro.to_string();
        let root = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let compile_packages = crate::packages::load(&manifest, root);
        let mut check = Vec::new();
        let mut report = rospm_core::probe::EnvReport::default();
        let mut verify = Vec::new();
        let mut deps = Vec::new();
        let mut deps_report = rospm_core::probe::EnvReport::default();
        let mut ros_installed = false;
        let mut apply_plan = vec![
            format!("provider  {}", manifest.environment.provider),
            format!("ROS       {distro}"),
            "workspaces".to_string(),
        ];
        let mut uninstall_plan = vec![format!("ROS       {distro}"), "apt source".to_string()];
        if let Some(platform) = Platform::current() {
            let host = Host::probe();
            // Match the distro targeted by the uninstall plan. An apt source
            // alone does not mean ROS is installed.
            ros_installed = host.has_ros_distro(&distro)
                || host
                    .packages
                    .installed_with_prefix(&format!("ros-{distro}-"))
                    .is_some_and(|packages| !packages.is_empty());
            let provider = provider::for_manifest(&manifest);
            let ctx = ProviderContext {
                venv: None,
                manifest: &manifest,
                root,
                platform,
                locked: false,
                lock: None,
                offline: false,
                host: &host,
                mirror: mirror::OFFICIAL,
            };
            match provider.probe_env(&ctx) {
                Ok(value) => report = value,
                Err(error) => check.push(format!("! {error}")),
            }
            match provider.probe_deps(&ctx) {
                Ok(report) => {
                    deps = report
                        .workspaces
                        .iter()
                        .map(|w| format!("{}  packages={} keys={}", w.name, w.packages, w.keys))
                        .collect();
                    deps.push(format!(
                        "internal={} external={} skipped={}",
                        report.internal.len(),
                        report.external.len(),
                        report.skipped.len()
                    ));
                    deps.push(format!(
                        "environment problems={} unknown={}",
                        report.environment.problems(),
                        report.environment.unknown()
                    ));
                    deps_report = report.environment;
                }
                Err(error) => deps.push(format!("! {error}")),
            }
            match provider.plan_apply(&ctx) {
                Ok(plan) => apply_plan = plan_lines(&plan),
                Err(error) => apply_plan.push(format!("! {error}")),
            }
            match provider.plan_uninstall(&ctx, false) {
                Ok(plan) => uninstall_plan = plan_lines(&plan),
                Err(error) => uninstall_plan.push(format!("! {error}")),
            }
            match load_lock(root) {
                Err(error) => verify.push(format!("! {error}")),
                Ok(None) => verify.push("! no lock file found".to_string()),
                Ok(Some(lock)) => {
                    let vctx = ProviderContext {
                        lock: Some(&lock),
                        ..ctx
                    };
                    match provider.verify(&vctx, &lock) {
                        Ok(report) => verify = env_lines(&report, text),
                        Err(error) => verify.push(format!("! {error}")),
                    }
                }
            }
        } else {
            let message = "host platform could not be detected".to_string();
            check.push(format!("! {message}"));
            verify.push(format!("! {message}"));
            deps.push(format!("! {message}"));
        }
        let diagnostics = crate::logs::diagnostics(root);
        Ok(Some(Self {
            manifest: path.clone(),
            project: manifest.project.name,
            provider: manifest.environment.provider,
            workspaces,
            conda,
            apply_plan,
            uninstall_plan,
            check,
            report,
            verify,
            deps,
            deps_report,
            ros_installed,
            diagnostics,
            compile_packages,
            root: root.canonicalize().unwrap_or_else(|_| root.to_path_buf()),
        }))
    }

    pub(crate) fn refresh_runtime(&mut self) {
        self.diagnostics = crate::logs::diagnostics(&self.root);
    }
}

fn load_lock(root: &Path) -> Result<Option<Lock>> {
    let path = root.join("rospm.lock");
    if path.is_file() {
        Ok(Some(Lock::load(&path)?))
    } else {
        Ok(None)
    }
}

fn env_lines(report: &rospm_core::probe::EnvReport, text: &Text) -> Vec<String> {
    report
        .items
        .iter()
        .take(24)
        .map(|item| {
            format!(
                "{}  {}  {}",
                item.subject,
                status_label(item.status, text),
                item.actual
            )
        })
        .collect()
}

fn status_label(status: rospm_core::probe::Status, text: &Text) -> &str {
    match status {
        rospm_core::probe::Status::Ok => &text.status_ok,
        rospm_core::probe::Status::Missing => &text.status_missing,
        rospm_core::probe::Status::Mismatch => &text.status_mismatch,
        rospm_core::probe::Status::Unknown => &text.status_unknown,
        rospm_core::probe::Status::Info => &text.status_info,
    }
}

fn plan_lines(plan: &Plan) -> Vec<String> {
    let mut lines = vec![format!("{}  steps={}", plan.title, plan.steps.len())];
    lines.extend(plan.steps.iter().map(step_line));
    lines
}

fn step_line(step: &rospm_core::plan::Step) -> String {
    let line = match step {
        rospm_core::plan::Step::Run(command) => command.render(),
        rospm_core::plan::Step::Conda(_) => "conda environment step".to_string(),
        rospm_core::plan::Step::WriteFile { path, .. } => format!("write {}", path.display()),
        rospm_core::plan::Step::FetchKey { url, dest, .. } => {
            format!("fetch {url} -> {}", dest.display())
        }
        rospm_core::plan::Step::Note(message) => format!("note: {message}"),
    };
    line
}
