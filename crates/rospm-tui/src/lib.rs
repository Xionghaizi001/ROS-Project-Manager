//! Terminal setup and event loop for the TUI.

mod dashboard;
mod log_view;
mod logs;
mod packages;
mod pages;
mod runs;
mod table;
mod view;

use std::{
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseButton,
        MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use rospm_core::conda::ToolInstallation;
use rospm_core::worker::{Operation, WorkspaceLock};
pub use runs::RunBackend;
pub use runs::TaskOutcome;
use std::time::{Duration, Instant};

const PROCESS_KEY: char = 'p';
const LOG_KEY: char = 'l';
const DEBUG_TAB: usize = 4;
const HELP_TAB: usize = 6;

pub const DEBUG_TOOLS: [&str; 8] = [
    "rqt",
    "rviz2",
    "rqt_graph",
    "rqt_topic",
    "rqt_publisher",
    "rqt_service_caller",
    "rqt_reconfigure",
    "rqt_console",
];

/// Localized strings supplied by the CLI layer.
#[derive(Default)]
pub struct Text {
    pub no_logs: String,
    pub log_error: String,
    pub log_help: String,
    pub build_order: String,
    pub running: String,
    pub not_installed: String,
    pub manifest_label: String,
    pub provider_label: String,
    pub root_label: String,
    pub refresh: String,
    pub missing: String,
    pub item: String,
    pub actual: String,
    pub expected: String,
    pub components: String,

    pub tabs: [String; 8],
    pub title: String,
    pub project: String,
    pub overview: String,
    pub environment: String,
    pub workspace: String,
    pub status: String,
    pub ready: String,
    pub installing: String,
    pub details: String,
    pub manifest_search: String,
    pub tui_usage: String,
    pub cli_help: String,
    pub cli_usage: String,
    pub footer_select: String,
    pub footer_open: String,
    pub footer_tabs: String,
    pub footer_scroll_up: String,
    pub footer_scroll_down: String,
    pub footer_refresh: String,
    pub footer_install: String,
    pub footer_back: String,
    pub footer_quit: String,
    pub footer_build: String,
    pub footer_rebuild: String,
    pub confirm_build: String,
    pub confirm_rebuild: String,
    pub confirm_action: String,
    pub footer_apply: String,
    pub footer_uninstall: String,
    pub footer_check: String,
    pub footer_verify: String,
    pub footer_deps_check: String,
    pub footer_deps_install: String,
    pub footer_create_project: String,
    pub footer_scan_projects: String,
    pub footer_rescan_projects: String,
    pub project_scan_title: String,
    pub no_projects_found: String,
    pub non_terminal: String,
    pub actions: String,
    pub check: String,
    pub verify: String,
    pub deps: String,
    pub apply_plan: String,
    pub uninstall_plan: String,
    pub plan: String,
    pub build_args: String,
    pub conda: String,
    pub check_result: String,
    pub verify_result: String,
    pub deps_result: String,
    pub task_status: String,
    pub task_attach: String,
    pub task_cancel: String,
    pub task_pending: String,
    pub task_waiting_confirmation: String,
    pub task_waiting_authorization: String,
    pub task_cancelling: String,
    pub task_succeeded: String,
    pub task_failed: String,
    pub task_cancelled: String,
    pub task_interrupted: String,
    pub task_offline: String,
    pub task_step: String,
    pub logs: String,
    pub diagnostics: String,
    pub status_ok: String,
    pub status_missing: String,
    pub status_mismatch: String,
    pub status_unknown: String,
    pub status_info: String,
    pub run_not_built: String,
    pub run_empty: String,
    pub run_node: String,
    pub run_launch: String,
    pub debug_tools: [String; DEBUG_TOOLS.len()],
    pub debug_no_project: String,
    pub run_processes: String,
    pub run_foreground: String,
    pub run_background: String,
    pub run_args: String,
    pub run_bad_args: String,
    pub run_venv: String,
    pub run_venv_help: String,
    pub run_terminate: String,
    pub run_kill: String,
    pub run_suspend: String,
    pub run_resume: String,
    pub run_suspended: String,
    pub shell_detached: String,
    pub run_starting: String,
    pub run_idle: String,
    pub run_current: String,
    pub run_history: String,
    pub run_no_history: String,
    pub run_exited: String,
    pub run_exit_code: String,
    pub run_exit_signal: String,
    pub run_exit_unknown: String,
    pub run_gone: String,
    pub tui_already_running: String,
}

enum InstallPage {
    Ros,
    Conda(ToolInstallation),
    Components,
    Detail {
        title: String,
        body: String,
        return_to_ros: bool,
    },
    ScannedProjects(Vec<std::path::PathBuf>),
    Run(runs::Page),
    DebugTool(&'static str),
    Shell,
    CliHelp,
}

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

fn task_backend<'a>(
    runs: &'a mut Option<(&mut dyn RunBackend, u8)>,
) -> Option<&'a mut dyn RunBackend> {
    match runs {
        Some((backend, _)) if backend.has_task_worker() => Some(&mut **backend),
        None => None,
        Some(_) => None,
    }
}

impl InstallPage {
    fn can_install(&self) -> bool {
        matches!(
            self,
            Self::Ros | Self::Components | Self::Conda(ToolInstallation::Needed(_))
        )
    }

