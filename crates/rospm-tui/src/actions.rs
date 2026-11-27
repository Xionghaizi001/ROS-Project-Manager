use std::{
    io::{self, Write},
    path::Path,
    time::Instant,
};

use anyhow::Result;
use crossterm::{
    event::EnableMouseCapture,
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen},
};
use ratatui::Terminal;
use rospm_core::{conda::ToolInstallation, worker::Operation};

use crate::{
    dashboard, navigation::first_selectable, terminal::leave_screen, InstallPage, RunBackend,
    TaskOutcome, Text,
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
    Build { rebuild: bool },
    InstallDebugTool { package: String },
}

impl PendingConfirmation {
    pub(super) fn prompt(&self, text: &Text) -> String {
        match self {
            Self::Action(action) => {
                format!("{} {}", text.confirm_action, action_label(*action, text))
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
    manifest: Option<&Path>,
    dashboard: &mut Option<dashboard::Dashboard>,
    install_page: &mut Option<InstallPage>,
    scroll: &mut usize,
    text: &Text,
    install_ros: &mut impl FnMut() -> Result<()>,
    install_components: &mut impl FnMut() -> Result<()>,
    backend: Option<&mut dyn RunBackend>,
    last_runtime_refresh: &mut Instant,
) -> Result<()> {
    leave_screen(terminal)?;
    disable_raw_mode()?;
    let root = dashboard
        .as_ref()
        .map(|project| project.root.as_path())
        .unwrap_or_else(|| Path::new("."));
    let result = match install_page.as_ref() {
        Some(InstallPage::Ros) => match backend {
            Some(backend) => backend.task(root, manifest, Operation::Apply),
            None => install_ros().map(|_| TaskOutcome::Completed),
        },
        Some(InstallPage::Conda(ToolInstallation::Needed(bootstrap))) => match backend {
            Some(backend) => backend.task(root, manifest, Operation::CondaInstall),
            None => bootstrap
                .perform(&mut io::stdout())
                .map(|_| TaskOutcome::Completed)
                .map_err(Into::into),
        },
        Some(InstallPage::Components) => match backend {
            Some(backend) => backend.task(root, manifest, Operation::ComponentsInstall),
            None => install_components().map(|_| TaskOutcome::Completed),
        },
        Some(InstallPage::DebugToolMissing { tool, .. }) => {
            let tool = *tool;
            let result = match (backend, dashboard.as_ref()) {
                (Some(backend), Some(project)) => backend.install_debug(&project.manifest, tool),
                _ => Err(anyhow::anyhow!("{}", text.debug_no_project)),
            };
            if result.is_ok() {
                *install_page = Some(InstallPage::DebugTool(tool));
            }
            result.map(|_| TaskOutcome::Completed)
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
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture
    )?;
    terminal.hide_cursor()?;
    terminal.clear()?;
    if let Some(page) = install_page {
        page.refresh();
    }
    *dashboard = dashboard::Dashboard::load(manifest, text)?;
    *scroll = 0;
    *last_runtime_refresh = Instant::now();
    Ok(())
}

pub(super) fn request_action(confirmation: &mut Option<PendingConfirmation>, operation: Action) {
    *confirmation = Some(PendingConfirmation::Action(operation));
}

pub(super) fn request_compile(confirmation: &mut Option<PendingConfirmation>, rebuild: bool) {
    *confirmation = Some(PendingConfirmation::Build { rebuild });
}

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_pending_confirmation<B: ratatui::backend::Backend + Write>(
    pending: PendingConfirmation,
    terminal: &mut Terminal<B>,
    manifest: Option<&Path>,
    text: &Text,
    dashboard: &mut Option<dashboard::Dashboard>,
    install_page: &mut Option<InstallPage>,
    focused_row: &mut Option<usize>,
    scroll: &mut usize,
    selected: usize,
    last_runtime_refresh: &mut Instant,
    build: &mut impl FnMut(bool) -> Result<()>,
    action: &mut impl FnMut(Action) -> Result<()>,
    backend: Option<&mut dyn RunBackend>,
) -> Result<()> {
    match pending {
        PendingConfirmation::Action(operation) => {
            execute_action(terminal, manifest, action, operation, backend)?;
        }
        PendingConfirmation::Build { rebuild } => {
            execute_compile(terminal, dashboard.as_ref(), build, rebuild, backend)?;
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
    manifest: Option<&Path>,
    action: &mut impl FnMut(Action) -> Result<()>,
    operation: Action,
    backend: Option<&mut dyn RunBackend>,
) -> Result<()> {
    leave_screen(terminal)?;
    disable_raw_mode()?;
    let mut answer = String::new();
    let mut detached = false;
    let result = if let (Some(backend), Some(operation)) = (backend, task_operation(operation)) {
        let root = manifest
            .and_then(Path::parent)
            .unwrap_or_else(|| Path::new("."));
        backend
            .task(root, manifest, operation)
            .map(|outcome| matches!(outcome, TaskOutcome::Detached))
    } else {
        action(operation).map(|_| false)
    };
    match result {
        Ok(true) => detached = true,
        Ok(false) => {
            println!("Enter ↩");
            io::stdin().read_line(&mut answer)?;
        }
        Err(error) => {
            eprintln!("{error:#}");
            println!("Enter ↩");
            io::stdin().read_line(&mut answer)?;
        }
    }
    enable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture
    )?;
    terminal.hide_cursor()?;
    if detached {
        // EnterAlternateScreen already blanked the screen. Reset both Ratatui buffers.
        terminal.swap_buffers();
        terminal.swap_buffers();
    } else {
        terminal.clear()?;
    }
    Ok(())
}

fn execute_compile<B: ratatui::backend::Backend + Write>(
    terminal: &mut Terminal<B>,
    dashboard: Option<&dashboard::Dashboard>,
    build: &mut impl FnMut(bool) -> Result<()>,
    rebuild: bool,
    backend: Option<&mut dyn RunBackend>,
) -> Result<()> {
    leave_screen(terminal)?;
    disable_raw_mode()?;
    let mut answer = String::new();
    let result = if let Some(backend) = backend {
        let root = dashboard.map_or(Path::new("."), |project| project.root.as_path());
        backend
            .task(
                root,
                dashboard.map(|project| project.manifest.as_path()),
                if rebuild {
                    Operation::Rebuild
                } else {
                    Operation::Build
                },
            )
            .map(|_| ())
    } else {
        build(rebuild)
    };
    if let Err(error) = result {
        eprintln!("{error:#}");
    }
    println!("Enter ↩");
    io::stdin().read_line(&mut answer)?;
    enable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture
    )?;
    terminal.hide_cursor()?;
    terminal.clear()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{task_operation, Action};
    use rospm_core::worker::Operation;

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
