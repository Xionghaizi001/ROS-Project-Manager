use std::{cell::Cell, io::Write, path::Path, time::Instant};

use anyhow::Result;
use ratatui::Terminal;
use rospm_core::worker::WorkspaceLock;

use crate::{
    actions::{
        execute_pending_confirmation, install, request_action, request_compile, PendingConfirmation,
    },
    dashboard, logs,
    navigation::{activate, back_run, first_selectable, move_focus, open_project, refresh_project},
    terminal::Screen,
    view, Action, InstallPage, RunBackend, Text,
};

use super::open_logs;

#[allow(clippy::too_many_arguments)]
pub(super) fn activate_footer<B: ratatui::backend::Backend + Write>(
    action: view::FooterAction,
    terminal: &mut Terminal<B>,
    screen: &Cell<Screen>,
    manifest: Option<&Path>,
    browser: &mut Option<logs::Browser>,
    processes: &[rospm_core::run::RunStatus],
    dashboard: &mut Option<dashboard::Dashboard>,
    project_lock: &mut Option<WorkspaceLock>,
    install_page: &mut Option<InstallPage>,
    focused_row: &mut Option<usize>,
    scroll: &mut usize,
    selected: &mut usize,
    text: &Text,
    install_ros: &mut impl FnMut() -> Result<()>,
    install_components: &mut impl FnMut() -> Result<()>,
    last_runtime_refresh: &mut Instant,
    build: &mut impl FnMut(bool) -> Result<()>,
    execute_action: &mut impl FnMut(Action) -> Result<()>,
    backend: Option<&mut dyn RunBackend>,
    confirmation: &mut Option<PendingConfirmation>,
) -> Result<bool> {
    match action {
        view::FooterAction::Logs => {
            *browser = Some(open_logs(
                dashboard.as_ref(),
                install_page.as_ref(),
                *focused_row,
                processes,
            ));
        }
        view::FooterAction::Select => {
            if !move_focus(
                focused_row,
                scroll,
                dashboard.as_ref(),
                text,
                *selected,
                install_page.as_ref(),
                terminal.size()?,
                true,
            ) {
                *scroll = scroll.saturating_add(1);
            }
        }
        view::FooterAction::Open => {
            if !open_project(
                text,
                dashboard,
                project_lock,
                install_page,
                focused_row,
                scroll,
                confirmation,
            ) {
                activate(
                    install_page,
                    focused_row,
                    scroll,
                    dashboard.as_ref(),
                    text,
                    *selected,
                    confirmation,
                );
            }
        }
        view::FooterAction::ChangeTab => {
            *selected = (*selected + 1) % 8;
            *install_page = None;
            *scroll = 0;
            *focused_row = first_selectable(dashboard.as_ref(), text, *selected, None);
        }
        view::FooterAction::ScrollUp => {
            let page_size = usize::from(terminal.size()?.height.saturating_sub(6).clamp(1, 10));
            *scroll = scroll.saturating_sub(page_size);
        }
        view::FooterAction::ScrollDown => {
            let page_size = usize::from(terminal.size()?.height.saturating_sub(6).clamp(1, 10));
            *scroll = scroll.saturating_add(page_size);
        }
        view::FooterAction::Refresh | view::FooterAction::RescanProjects => {
            refresh_project(manifest, text, dashboard, install_page, *selected == 0)?;
            *scroll = 0;
            *focused_row =
                first_selectable(dashboard.as_ref(), text, *selected, install_page.as_ref());
            *last_runtime_refresh = Instant::now();
        }
        view::FooterAction::Install => install(
            terminal,
            screen,
            manifest,
            dashboard,
            install_page,
            scroll,
            text,
            install_ros,
            install_components,
            backend,
            last_runtime_refresh,
        )?,
        view::FooterAction::Confirm(approved) => {
            if let Some(pending) = confirmation.take() {
                if approved {
                    execute_pending_confirmation(
                        pending,
                        terminal,
                        screen,
                        manifest,
                        text,
                        dashboard,
                        project_lock,
                        install_page,
                        focused_row,
                        scroll,
                        *selected,
                        last_runtime_refresh,
                        build,
                        execute_action,
                        backend,
                    )?;
                }
            }
        }
        view::FooterAction::Build | view::FooterAction::Rebuild => {
            request_compile(confirmation, action == view::FooterAction::Rebuild);
        }
        view::FooterAction::Execute(operation) => {
            request_action(confirmation, operation);
        }
        view::FooterAction::CreateProject => {
            request_action(confirmation, Action::CreateProject);
        }
        view::FooterAction::ScanProjects => {
            *install_page = Some(InstallPage::scan_projects(manifest)?);
            *focused_row =
                first_selectable(dashboard.as_ref(), text, *selected, install_page.as_ref());
            *scroll = 0;
        }
        view::FooterAction::Back => {
            if back_run(install_page, focused_row, scroll, text) {
                return Ok(false);
            }
            *install_page = None;
            *scroll = 0;
            *focused_row = first_selectable(dashboard.as_ref(), text, *selected, None);
        }
        view::FooterAction::Quit => return Ok(true),
    }
    Ok(false)
}
