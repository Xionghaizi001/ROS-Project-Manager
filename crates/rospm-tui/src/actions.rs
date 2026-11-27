use std::{
    cell::Cell,
    io::{self, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::Result;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::Terminal;
use rospm_core::{
    conda::ToolInstallation,
    worker::{Operation, WorkspaceLock},
};

use crate::{
    dashboard,
    navigation::{first_selectable, lock_project},
    runs,
    terminal::{leave_screen, Screen},
    InstallPage, RunBackend, TaskOutcome, Text,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Apply,
    Uninstall,
    Check,
    Verify,
    DepsCheck,
    DepsInstall,
    CreateProject,
}

pub(super) enum PendingConfirmation {
    Action(Action),
    RecoverProject {
        path: PathBuf,
        conda: Option<String>,
    },
    AddProjectConda {
        path: PathBuf,
        name: String,
    },
    Build {
        rebuild: bool,
    },
    InstallDebugTool {
        package: String,
    },
}

impl PendingConfirmation {
    pub(super) fn prompt(&self, text: &Text) -> String {
        match self {
            Self::Action(action) => {
                format!("{} {}", text.confirm_action, action_label(*action, text))
            }
            Self::RecoverProject { .. } => text.confirm_recover_project.clone(),
            Self::AddProjectConda { name, .. } => {
                text.confirm_project_conda.replace("{name}", name)
            }
            Self::Build { rebuild: true } => text.confirm_rebuild.clone(),
            Self::Build { rebuild: false } => text.confirm_build.clone(),
            Self::InstallDebugTool { package } => {
                format!("{} {package}", text.debug_confirm_install)
            }
        }
    }
}

fn action_label(action: Action, text: &Text) -> &str {
    match action {
        Action::Apply => &text.footer_apply,
        Action::Uninstall => &text.footer_uninstall,
        Action::Check => &text.footer_check,
        Action::Verify => &text.footer_verify,
        Action::DepsCheck => &text.footer_deps_check,
        Action::DepsInstall => &text.footer_deps_install,
        Action::CreateProject => &text.footer_create_project,
    }
}

fn task_operation(action: Action) -> Option<Operation> {
    Some(match action {
        Action::Apply => Operation::Apply,
        Action::Uninstall => Operation::Uninstall,
        Action::Check => Operation::Check,
        Action::Verify => Operation::Verify,
        Action::DepsCheck => Operation::DepsCheck,
        Action::DepsInstall => Operation::DepsInstall,
        Action::CreateProject => return None,
    })
}

pub(super) fn task_backend<'a>(
    runs: &'a mut Option<(&mut dyn RunBackend, u8)>,
) -> Option<&'a mut dyn RunBackend> {
    match runs {
        Some((backend, _)) if backend.has_task_worker() => Some(&mut **backend),
        None => None,
        Some(_) => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn install<B: ratatui::backend::Backend + Write>(
    terminal: &mut Terminal<B>,
    screen: &Cell<Screen>,
    manifest: Option<&Path>,
    dashboard: &mut Option<dashboard::Dashboard>,
    install_page: &mut Option<InstallPage>,
    focused_row: &mut Option<usize>,
    scroll: &mut usize,
    text: &Text,
    install_ros: &mut impl FnMut() -> Result<()>,
    install_components: &mut impl FnMut() -> Result<()>,
    backend: Option<&mut dyn RunBackend>,
    last_runtime_refresh: &mut Instant,
) -> Result<()> {
    if let Some(backend) = backend {
        let root = dashboard
            .as_ref()
            .map(|project| project.root.as_path())
            .unwrap_or_else(|| Path::new("."));
        let result = match install_page.as_ref() {
            Some(InstallPage::Ros) => backend.task(root, manifest, Operation::Apply),
            Some(InstallPage::Conda { .. }) => {
                backend.task(root, manifest, Operation::CondaInstall)
            }
            Some(InstallPage::Components) => {
                backend.task(root, manifest, Operation::ComponentsInstall)
            }
            Some(InstallPage::DebugToolMissing { tool, .. }) => match dashboard.as_ref() {
                Some(project) => backend.install_debug(&project.manifest, tool),
                None => Err(anyhow::anyhow!("{}", text.debug_no_project)),
            },
            _ => unreachable!(),
        };
        if show_task_result(result, install_page, focused_row, scroll, text) {
            if let Some(project) = dashboard {
                project.installing = true;
            }
        }
        return Ok(());
    }
    leave_screen(terminal, screen)?;
    disable_raw_mode()?;
    let result = match install_page.as_ref() {
        Some(InstallPage::Ros) => install_ros(),
        Some(InstallPage::Conda {
            tool: ToolInstallation::Needed(bootstrap),
            ..
        }) => bootstrap.perform(&mut io::stdout()).map_err(Into::into),
        Some(InstallPage::Components) => install_components(),
        Some(InstallPage::DebugToolMissing { .. }) => {
            Err(anyhow::anyhow!("{}", text.debug_no_project))
        }
        _ => unreachable!(),
    };
    if let Err(error) = result {
        eprintln!("{error:#}");
    }
    println!("Enter ↩");
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    enable_raw_mode()?;
    if let Some(page) = install_page {
        page.refresh();
    }
    *dashboard = dashboard::Dashboard::load(manifest, text)?;
    *scroll = 0;
    *last_runtime_refresh = Instant::now();
    Ok(())
}

fn show_task_result(
    result: Result<TaskOutcome>,
    install_page: &mut Option<InstallPage>,
    focused_row: &mut Option<usize>,
    scroll: &mut usize,
    text: &Text,
) -> bool {
    let started = match result {
        Ok(TaskOutcome::Detached { id }) => {
            let page = runs::Page::task(id);
            *focused_row = page.rows(text).iter().position(|row| row.selectable);
            *install_page = Some(InstallPage::Run(page));
            true
        }
        Ok(TaskOutcome::Completed) => false,
        Err(error) => {
            *install_page = Some(InstallPage::Detail {
                title: text.run_processes.clone(),
                body: format!("{error:#}"),
                return_to_ros: false,
            });
            *focused_row = None;
            false
        }
    };
    *scroll = 0;
    started
}

pub(super) fn request_action(confirmation: &mut Option<PendingConfirmation>, operation: Action) {
    *confirmation = Some(PendingConfirmation::Action(operation));
}

pub(super) fn request_compile(confirmation: &mut Option<PendingConfirmation>, rebuild: bool) {
    *confirmation = Some(PendingConfirmation::Build { rebuild });
}

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_pending_confirmation<B: ratatui::backend::Backend + Write>(
    confirmation: &mut Option<PendingConfirmation>,
    terminal: &mut Terminal<B>,
    screen: &Cell<Screen>,
    manifest: Option<&Path>,
    text: &Text,
    dashboard: &mut Option<dashboard::Dashboard>,
    project_lock: &mut Option<WorkspaceLock>,
    install_page: &mut Option<InstallPage>,
    focused_row: &mut Option<usize>,
    scroll: &mut usize,
    selected: usize,
    last_runtime_refresh: &mut Instant,
    build: &mut impl FnMut(bool) -> Result<()>,
    action: &mut impl FnMut(Action) -> Result<()>,
    backend: Option<&mut dyn RunBackend>,
) -> Result<()> {
    let Some(pending) = confirmation.take() else {
        return Ok(());
    };
    let operation = match &pending {
        PendingConfirmation::Action(action) => task_operation(*action),
        PendingConfirmation::Build { rebuild: true } => Some(Operation::Rebuild),
        PendingConfirmation::Build { rebuild: false } => Some(Operation::Build),
        _ => None,
    };
    if let (Some(backend), Some(operation)) = (backend, operation) {
        let root = dashboard.as_ref().map_or_else(
            || {
                manifest
                    .and_then(Path::parent)
                    .unwrap_or_else(|| Path::new("."))
            },
            |project| project.root.as_path(),
        );
        if show_task_result(
            backend.task(root, manifest, operation),
            install_page,
            focused_row,
            scroll,
            text,
        ) {
            if let Some(project) = dashboard {
                project.installing = true;
            }
        }
        return Ok(());
    }
    match pending {
        PendingConfirmation::Action(operation) => {
            execute_action(terminal, screen, action, operation)?;
        }
        PendingConfirmation::RecoverProject { path, conda } => {
            let root = path.parent().unwrap_or_else(|| Path::new("."));
            let recovered = lock_project(root, text).and_then(|lease| {
                dashboard::recover_manifest(&path, &text.project_recovery_busy)
                    .map(|manifest| (manifest, lease))
            });
            match recovered {
                Ok((manifest, lease)) => {
                    if let Some(name) =
                        conda.filter(|name| dashboard::conda_project_name(&manifest) == Some(name))
                    {
                        *confirmation = Some(PendingConfirmation::AddProjectConda {
                            path: path.clone(),
                            name,
                        });
                    }
                    *dashboard = Some(dashboard::Dashboard::from_manifest(path, manifest, text));
                    *project_lock = Some(lease);
                    *install_page = None;
                }
                Err(error) => {
                    *install_page = Some(InstallPage::Detail {
                        title: text.project_scan_title.clone(),
                        body: format!("{error:#}"),
                        return_to_ros: false,
                    });
                }
            }
            *scroll = 0;
            *focused_row =
                first_selectable(dashboard.as_ref(), text, selected, install_page.as_ref());
            *last_runtime_refresh = Instant::now();
            return Ok(());
        }
        PendingConfirmation::AddProjectConda { path, name } => {
            match dashboard::add_conda(&path, &name, &text.project_recovery_busy) {
                Ok(manifest) => {
                    *dashboard = Some(dashboard::Dashboard::from_manifest(path, manifest, text));
                }
                Err(error) => {
                    *install_page = Some(InstallPage::Detail {
                        title: text.conda.clone(),
                        body: format!("{error:#}"),
                        return_to_ros: false,
                    });
                }
            }
            *scroll = 0;
            *focused_row =
                first_selectable(dashboard.as_ref(), text, selected, install_page.as_ref());
            *last_runtime_refresh = Instant::now();
            return Ok(());
        }
        PendingConfirmation::Build { rebuild } => {
            execute_compile(terminal, screen, build, rebuild)?;
        }
        PendingConfirmation::InstallDebugTool { package } => {
            if let Some(InstallPage::DebugToolMissing {
                package: current,
                install,
                ..
            }) = install_page
            {
                *install = *current == package;
            }
            return Ok(());
        }
    }
    *dashboard = dashboard::Dashboard::load(manifest, text)?;
    *scroll = 0;
    *focused_row = first_selectable(dashboard.as_ref(), text, selected, None);
    *last_runtime_refresh = Instant::now();
    Ok(())
}

fn execute_action<B: ratatui::backend::Backend + Write>(
    terminal: &mut Terminal<B>,
    screen: &Cell<Screen>,
    action: &mut impl FnMut(Action) -> Result<()>,
    operation: Action,
) -> Result<()> {
    leave_screen(terminal, screen)?;
    disable_raw_mode()?;
    let mut answer = String::new();
    if let Err(error) = action(operation) {
        eprintln!("{error:#}");
    }
    println!("Enter ↩");
    io::stdin().read_line(&mut answer)?;
    enable_raw_mode()?;
    Ok(())
}

fn execute_compile<B: ratatui::backend::Backend + Write>(
    terminal: &mut Terminal<B>,
    screen: &Cell<Screen>,
    build: &mut impl FnMut(bool) -> Result<()>,
    rebuild: bool,
) -> Result<()> {
    leave_screen(terminal, screen)?;
    disable_raw_mode()?;
    let mut answer = String::new();
    if let Err(error) = build(rebuild) {
        eprintln!("{error:#}");
    }
    println!("Enter ↩");
    io::stdin().read_line(&mut answer)?;
    enable_raw_mode()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::CrosstermBackend, layout::Rect, TerminalOptions, Viewport};
    use rospm_core::run::{RunRequest, RunSignal, RunStatus};

    #[derive(Default)]
    struct TaskBackend {
        operations: Vec<Operation>,
        fail: bool,
    }

    impl RunBackend for TaskBackend {
        fn task(
            &mut self,
            _: &Path,
            _: Option<&Path>,
            operation: Operation,
        ) -> Result<TaskOutcome> {
            self.operations.push(operation);
            if self.fail {
                anyhow::bail!("submission failed");
            }
            Ok(TaskOutcome::Detached {
                id: "new-task".into(),
            })
        }

        fn start(&mut self, _: &Path, _: RunRequest, _: bool) -> Result<()> {
            unreachable!()
        }
        fn start_debug(&mut self, _: &Path, _: &Path, _: &str) -> Result<crate::DebugStart> {
            unreachable!()
        }
        fn debug_tool_statuses(
            &mut self,
            _: &Path,
        ) -> Result<[rospm_core::probe::Status; crate::DEBUG_TOOLS.len()]> {
            unreachable!()
        }
        fn install_debug(&mut self, _: &Path, _: &str) -> Result<TaskOutcome> {
            unreachable!()
        }
        fn foreground(&mut self) -> Vec<RunStatus> {
            unreachable!()
        }
        fn background(&mut self, _: &Path) -> Result<Vec<RunStatus>> {
            unreachable!()
        }
        fn history(&mut self) -> Vec<RunStatus> {
            unreachable!()
        }
        fn signal(&mut self, _: &Path, _: &RunStatus, _: RunSignal) -> Result<()> {
            unreachable!()
        }
        fn close_foreground(&mut self) {
            unreachable!()
        }
        fn shell(&mut self, _: &Path, _: &Path) -> Result<()> {
            unreachable!()
        }
    }

    fn assert_task_page(page: &Option<InstallPage>, focused: Option<usize>, scroll: usize) {
        let Some(InstallPage::Run(page)) = page else {
            panic!("task page missing")
        };
        assert_eq!(page.focused_process(focused).unwrap().id, "new-task");
        assert_eq!(page.rows(&Text::default())[0].cells[0], "task/new-task");
        assert_eq!(focused, Some(1));
        assert_eq!(scroll, 0);
    }

    #[test]
    fn confirmed_worker_actions_open_the_task_without_leaving_the_screen() {
        let mut bytes = Vec::new();
        let mut terminal = Terminal::with_options(
            CrosstermBackend::new(&mut bytes),
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, 80, 24)),
            },
        )
        .unwrap();
        let screen = Cell::new(Screen::Active);
        let mut backend = TaskBackend::default();
        for (pending, operation) in [
            (PendingConfirmation::Action(Action::Apply), Operation::Apply),
            (
                PendingConfirmation::Action(Action::Uninstall),
                Operation::Uninstall,
            ),
            (PendingConfirmation::Action(Action::Check), Operation::Check),
            (
                PendingConfirmation::Action(Action::Verify),
                Operation::Verify,
            ),
            (
                PendingConfirmation::Action(Action::DepsCheck),
                Operation::DepsCheck,
            ),
            (
                PendingConfirmation::Action(Action::DepsInstall),
                Operation::DepsInstall,
            ),
            (
                PendingConfirmation::Build { rebuild: false },
                Operation::Build,
            ),
            (
                PendingConfirmation::Build { rebuild: true },
                Operation::Rebuild,
            ),
        ] {
            let mut confirmation = Some(pending);
            let mut page = None;
            let mut focused = Some(7);
            let mut scroll = 10;
            execute_pending_confirmation(
                &mut confirmation,
                &mut terminal,
                &screen,
                None,
                &Text::default(),
                &mut None,
                &mut None,
                &mut page,
                &mut focused,
                &mut scroll,
                2,
                &mut Instant::now(),
                &mut |_| panic!("synchronous build"),
                &mut |_| panic!("synchronous action"),
                Some(&mut backend),
            )
            .unwrap();
            assert!(confirmation.is_none());
            assert!(screen.get() == Screen::Active);
            assert_eq!(backend.operations.last(), Some(&operation));
            assert_task_page(&page, focused, scroll);
        }
        drop(terminal);
        assert!(bytes.is_empty());
    }

    #[test]
    fn installer_submission_and_errors_stay_in_the_page() {
        let mut bytes = Vec::new();
        let mut terminal = Terminal::with_options(
            CrosstermBackend::new(&mut bytes),
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, 80, 24)),
            },
        )
        .unwrap();
        let screen = Cell::new(Screen::Active);
        let mut backend = TaskBackend::default();
        for (page, operation, fail) in [
            (InstallPage::Ros, Operation::Apply, false),
            (InstallPage::Components, Operation::ComponentsInstall, false),
            (InstallPage::Ros, Operation::Apply, true),
        ] {
            backend.fail = fail;
            let mut page = Some(page);
            let mut focused = Some(7);
            let mut scroll = 10;
            install(
                &mut terminal,
                &screen,
                None,
                &mut None,
                &mut page,
                &mut focused,
                &mut scroll,
                &Text::default(),
                &mut || panic!("synchronous install"),
                &mut || panic!("synchronous install"),
                Some(&mut backend),
                &mut Instant::now(),
            )
            .unwrap();
            assert!(screen.get() == Screen::Active);
            assert_eq!(backend.operations.last(), Some(&operation));
            if fail {
                assert!(
                    matches!(page, Some(InstallPage::Detail { body, .. }) if body == "submission failed")
                );
                assert_eq!(focused, None);
                assert_eq!(scroll, 0);
            } else {
                assert_task_page(&page, focused, scroll);
            }
        }
        drop(terminal);
        assert!(bytes.is_empty());
    }

    #[test]
    fn install_actions_use_task_operations_except_project_creation() {
        assert_eq!(task_operation(Action::Apply), Some(Operation::Apply));
        assert_eq!(
            task_operation(Action::DepsCheck),
            Some(Operation::DepsCheck)
        );
        assert_eq!(task_operation(Action::CreateProject), None);
    }
}
