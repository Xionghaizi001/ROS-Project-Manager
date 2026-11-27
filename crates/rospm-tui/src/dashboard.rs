//! Project data loaded for the TUI dashboard.

use std::{
    collections::BTreeSet,
    fs,
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use rospm_core::{
    conda,
    introspect::{self, HostFacts},
    lock::{Lock, LOCK_FILE},
    manifest::{CondaBuild, Manifest, ProviderKind, MANIFEST_FILE},
    mirror,
    package_xml::{find_package_files, IGNORE_MARKERS},
    plan::Plan,
    platform::Platform,
    probe::{Host, Status},
    provider::{self, Context as ProviderContext},
    worker::WorkspaceLock,
    Error,
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
    pub(crate) debug_tools: Option<[Status; crate::DEBUG_TOOLS.len()]>,
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

pub(crate) struct ScannedProject {
    pub(crate) manifest: PathBuf,
    pub(crate) needs_manifest: bool,
    pub(crate) conda: Option<String>,
}

impl ScannedProject {
    fn new(manifest: PathBuf, environments: &BTreeSet<String>) -> Self {
        let (needs_manifest, name) = match read_manifest(&manifest) {
            Ok(manifest) => (false, conda_project_name(&manifest).map(str::to_owned)),
            Err(error) if can_recover(&error) => (
                true,
                manifest
                    .parent()
                    .and_then(|root| root.canonicalize().ok())
                    .filter(|root| !root.join("pixi.toml").is_file())
                    .and_then(|root| {
                        root.file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                    }),
            ),
            Err(_) => (false, None),
        };
        let conda = name.filter(|name| {
            environments.contains(name)
                && !name.chars().any(char::is_control)
                && CondaBuild {
                    name: Some(name.clone()),
                    ..Default::default()
                }
                .validate()
                .is_ok()
        });
        Self {
            manifest,
            needs_manifest,
            conda,
        }
    }
}

pub(crate) fn conda_project_name(manifest: &Manifest) -> Option<&str> {
    (manifest.environment.provider == ProviderKind::Native && manifest.build.conda.is_none())
        .then_some(manifest.project.name.as_str())
}

pub(crate) fn read_manifest(path: &Path) -> rospm_core::Result<Manifest> {
    let metadata = fs::metadata(path).map_err(|error| Error::io(path, error))?;
    if !metadata.is_file() {
        return Err(Error::io(path, io::Error::other("expected a regular file")));
    }
    Manifest::load(path)
}

pub(crate) fn can_recover(error: &Error) -> bool {
    match error {
        Error::ManifestParse { .. } | Error::Validation(_) => true,
        Error::Io { source, .. } => {
            matches!(
                source.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidData
            )
        }
        _ => false,
    }
}

pub(crate) fn recover_manifest(path: &Path, busy_message: &str) -> Result<Manifest> {
    let root = path.parent().context("manifest has no parent directory")?;
    let _lease = WorkspaceLock::try_acquire_mutation(root)?.context(busy_message.to_owned())?;
    let facts = HostFacts::gather(&Host::probe());
    let draft = introspect::draft(root, &facts).render();
    let manifest = Manifest::parse(&draft, path)?;

    let existing = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => match read_manifest(path) {
            Ok(current) => return Ok(current),
            Err(error) if can_recover(&error) => true,
            Err(error) => return Err(error.into()),
        },
        Ok(_) => return Err(Error::io(path, io::Error::other("expected a regular file")).into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(Error::io(path, error).into()),
    };

    write_manifest(path, &draft, None, |temporary| {
        if existing {
            let mut backup = path.as_os_str().to_os_string();
            backup.push(".bak");
            for number in 0.. {
                let mut candidate = backup.clone();
                if number != 0 {
                    candidate.push(format!(".{number}"));
                }
                match fs::hard_link(path, candidate) {
                    Ok(()) => break,
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(error),
                }
            }
            fs::rename(temporary, path)
        } else {
            fs::hard_link(temporary, path)
        }
    })?;
    Ok(manifest)
}

pub(crate) fn add_conda(path: &Path, name: &str, busy_message: &str) -> Result<Manifest> {
    let root = path.parent().context("manifest has no parent directory")?;
    let _lease = WorkspaceLock::try_acquire_mutation(root)?.context(busy_message.to_owned())?;
    let target = path
        .canonicalize()
        .map_err(|error| Error::io(path, error))?;
    let target_root = target
        .parent()
        .context("manifest has no parent directory")?;
    // A symlink can share its manifest with a separately locked workspace.
    let _target_lease = if target_root != root.canonicalize()? {
        Some(WorkspaceLock::try_acquire_mutation(target_root)?.context(busy_message.to_owned())?)
    } else {
        None
    };
    let metadata = fs::metadata(&target).map_err(|error| Error::io(&target, error))?;
    if !metadata.is_file() {
        return Err(Error::io(path, io::Error::other("expected a regular file")).into());
    }
    let contents = fs::read_to_string(&target).map_err(|error| Error::io(&target, error))?;
    let manifest = Manifest::parse(&contents, path)?;
    if conda_project_name(&manifest) != Some(name) {
        return Ok(manifest);
    }

    let mut doc = contents.parse::<toml_edit::DocumentMut>()?;
    let mut conda = toml_edit::Table::new();
    conda.insert("name", toml_edit::value(name));
    doc.entry("build")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_like_mut()
        .context("expected a build table")?
        .insert("conda", toml_edit::Item::Table(conda));
    let updated = doc.to_string();
    let manifest = Manifest::parse(&updated, path)?;
    write_manifest(&target, &updated, Some(metadata.permissions()), |temp| {
        fs::rename(temp, &target)
    })?;
    Ok(manifest)
}

fn write_manifest(
    path: &Path,
    text: &str,
    permissions: Option<fs::Permissions>,
    commit: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<()> {
    let mut temporary = path.as_os_str().to_os_string();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    temporary.push(format!(".{stamp}.tmp"));
    let temporary = PathBuf::from(temporary);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    if let Some(permissions) = &permissions {
        options.mode(permissions.mode() & 0o777);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|error| Error::io(&temporary, error))?;
    let result = (|| {
        file.write_all(text.as_bytes())?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.sync_all()?;
        commit(&temporary)
    })();
    let _ = fs::remove_file(&temporary);
    result.map_err(|error| Error::io(path, error))?;
    Ok(())
}

impl Dashboard {
    pub(crate) fn scan_projects(root: &Path, known_manifest: Option<&Path>) -> Vec<ScannedProject> {
        let environments = conda::named_environments();
        let mut pending = vec![(root.to_path_buf(), true)];
        let mut projects = Vec::new();
        while let Some((directory, detect_workspace)) = pending.pop() {
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            let manifest = directory.join(MANIFEST_FILE);
            let has_manifest = fs::symlink_metadata(&manifest)
                .is_ok_and(|metadata| metadata.is_file() || metadata.is_symlink());
            let detect_workspace = detect_workspace
                && !IGNORE_MARKERS
                    .iter()
                    .any(|marker| directory.join(marker).exists());
            let has_workspace = detect_workspace
                && (directory.join(LOCK_FILE).is_file()
                    || directory.join(".rospm").is_dir()
                    || directory.join("package.xml").is_file()
                    || !find_package_files(&directory.join("src")).is_empty());
            let is_project = has_manifest || has_workspace || known_manifest == Some(&manifest);
            if is_project {
                projects.push(ScannedProject::new(manifest, &environments));
            }
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
                        pending.push((path, detect_workspace && !is_project));
                    }
                }
            }
        }
        if let Some(manifest) =
            known_manifest.filter(|path| !projects.iter().any(|project| project.manifest == *path))
        {
            projects.push(ScannedProject::new(manifest.to_path_buf(), &environments));
        }
        let root_manifest = root.join(MANIFEST_FILE);
        // Drafting can discover workspaces below a project root that has no marker of its own.
        if !projects
            .iter()
            .any(|project| project.manifest == root_manifest)
            && introspect::has_workspace_inputs(root)
        {
            projects.push(ScannedProject::new(root_manifest.clone(), &environments));
        }
        projects.sort_by(|a, b| {
            (a.manifest != root_manifest, &a.manifest)
                .cmp(&(b.manifest != root_manifest, &b.manifest))
        });
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
        Ok(read_manifest(&path)
            .ok()
            .map(|manifest| Self::from_manifest(path, manifest, text)))
    }

    pub(crate) fn from_manifest(path: PathBuf, manifest: Manifest, text: &Text) -> Self {
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
        Self {
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
            debug_tools: None,
            diagnostics,
            compile_packages,
            run,
            installing: false,
            root: root.canonicalize().unwrap_or_else(|_| root.to_path_buf()),
        }
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

    fn test_root() -> PathBuf {
        std::env::temp_dir().join(format!(
            "rospm-tui-scan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn project_scan_finds_manifests_and_workspaces_and_skips_generated_directories() {
        let root = test_root();
        let expected = root.join("nested/project").join(MANIFEST_FILE);
        let missing = root.join("workspace").join(MANIFEST_FILE);
        for path in [
            expected.clone(),
            root.join(".cache/hidden").join(MANIFEST_FILE),
            root.join("target/generated").join(MANIFEST_FILE),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let package = root.join("workspace/src/repo/pkg/package.xml");
        fs::create_dir_all(package.parent().unwrap()).unwrap();
        fs::write(package, "<package><name>pkg</name></package>").unwrap();
        let found = Dashboard::scan_projects(&root, None);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            found
                .into_iter()
                .map(|project| (project.manifest, project.needs_manifest))
                .collect::<Vec<_>>(),
            [
                (root.join(MANIFEST_FILE), true),
                (expected, true),
                (missing, true)
            ]
        );
    }

    #[test]
    fn manifest_recovery_preserves_existing_backups_and_accepts_restored_files() {
        let root = test_root();
        let package = root.join("src/example/package.xml");
        fs::create_dir_all(package.parent().unwrap()).unwrap();
        fs::write(package, "<package><name>example</name></package>").unwrap();
        let path = root.join(MANIFEST_FILE);
        let text = Text::default();
        assert!(Dashboard::load(Some(&path), &text).unwrap().is_none());
        let manifest = recover_manifest(&path, "busy").unwrap();
        assert_eq!(manifest.workspaces["main"].path, Path::new("."));
        let generated = fs::read(&path).unwrap();
        assert_eq!(
            recover_manifest(&path, "busy").unwrap().project.name,
            manifest.project.name
        );
        assert_eq!(fs::read(&path).unwrap(), generated);

        fs::write(&path, "[broken").unwrap();
        assert!(Dashboard::load(Some(&path), &text).unwrap().is_none());
        fs::write(root.join("rospm.toml.bak"), "previous backup").unwrap();
        recover_manifest(&path, "busy").unwrap();
        assert_eq!(
            fs::read(root.join("rospm.toml.bak")).unwrap(),
            b"previous backup"
        );
        assert_eq!(fs::read(root.join("rospm.toml.bak.1")).unwrap(), b"[broken");
        assert!(read_manifest(&path).is_ok());
        assert!(!fs::read_dir(&root)
            .unwrap()
            .flatten()
            .any(|entry| entry.path().extension().is_some_and(|ext| ext == "tmp")));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn same_name_conda_is_added_without_overwriting_the_manifest_configuration() {
        let root = test_root().join("different-directory-name");
        fs::create_dir_all(&root).unwrap();
        let path = root.join(MANIFEST_FILE);
        let environments = BTreeSet::from(["minimal".into()]);
        let original = include_str!("../../rospm-core/tests/fixtures/minimal.toml");
        for build in [
            "",
            "[build]\nverify = [\"echo ok\"] # build check\n\n",
            "build = { verify = [\"echo ok\"] } # build check\n\n",
            "build.verify = [\"echo ok\"] # build check\n\n",
        ] {
            let contents = original.replacen("[project]", &format!("{build}[project]"), 1);
            fs::write(&path, &contents).unwrap();
            assert_eq!(
                ScannedProject::new(path.clone(), &environments)
                    .conda
                    .as_deref(),
                Some("minimal")
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), contents);

            let manifest = add_conda(&path, "minimal", "busy").unwrap();
            assert_eq!(
                manifest.build.conda.unwrap().name.as_deref(),
                Some("minimal")
            );
            let updated = fs::read_to_string(&path).unwrap();
            if !build.is_empty() {
                assert_eq!(manifest.build.verify, ["echo ok"]);
                assert!(updated.contains("# build check"));
            }
            assert!(ScannedProject::new(path.clone(), &environments)
                .conda
                .is_none());
            add_conda(&path, "minimal", "busy").unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), updated);
        }
        fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }
}