    fn refresh(&mut self) {
        if let Self::Conda(tool) = self {
            *tool = ToolInstallation::probe();
        } else if let Self::ScannedProjects(projects) = self {
            if let Ok(root) = std::env::current_dir() {
                *projects = dashboard::Dashboard::scan_projects(&root);
            }
        }
    }
}

pub fn run(
    manifest_path: Option<&Path>,
    text: Text,
    install_ros: impl FnMut() -> Result<()>,
) -> Result<()> {
    run_with_installers(manifest_path, text, install_ros, || {
        Err(anyhow!("project component installer unavailable"))
    })
}

pub fn run_with_installers(
    manifest_path: Option<&Path>,
    text: Text,
    install_ros: impl FnMut() -> Result<()>,
    install_components: impl FnMut() -> Result<()>,
) -> Result<()> {
    run_with_build(manifest_path, text, install_ros, install_components, |_| {
        Err(anyhow!("project builder unavailable"))
    })
}

pub fn run_with_build(
    manifest_path: Option<&Path>,
    text: Text,
    install_ros: impl FnMut() -> Result<()>,
    install_components: impl FnMut() -> Result<()>,
    build: impl FnMut(bool) -> Result<()>,
) -> Result<()> {
    run_with_actions(
        manifest_path,
        text,
        install_ros,
        install_components,
        build,
        |_| Err(anyhow!("project action unavailable")),
    )
}

pub fn run_with_actions(
    manifest_path: Option<&Path>,
    text: Text,
    install_ros: impl FnMut() -> Result<()>,
    install_components: impl FnMut() -> Result<()>,
    build: impl FnMut(bool) -> Result<()>,
    action: impl FnMut(Action) -> Result<()>,
) -> Result<()> {
    run_dashboard(
        manifest_path,
        text,
        install_ros,
        install_components,
        build,
        action,
        None,
    )
}

#[allow(clippy::too_many_arguments)] // Preserve the public callback-based entry point.
pub fn run_with_runs(
    manifest_path: Option<&Path>,
    text: Text,
    install_ros: impl FnMut() -> Result<()>,
    install_components: impl FnMut() -> Result<()>,
    build: impl FnMut(bool) -> Result<()>,
    action: impl FnMut(Action) -> Result<()>,
    runs: &mut dyn RunBackend,
    verbosity: u8,
) -> Result<()> {
    run_dashboard(
        manifest_path,
        text,
        install_ros,
        install_components,
        build,
        action,
        Some((runs, verbosity)),
    )
}

fn run_dashboard(
    manifest_path: Option<&Path>,
    text: Text,
    mut install_ros: impl FnMut() -> Result<()>,
    mut install_components: impl FnMut() -> Result<()>,
    mut build: impl FnMut(bool) -> Result<()>,
    mut action: impl FnMut(Action) -> Result<()>,
    mut runs: Option<(&mut dyn RunBackend, u8)>,
) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(anyhow!(text.non_terminal));
    }
    let dashboard = dashboard::Dashboard::load(manifest_path, &text)?;
    let mut project_lock = dashboard
        .as_ref()
        .map(|project| lock_project(project, &text))
        .transpose()?;
    enable_raw_mode().context("failed to enable terminal raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .context("failed to enter alternate screen")?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(stdout)).context("failed to initialise terminal UI")?;
    let result = loop_ui(
        &mut terminal,
        manifest_path,
        dashboard,
        &text,
        &mut install_ros,
        &mut install_components,
        &mut build,
        &mut action,
        &mut runs,
        &mut project_lock,
    );
    if let Some((backend, _)) = runs {
        backend.close_foreground();
    }
    leave_screen(&mut terminal).ok();
    disable_raw_mode().ok();
    result
}

