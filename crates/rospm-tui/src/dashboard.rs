//! Project data loaded for the TUI dashboard.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rospm_core::{
    lock::Lock,
    manifest::{Manifest, ProviderKind, MANIFEST_FILE},
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
    pub(crate) conda: String,
    pub(crate) apply_plan: Vec<String>,
    pub(crate) uninstall_plan: Vec<String>,
    pub(crate) check: Vec<String>,
    pub(crate) report: rospm_core::probe::EnvReport,
    pub(crate) verify: Vec<String>,
    pub(crate) deps: Vec<String>,
    pub(crate) deps_report: rospm_core::probe::EnvReport,
    pub(crate) components: Vec<Component>,
    pub(crate) ros_installed: bool,
    pub(crate) diagnostics: Vec<String>,
    pub(crate) compile_packages: Vec<crate::packages::BuildPackage>,
    pub(crate) root: PathBuf,
    pub(crate) run: rospm_core::run::WorkspaceRun,
    pub(crate) installing: bool,
}

pub(crate) struct Component {
    pub(crate) kind: &'static str,
    pub(crate) name: String,
    pub(crate) actual: String,
    pub(crate) status: rospm_core::probe::Status,
}

impl Dashboard {
    pub(crate) fn scan_projects(root: &Path) -> Vec<PathBuf> {
        let mut pending = vec![root.to_path_buf()];
        let mut projects = Vec::new();
        while let Some(directory) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if !name.starts_with('.')
                        && !matches!(
                            name.as_ref(),
                            "build" | "install" | "log" | "node_modules" | "target" | ".venv"
                        )
                    {
                        pending.push(path);
                    }
                } else if path.file_name().is_some_and(|name| name == MANIFEST_FILE) {
                    projects.push(path);
                }
            }
        }
        projects.sort();
        projects
    }

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
        let distro = manifest.project.ros_distro.to_string();
        let root = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let compile_packages = crate::packages::load(&manifest, root);
        let run = rospm_core::run::scan_project(&manifest, root);
        let mut check = Vec::new();
        let mut report = rospm_core::probe::EnvReport::default();
        let mut verify = Vec::new();
        let mut deps = Vec::new();
        let mut deps_report = rospm_core::probe::EnvReport::default();
        let mut components = Vec::new();
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
            ros_installed = host.has_ros_install(&distro);
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
            components = scan_components(&manifest, &host, platform);
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
            conda,
            apply_plan,
            uninstall_plan,
            check,
            report,
            verify,
            deps,
            deps_report,
            components,
            ros_installed,
            diagnostics,
            compile_packages,
            run,
            installing: false,
            root: root.canonicalize().unwrap_or_else(|_| root.to_path_buf()),
        }))
    }

    pub(crate) fn refresh_runtime(&mut self) {
        self.diagnostics = crate::logs::diagnostics(&self.root);
    }
}

fn scan_components(manifest: &Manifest, host: &Host, platform: Platform) -> Vec<Component> {
    let apt = manifest.resolve_apt(platform).packages;
    let apt_status = host.packages.status(&apt);
    let mut components = apt
        .into_iter()
        .map(|name| {
            let status = match apt_status.as_ref().and_then(|s| s.get(&name)) {
                Some(package) if package.installed => rospm_core::probe::Status::Ok,
                Some(_) => rospm_core::probe::Status::Missing,
                None => rospm_core::probe::Status::Unknown,
            };
            let actual = match apt_status.as_ref().and_then(|s| s.get(&name)) {
                Some(package) if package.installed => package
                    .version
                    .clone()
                    .unwrap_or_else(|| "installed".into()),
                Some(_) => "not installed".into(),
                None => "unavailable".into(),
            };
            Component {
                kind: "deb",
                name,
                actual,
                status,
            }
        })
        .collect::<Vec<_>>();

    let pip_status = host.pip_freeze();
    components.extend(manifest.dependencies.pip.iter().map(|requirement| {
        let name = rospm_core::probe::pip_requirement_name(requirement);
        let installed = pip_status.as_ref().and_then(|packages| packages.get(&name));
        let status = match pip_status.as_ref() {
            Some(_) if installed.is_some() => rospm_core::probe::Status::Ok,
            Some(_) => rospm_core::probe::Status::Missing,
            None => rospm_core::probe::Status::Unknown,
        };
        Component {
            kind: "pip",
            name: requirement.clone(),
            actual: installed.cloned().unwrap_or_else(|| "not installed".into()),
            status,
        }
    }));
    components
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_scan_finds_manifests_and_skips_generated_directories() {
        let root = std::env::temp_dir().join(format!(
            "rospm-tui-scan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let expected = root.join("nested/project").join(MANIFEST_FILE);
        let hidden = root.join(".cache/hidden").join(MANIFEST_FILE);
        let generated = root.join("target/generated").join(MANIFEST_FILE);
        for path in [&expected, &hidden, &generated] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let found = Dashboard::scan_projects(&root);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(found, [expected]);
    }
}