#[allow(clippy::too_many_arguments)] // Callbacks and the workspace lock remain owned by the terminal session.
fn loop_ui<B: ratatui::backend::Backend + Write>(
    terminal: &mut Terminal<B>,
    manifest: Option<&Path>,
    mut dashboard: Option<dashboard::Dashboard>,
    text: &Text,
    install_ros: &mut impl FnMut() -> Result<()>,
    install_components: &mut impl FnMut() -> Result<()>,
    build: &mut impl FnMut(bool) -> Result<()>,
    action: &mut impl FnMut(Action) -> Result<()>,
    runs: &mut Option<(&mut dyn RunBackend, u8)>,
    project_lock: &mut Option<WorkspaceLock>,
) -> Result<()> {
    let mut active_manifest = manifest.map(Path::to_path_buf);
    let mut browser: Option<logs::Browser> = None;
    let mut selected = 0usize;
    let mut install_page: Option<InstallPage> = None;
    let mut scroll = 0usize;
    let mut focused_row = first_selectable(dashboard.as_ref(), text, selected, None);
    let mut last_runtime_refresh = Instant::now();
    let mut last_process_refresh = None::<Instant>;
    let mut processes = Vec::new();
    let mut active_processes = 0;
    let mut process_error = None;
    let mut left_press = None;
    loop {
        if let Some(project) = dashboard.as_mut() {
            if project_lock
                .as_ref()
                .is_none_or(|lease| lease.workspace() != project.root)
            {
                *project_lock = Some(lock_project(project, text)?);
            }
            if let Some((backend, verbosity)) = runs {
                if let Some(InstallPage::DebugTool(tool)) = install_page {
                    match backend.start_debug(&project.root, &project.manifest, tool) {
                        Ok(process) => {
                            install_page =
                                Some(InstallPage::Run(runs::Page::processes(vec![process], None)));
                            focused_row = Some(1);
                        }
                        Err(error) => {
                            install_page = Some(InstallPage::Detail {
                                title: tool.into(),
                                body: format!("{error:#}"),
                                return_to_ros: false,
                            });
                            focused_row = None;
                        }
                    }
                    scroll = 0;
                    last_process_refresh = None;
                }
                if matches!(install_page, Some(InstallPage::Shell)) {
                    leave_screen(terminal)?;
                    let result = backend.shell(&project.root, &project.manifest);
                    // The main screen may still be at the shell prompt, with raw output enabled.
                    terminal.backend_mut().write_all(b"\r\n")?;
                    execute!(
                        terminal.backend_mut(),
                        EnterAlternateScreen,
                        EnableMouseCapture
                    )?;
                    terminal.hide_cursor()?;
                    terminal.clear()?;
                    install_page = result.err().map(|error| InstallPage::Detail {
                        title: "Shell".into(),
                        body: format!("{error:#}"),
                        return_to_ros: false,
                    });
                    focused_row =
                        first_selectable(Some(project), text, selected, install_page.as_ref());
                    scroll = 0;
                    last_process_refresh = None;
                }
                if let Some(InstallPage::Run(page)) = &mut install_page {
                    if let Some(id) = page.take_task_attach() {
                        leave_screen(terminal)?;
                        disable_raw_mode()?;
                        if let Err(error) = backend.attach_task(&project.root, &id) {
                            eprintln!("{error:#}");
                        }
                        println!("Enter ↩");
                        io::stdin().read_line(&mut String::new())?;
                        enable_raw_mode()?;
                        execute!(
                            terminal.backend_mut(),
                            EnterAlternateScreen,
                            EnableMouseCapture
                        )?;
                        terminal.hide_cursor()?;
                        terminal.clear()?;
                        last_process_refresh = None;
                    }
                    if page.dispatch(*backend, project, *verbosity, text) {
                        install_page = None;
                        focused_row = first_selectable(Some(project), text, selected, None);
                    }
                }
                if last_process_refresh
                    .is_none_or(|last| last.elapsed() >= Duration::from_millis(400))
                {
                    processes = backend.foreground();
                    match backend.background(&project.root) {
                        Ok(background) => {
                            processes.extend(background);
                            process_error = None;
                        }
                        Err(error) => process_error = Some(format!("{error:#}")),
                    }
                    if let Some(id) = backend.pending_authorization().map(str::to_owned) {
                        leave_screen(terminal)?;
                        disable_raw_mode()?;
                        let result = backend.authorize_task(&project.root, &id);
                        enable_raw_mode()?;
                        execute!(
                            terminal.backend_mut(),
                            EnterAlternateScreen,
                            EnableMouseCapture
                        )?;
                        terminal.hide_cursor()?;
                        terminal.clear()?;
                        if let Err(error) = result {
                            process_error = Some(format!("{error:#}"));
                        }
                    }
                    let history = backend.history();
                    let finished: std::collections::HashSet<_> =
                        history.iter().map(|process| &process.id).collect();
                    processes.retain(|process| !finished.contains(&process.id));
                    project.installing = processes
                        .iter()
                        .any(|process| process.task && process.exit.is_none());
                    processes.extend(history);
                    processes.sort_by_key(|process| process.exit.is_some());
                    active_processes = processes
                        .iter()
                        .filter(|process| process.exit.is_none())
                        .count();
                    if let Some(InstallPage::Run(page)) = &mut install_page {
                        page.refresh(&processes, process_error.as_deref(), &mut focused_row, text);
                    }
                    last_process_refresh = Some(Instant::now());
                }
            }
        }
        if last_runtime_refresh.elapsed() >= Duration::from_secs(1) {
            if let Some(browser) = browser.as_mut() {
                browser.reload();
            } else if let Some(dashboard) = dashboard.as_mut() {
                dashboard.refresh_runtime();
            }
            last_runtime_refresh = Instant::now();
        }
        terminal.draw(|frame| {
            if let Some(browser) = &browser {
                log_view::draw(frame, browser, text);
                return;
            }
            view::draw(
                frame,
                dashboard.as_ref(),
                text,
                selected,
                install_page.as_ref(),
                &mut scroll,
                focused_row,
                runs.is_some().then_some(active_processes),
            )
        })?;
        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('c'))
                    {
                        break Ok(());
                    }
                    if let Some(log) = browser.as_mut() {
                        match key.code {
                            KeyCode::Esc | KeyCode::Char('q') => browser = None,
                            KeyCode::Tab => log.select(true),
                            KeyCode::BackTab => log.select(false),
                            KeyCode::Char('n') => log.page(true),
                            KeyCode::Char('p') => log.page(false),
                            KeyCode::Char('r') => log.reload(),
                            KeyCode::Down | KeyCode::Char('j') => {
                                log.scroll = (log.scroll + 1).min(log.lines.len().saturating_sub(1))
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                log.scroll = log.scroll.saturating_sub(1)
                            }
                            KeyCode::PageDown => {
                                log.scroll =
                                    (log.scroll + 10).min(log.lines.len().saturating_sub(1))
                            }
                            KeyCode::PageUp => log.scroll = log.scroll.saturating_sub(10),
                            KeyCode::Home => log.scroll = 0,
                            KeyCode::End => log.scroll = log.lines.len().saturating_sub(10),
                            KeyCode::Right => log.horizontal = log.horizontal.saturating_add(8),
                            KeyCode::Left => log.horizontal = log.horizontal.saturating_sub(8),
                            _ => {}
                        }
                        continue;
                    }
                    if let Some(InstallPage::Run(page)) = &mut install_page {
                        if page.edit_key(key) {
                            continue;
                        }
                    }
                    if key.code == KeyCode::Char(LOG_KEY) && key.modifiers.is_empty() {
                        browser = Some(open_logs(
                            dashboard.as_ref(),
                            install_page.as_ref(),
                            focused_row,
                            &processes,
                        ));
                        continue;
                    }
                    if key.code == KeyCode::Char(PROCESS_KEY) && runs.is_some() {
                        install_page = Some(InstallPage::Run(runs::Page::processes(
                            processes.clone(),
                            process_error.clone(),
                        )));
                        focused_row = first_selectable(
                            dashboard.as_ref(),
                            text,
                            selected,
                            install_page.as_ref(),
                        );
                        scroll = 0;
                        continue;
                    }
                    let size = terminal.size()?;
                    let page_size = usize::from(size.height.saturating_sub(6).clamp(1, 10));
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('q') if install_page.is_some() => {
                            if back_run(&mut install_page, &mut focused_row, &mut scroll, text) {
                                continue;
                            }
                            install_page = match install_page.take() {
                                Some(InstallPage::Detail {
                                    return_to_ros: true,
                                    ..
                                }) => Some(InstallPage::Ros),
                                _ => None,
                            };
                            scroll = 0;
                            focused_row = first_selectable(
                                dashboard.as_ref(),
                                text,
                                selected,
                                install_page.as_ref(),
                            );
                        }
                        KeyCode::Char('i')
                            if install_page.as_ref().is_some_and(InstallPage::can_install) =>
                        {
                            install(
                                terminal,
                                active_manifest.as_deref(),
                                &mut dashboard,
                                &mut install_page,
                                &mut scroll,
                                text,
                                install_ros,
                                install_components,
                                task_backend(runs),
                                &mut last_runtime_refresh,
                            )?;
                        }
                        KeyCode::Enter => {
                            if let Some(InstallPage::ScannedProjects(projects)) = &install_page {
                                if let Some(path) =
                                    focused_row.and_then(|index| projects.get(index)).cloned()
                                {
                                    open_project(
                                        path,
                                        text,
                                        &mut active_manifest,
                                        &mut dashboard,
                                        project_lock,
                                        &mut install_page,
                                        &mut focused_row,
                                        &mut scroll,
                                    )?;
                                }
                            } else {
                                activate(
                                    &mut install_page,
                                    &mut focused_row,
                                    &mut scroll,
                                    dashboard.as_ref(),
                                    text,
                                    selected,
                                );
                            }
                        }
                        KeyCode::Right if install_page.is_some() => {
                            let rows = pages::rows(
                                dashboard.as_ref(),
                                text,
                                selected,
                                install_page.as_ref(),
                            );
                            if focused_row
                                .and_then(|index| rows.get(index))
                                .is_some_and(|row| row.detail.is_some())
                            {
                                activate(
                                    &mut install_page,
                                    &mut focused_row,
                                    &mut scroll,
                                    dashboard.as_ref(),
                                    text,
                                    selected,
                                );
                            }
                        }
                        KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                        KeyCode::Left | KeyCode::BackTab if install_page.is_none() => {
                            selected = (selected + 7) % 8;
                            scroll = 0;
                            focused_row =
                                first_selectable(dashboard.as_ref(), text, selected, None);
                        }
                        KeyCode::Right | KeyCode::Tab if install_page.is_none() => {
                            let rows = pages::rows(dashboard.as_ref(), text, selected, None);
                            if focused_row
                                .and_then(|index| rows.get(index))
                                .is_some_and(|row| row.detail.is_some())
                            {
                                activate(
                                    &mut install_page,
                                    &mut focused_row,
                                    &mut scroll,
                                    dashboard.as_ref(),
                                    text,
                                    selected,
                                );
                                continue;
                            }
                            selected = (selected + 1) % 8;
                            scroll = 0;
                            focused_row =
                                first_selectable(dashboard.as_ref(), text, selected, None);
                        }
                        KeyCode::PageDown => scroll = scroll.saturating_add(page_size),
                        KeyCode::PageUp => scroll = scroll.saturating_sub(page_size),
                        KeyCode::Home => scroll = 0,
                        KeyCode::Down | KeyCode::Char('j') => {
                            if !move_focus(
                                &mut focused_row,
                                &mut scroll,
                                dashboard.as_ref(),
                                text,
                                selected,
                                install_page.as_ref(),
                                size,
                                true,
                            ) {
                                scroll = scroll.saturating_add(1);
                            }
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            if !move_focus(
                                &mut focused_row,
                                &mut scroll,
                                dashboard.as_ref(),
                                text,
                                selected,
                                install_page.as_ref(),
                                size,
                                false,
                            ) {
                                scroll = scroll.saturating_sub(1);
                            }
                        }
                        KeyCode::Char('r') => {
                            if let Some(page) = &mut install_page {
                                page.refresh();
                            }
                            dashboard =
                                dashboard::Dashboard::load(active_manifest.as_deref(), text)?;
                            scroll = 0;
                            focused_row = first_selectable(
                                dashboard.as_ref(),
                                text,
                                selected,
                                install_page.as_ref(),
                            );
                            last_runtime_refresh = Instant::now();
                        }
                        KeyCode::Char('n') if selected == 0 && dashboard.is_none() => {
                            run_action(
                                terminal,
                                active_manifest.as_deref(),
                                text,
                                action,
                                Action::CreateProject,
                                task_backend(runs),
                            )?;
                            dashboard =
                                dashboard::Dashboard::load(active_manifest.as_deref(), text)?;
                        }
                        KeyCode::Char('s') if selected == 0 && dashboard.is_none() => {
                            install_page = Some(InstallPage::ScannedProjects(
                                dashboard::Dashboard::scan_projects(&std::env::current_dir()?),
                            ));
                            focused_row = first_selectable(
                                dashboard.as_ref(),
                                text,
                                selected,
                                install_page.as_ref(),
                            );
                            scroll = 0;
                        }
                        KeyCode::Char('b') | KeyCode::Char('B') if selected == 2 => {
                            let rebuild = key.code == KeyCode::Char('B');
                            compile(
                                terminal,
                                dashboard.as_ref(),
                                text,
                                build,
                                rebuild,
                                task_backend(runs),
                            )?;
                            dashboard =
                                dashboard::Dashboard::load(active_manifest.as_deref(), text)?;
                            scroll = 0;
                            focused_row =
                                first_selectable(dashboard.as_ref(), text, selected, None);
                            last_runtime_refresh = Instant::now();
                        }
                        KeyCode::Char('a') if selected == 1 => {
                            run_action(
                                terminal,
                                active_manifest.as_deref(),
                                text,
                                action,
                                Action::Apply,
                                task_backend(runs),
                            )?;
                            dashboard =
                                dashboard::Dashboard::load(active_manifest.as_deref(), text)?;
                        }
                        KeyCode::Char('u') if selected == 1 => {
                            run_action(
                                terminal,
                                active_manifest.as_deref(),
                                text,
                                action,
                                Action::Uninstall,
                                task_backend(runs),
                            )?;
                            dashboard =
                                dashboard::Dashboard::load(active_manifest.as_deref(), text)?;
                        }
                        KeyCode::Char('D') if selected == 1 => {
                            run_action(
                                terminal,
                                active_manifest.as_deref(),
                                text,
                                action,
                                Action::DepsInstall,
                                task_backend(runs),
                            )?;
                            dashboard =
                                dashboard::Dashboard::load(active_manifest.as_deref(), text)?;
                        }
                        KeyCode::Char('d') if selected == 1 => {
                            run_action(
                                terminal,
                                active_manifest.as_deref(),
                                text,
                                action,
                                Action::DepsCheck,
                                task_backend(runs),
                            )?;
                            dashboard =
                                dashboard::Dashboard::load(active_manifest.as_deref(), text)?;
                        }
                        KeyCode::Char('c') if selected == 5 => {
                            run_action(
                                terminal,
                                active_manifest.as_deref(),
                                text,
                                action,
                                Action::Check,
                                task_backend(runs),
                            )?;
                            dashboard =
                                dashboard::Dashboard::load(active_manifest.as_deref(), text)?;
                        }
                        KeyCode::Char('v') if selected == 5 => {
                            run_action(
                                terminal,
                                active_manifest.as_deref(),
                                text,
                                action,
                                Action::Verify,
                                task_backend(runs),
                            )?;
                            dashboard =
                                dashboard::Dashboard::load(active_manifest.as_deref(), text)?;
                        }
                        _ => {}
                    }
                }
                Event::Mouse(mouse) => {
                    // Shift-drag remains terminal text selection instead of a TUI action.
                    if mouse.modifiers.contains(KeyModifiers::SHIFT) {
                        left_press = None;
                        continue;
                    }
                    let cell = (mouse.column, mouse.row);
                    match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            left_press = Some(cell);
                            continue;
                        }
                        MouseEventKind::Up(MouseButton::Left)
                            if left_press.take() != Some(cell) =>
                        {
                            continue;
                        }
                        _ => {}
                    }
                    let size = terminal.size()?;
                    if let Some(log) = browser.as_mut() {
                        match mouse.kind {
                            MouseEventKind::ScrollDown => {
                                log.scroll =
                                    (log.scroll + 3).min(log.lines.len().saturating_sub(1));
                            }
                            MouseEventKind::ScrollUp => {
                                log.scroll = log.scroll.saturating_sub(3);
                            }
                            MouseEventKind::Up(MouseButton::Left)
                                if mouse.row == log_view::SOURCE_ROW =>
                            {
                                if let Some(forward) =
                                    log_view::source_direction_at(mouse.column, size.width)
                                {
                                    log.select(forward);
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }
                    match mouse.kind {
                        MouseEventKind::Up(MouseButton::Left)
                            if mouse.row == 0
                                && runs.is_some()
                                && view::process_counter_at(
                                    mouse.column,
                                    size.width,
                                    active_processes,
                                    text,
                                ) =>
                        {
                            install_page = Some(InstallPage::Run(runs::Page::processes(
                                processes.clone(),
                                process_error.clone(),
                            )));
                            focused_row = first_selectable(
                                dashboard.as_ref(),
                                text,
                                selected,
                                install_page.as_ref(),
                            );
                            scroll = 0;
                        }
                        MouseEventKind::ScrollDown => scroll = scroll.saturating_add(3),
                        MouseEventKind::ScrollUp => scroll = scroll.saturating_sub(3),
                        MouseEventKind::Up(MouseButton::Left) if mouse.row == 1 => {
                            selected = tab_at(mouse.column, size.width);
                            install_page = None;
                            scroll = 0;
                            focused_row =
                                first_selectable(dashboard.as_ref(), text, selected, None);
                        }
                        MouseEventKind::Up(MouseButton::Left)
                            if mouse.row == size.height.saturating_sub(1) =>
                        {
                            let Some(footer_action) = view::footer_action_at(
                                mouse.column,
                                size.width,
                                install_page.as_ref(),
                                selected,
                                dashboard.is_some(),
                                text,
                            ) else {
                                continue;
                            };
                            let quit = activate_footer(
                                footer_action,
                                terminal,
                                active_manifest.as_deref(),
                                &mut browser,
                                &processes,
                                &mut dashboard,
                                &mut install_page,
                                &mut focused_row,
                                &mut scroll,
                                &mut selected,
                                text,
                                install_ros,
                                install_components,
                                &mut last_runtime_refresh,
                                build,
                                action,
                                task_backend(runs),
                            )?;
                            if quit {
                                break Ok(());
                            }
                        }
                        MouseEventKind::Up(MouseButton::Left) => {
                            let rows = pages::rows(
                                dashboard.as_ref(),
                                text,
                                selected,
                                install_page.as_ref(),
                            );
                            if let Some(index) = table::row_at(
                                view::table_area(ratatui::layout::Rect::new(
                                    0,
                                    0,
                                    size.width,
                                    size.height,
                                )),
                                &rows,
                                text,
                                scroll,
                                mouse.row,
                            ) {
                                if rows.get(index).is_some_and(|row| row.selectable) {
                                    focused_row = Some(index);
                                    if let Some(InstallPage::ScannedProjects(projects)) =
                                        &install_page
                                    {
                                        if let Some(path) = projects.get(index).cloned() {
                                            open_project(
                                                path,
                                                text,
                                                &mut active_manifest,
                                                &mut dashboard,
                                                project_lock,
                                                &mut install_page,
                                                &mut focused_row,
                                                &mut scroll,
                                            )?;
                                        }
                                    } else {
                                        activate(
                                            &mut install_page,
                                            &mut focused_row,
                                            &mut scroll,
                                            dashboard.as_ref(),
                                            text,
                                            selected,
                                        );
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Event::Resize(_, _) => scroll = 0,
                _ => {}
            }
        }
    }
}

fn leave_screen<B: ratatui::backend::Backend + Write>(terminal: &mut Terminal<B>) -> Result<()> {
    execute!(terminal.backend_mut(), DisableMouseCapture)?;
    // Consume input already queued for the TUI before handing the terminal back.
    while event::poll(Duration::ZERO).unwrap_or(false) && event::read().is_ok() {}
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn lock_project(project: &dashboard::Dashboard, text: &Text) -> Result<WorkspaceLock> {
    WorkspaceLock::try_acquire_tui(&project.root)?
        .with_context(|| format!("{} ({})", text.tui_already_running, project.root.display()))
}

#[allow(clippy::too_many_arguments)] // Switching projects updates the lock and navigation state together.
fn open_project(
    path: PathBuf,
    text: &Text,
    active_manifest: &mut Option<PathBuf>,
    dashboard: &mut Option<dashboard::Dashboard>,
    project_lock: &mut Option<WorkspaceLock>,
    install_page: &mut Option<InstallPage>,
    focused: &mut Option<usize>,
    scroll: &mut usize,
) -> Result<()> {
    if let Some(project) = dashboard::Dashboard::load(Some(&path), text)? {
        match lock_project(&project, text) {
            Ok(lease) => {
                *active_manifest = Some(path);
                *dashboard = Some(project);
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
        *focused = first_selectable(dashboard.as_ref(), text, 0, install_page.as_ref());
        *scroll = 0;
    }
    Ok(())
}

fn back_run(
    install_page: &mut Option<InstallPage>,
    focused: &mut Option<usize>,
    scroll: &mut usize,
    text: &Text,
) -> bool {
    if let Some(InstallPage::Run(page)) = install_page {
        if page.back() {
            *focused = page.rows(text).iter().position(|row| row.selectable);
            *scroll = 0;
            return true;
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn install<B: ratatui::backend::Backend + Write>(
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

fn open_logs(
    dashboard: Option<&dashboard::Dashboard>,
    install_page: Option<&InstallPage>,
    focused_row: Option<usize>,
    processes: &[rospm_core::run::RunStatus],
) -> logs::Browser {
    let preferred = match install_page {
        Some(InstallPage::Run(page)) => page
            .focused_process(focused_row)
            .map(|process| process.id.as_str()),
        _ => None,
    };
    logs::Browser::new(
        dashboard.map(|project| project.root.as_path()),
        processes,
        preferred,
    )
}

#[allow(clippy::too_many_arguments)]
fn activate_footer<B: ratatui::backend::Backend + Write>(
    action: view::FooterAction,
    terminal: &mut Terminal<B>,
    manifest: Option<&Path>,
    browser: &mut Option<logs::Browser>,
    processes: &[rospm_core::run::RunStatus],
    dashboard: &mut Option<dashboard::Dashboard>,
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
        view::FooterAction::Open => activate(
            install_page,
            focused_row,
            scroll,
            dashboard.as_ref(),
            text,
            *selected,
        ),
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
        view::FooterAction::Refresh => {
            if let Some(page) = install_page {
                page.refresh();
            }
            *dashboard = dashboard::Dashboard::load(manifest, text)?;
            *scroll = 0;
            *focused_row =
                first_selectable(dashboard.as_ref(), text, *selected, install_page.as_ref());
            *last_runtime_refresh = Instant::now();
        }
        view::FooterAction::Install => install(
            terminal,
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
        view::FooterAction::Build | view::FooterAction::Rebuild => {
            compile(
                terminal,
                dashboard.as_ref(),
                text,
                build,
                action == view::FooterAction::Rebuild,
                backend,
            )?;
            *dashboard = dashboard::Dashboard::load(manifest, text)?;
            *scroll = 0;
            *focused_row = first_selectable(dashboard.as_ref(), text, *selected, None);
            *last_runtime_refresh = Instant::now();
        }
        view::FooterAction::Execute(operation) => {
            run_action(terminal, manifest, text, execute_action, operation, backend)?;
            *dashboard = dashboard::Dashboard::load(manifest, text)?;
            *scroll = 0;
            *focused_row = first_selectable(dashboard.as_ref(), text, *selected, None);
            *last_runtime_refresh = Instant::now();
        }
        view::FooterAction::CreateProject => {
            run_action(
                terminal,
                manifest,
                text,
                execute_action,
                Action::CreateProject,
                backend,
            )?;
            *dashboard = dashboard::Dashboard::load(manifest, text)?;
            *scroll = 0;
            *focused_row = first_selectable(dashboard.as_ref(), text, *selected, None);
            *last_runtime_refresh = Instant::now();
        }
        view::FooterAction::ScanProjects => {
            let root = std::env::current_dir()?;
            *install_page = Some(InstallPage::ScannedProjects(
                dashboard::Dashboard::scan_projects(&root),
            ));
            *focused_row =
                first_selectable(dashboard.as_ref(), text, *selected, install_page.as_ref());
            *scroll = 0;
        }
        view::FooterAction::RescanProjects => {
            *dashboard = dashboard::Dashboard::load(manifest, text)?;
            *scroll = 0;
            *focused_row = first_selectable(dashboard.as_ref(), text, *selected, None);
            *last_runtime_refresh = Instant::now();
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

fn run_action<B: ratatui::backend::Backend + Write>(
    terminal: &mut Terminal<B>,
    manifest: Option<&Path>,
    text: &Text,
    action: &mut impl FnMut(Action) -> Result<()>,
    operation: Action,
    backend: Option<&mut dyn RunBackend>,
) -> Result<()> {
    leave_screen(terminal)?;
    disable_raw_mode()?;
    let label = match operation {
        Action::Apply => &text.footer_apply,
        Action::Uninstall => &text.footer_uninstall,
        Action::Check => &text.footer_check,
        Action::Verify => &text.footer_verify,
        Action::DepsCheck => &text.footer_deps_check,
        Action::DepsInstall => &text.footer_deps_install,
        Action::CreateProject => &text.footer_create_project,
    };
    println!("{} {} [y/N]", text.confirm_action, label);
    let mut answer = String::new();
    let mut detached = false;
    io::stdin().read_line(&mut answer)?;
    if matches!(answer.trim(), "y" | "Y" | "yes") {
        let result = if let (Some(backend), Some(operation)) = (backend, task_operation(operation))
        {
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
                answer.clear();
                io::stdin().read_line(&mut answer)?;
            }
            Err(error) => {
                eprintln!("{error:#}");
                println!("Enter ↩");
                answer.clear();
                io::stdin().read_line(&mut answer)?;
            }
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

fn compile<B: ratatui::backend::Backend + Write>(
    terminal: &mut Terminal<B>,
    dashboard: Option<&dashboard::Dashboard>,
    text: &Text,
    build: &mut impl FnMut(bool) -> Result<()>,
    rebuild: bool,
    backend: Option<&mut dyn RunBackend>,
) -> Result<()> {
    leave_screen(terminal)?;
    disable_raw_mode()?;
    println!(
        "{} [y/N]",
        if rebuild {
            &text.confirm_rebuild
        } else {
            &text.confirm_build
        }
    );
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if matches!(answer.trim(), "y" | "Y" | "yes") {
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
        answer.clear();
        io::stdin().read_line(&mut answer)?;
    }
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

fn first_selectable(
    dashboard: Option<&dashboard::Dashboard>,
    text: &Text,
    tab: usize,
    install_page: Option<&InstallPage>,
) -> Option<usize> {
    pages::rows(dashboard, text, tab, install_page)
        .iter()
        .position(|row| row.selectable)
}

#[allow(clippy::too_many_arguments)] // Focus and scrolling use the current page and viewport together.
fn move_focus(
    focused: &mut Option<usize>,
    scroll: &mut usize,
    dashboard: Option<&dashboard::Dashboard>,
    text: &Text,
    tab: usize,
    install_page: Option<&InstallPage>,
    size: ratatui::layout::Size,
    forward: bool,
) -> bool {
    let rows = pages::rows(dashboard, text, tab, install_page);
    let selectable: Vec<_> = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| row.selectable.then_some(index))
        .collect();
    let Some(current) = focused.and_then(|index| selectable.iter().position(|&i| i == index))
    else {
        *focused = selectable.first().copied();
        *scroll = if forward { 0 } else { usize::MAX };
        return !selectable.is_empty();
    };
    let next = if forward {
        (current + 1) % selectable.len()
    } else {
        (current + selectable.len() - 1) % selectable.len()
    };
    let index = selectable[next];
    *focused = Some(index);
    if (matches!(tab, 3 | DEBUG_TAB) && install_page.is_none()
        || matches!(install_page, Some(InstallPage::Run(page)) if page.is_processes()))
        && next != 0
    {
        let area = view::table_area(ratatui::layout::Rect::new(0, 0, size.width, size.height));
        table::reveal_row(area, &rows, text, scroll, index);
    } else {
        *scroll = if next == 0 { 0 } else { usize::MAX };
    }
    true
}

fn activate(
    install_page: &mut Option<InstallPage>,
    focused: &mut Option<usize>,
    scroll: &mut usize,
    dashboard: Option<&dashboard::Dashboard>,
    text: &Text,
    selected: usize,
) {
    let rows = pages::rows(dashboard, text, selected, install_page.as_ref());
    let Some(index) = *focused else {
        return;
    };
    let Some(row) = rows.get(index).filter(|row| row.selectable) else {
        return;
    };
    if let Some(InstallPage::Run(page)) = install_page {
        *focused = page.activate(index);
        *scroll = 0;
        return;
    }
    if selected == 3 && install_page.is_none() && index == 0 {
        *install_page = Some(InstallPage::Shell);
        return;
    }
    if selected == DEBUG_TAB && install_page.is_none() {
        *install_page = Some(InstallPage::DebugTool(DEBUG_TOOLS[index]));
        return;
    }
    if let (Some(index), Some(dashboard)) = (row.run_target, dashboard) {
        *install_page = Some(InstallPage::Run(runs::Page::form(
            dashboard.run.targets[index].clone(),
        )));
        *focused = Some(0);
        *scroll = 0;
        return;
    }
    if let Some(body) = &row.detail {
        *install_page = Some(InstallPage::Detail {
            title: row.cells[0].clone(),
            body: body.clone(),
            return_to_ros: matches!(install_page.as_ref(), Some(InstallPage::Ros)),
        });
        *focused = None;
        *scroll = 0;
        return;
    }
    *install_page = match (selected, install_page.as_ref(), index) {
        (1, None, 0) => Some(InstallPage::Ros),
        (1, None, 1) => Some(InstallPage::Conda(ToolInstallation::probe())),
        (1, None, 2) if dashboard.is_some_and(|d| d.ros_installed) => Some(InstallPage::Components),
        (HELP_TAB, None, 0) => Some(InstallPage::CliHelp),
        _ => return,
    };
    *focused = first_selectable(dashboard, text, selected, install_page.as_ref());
    *scroll = 0;
}

fn tab_at(column: u16, width: u16) -> usize {
    if width == 0 {
        return 0;
    }
    (usize::from(column).saturating_mul(8) / usize::from(width)).min(7)
}

#[cfg(test)]
mod tests {
    use super::{tab_at, task_operation, Action};
    use rospm_core::worker::Operation;

    #[test]
    fn help_opens_cli_guide_without_a_project() {
        use super::{activate, first_selectable, pages, InstallPage, Text, HELP_TAB};

        let text = Text {
            tui_usage: "Launch with rospm tui".into(),
            cli_help: "CLI usage".into(),
            cli_usage: "List commands with rospm --help".into(),
            ..Text::default()
        };
        let main = pages::rows(None, &text, HELP_TAB, None);
        assert_eq!(main[0].cells[0], text.cli_help);
        assert_eq!(main[1].cells[2], text.tui_usage);

        let mut focus = first_selectable(None, &text, HELP_TAB, None);
        let mut page = None;
        let mut scroll = 5;
        activate(&mut page, &mut focus, &mut scroll, None, &text, HELP_TAB);
        assert!(matches!(&page, Some(InstallPage::CliHelp)));
        assert_eq!(focus, None);
        assert_eq!(scroll, 0);
        let guide = pages::rows(None, &text, HELP_TAB, page.as_ref());
        assert_eq!(guide[0].cells[2], text.cli_usage);
    }

    #[test]
    fn mouse_tab_hit_testing_handles_narrow_and_wide_windows() {
        assert_eq!(tab_at(0, 80), 0);
        assert_eq!(tab_at(10, 80), 1);
        assert_eq!(tab_at(79, 80), 7);
        assert_eq!(tab_at(2, 3), 5);
        assert_eq!(tab_at(0, 0), 0);
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
