//! Terminal setup and event loop for the TUI.

mod dashboard;
mod log_view;
mod logs;
mod packages;
mod pages;
mod table;
mod view;

use std::{
    io::{self, IsTerminal, Write},
    path::Path,
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
use std::time::{Duration, Instant};

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
    pub details: String,
    pub manifest_search: String,
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
    pub logs: String,
    pub diagnostics: String,
    pub status_ok: String,
    pub status_missing: String,
    pub status_mismatch: String,
    pub status_unknown: String,
    pub status_info: String,
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
    mut install_ros: impl FnMut() -> Result<()>,
    mut install_components: impl FnMut() -> Result<()>,
    mut build: impl FnMut(bool) -> Result<()>,
    mut action: impl FnMut(Action) -> Result<()>,
) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(anyhow!(text.non_terminal));
    }
    let dashboard = dashboard::Dashboard::load(manifest_path, &text)?;
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
    );
    disable_raw_mode().ok();
    let stdout = terminal.backend_mut();
    execute!(stdout, DisableMouseCapture, LeaveAlternateScreen).ok();
    stdout.flush().ok();
    terminal.show_cursor().ok();
    result
}

fn loop_ui<B: ratatui::backend::Backend + Write>(
    terminal: &mut Terminal<B>,
    manifest: Option<&Path>,
    mut dashboard: Option<dashboard::Dashboard>,
    text: &Text,
    install_ros: &mut impl FnMut() -> Result<()>,
    install_components: &mut impl FnMut() -> Result<()>,
    build: &mut impl FnMut(bool) -> Result<()>,
    action: &mut impl FnMut(Action) -> Result<()>,
) -> Result<()> {
    let mut active_manifest = manifest.map(Path::to_path_buf);
    let mut browser: Option<logs::Browser> = None;
    let mut selected = 0usize;
    let mut install_page: Option<InstallPage> = None;
    let mut scroll = 0usize;
    let mut focused_row = first_selectable(dashboard.as_ref(), text, selected, None);
    let mut last_runtime_refresh = Instant::now();
    loop {
        if last_runtime_refresh.elapsed() >= Duration::from_secs(1) {
            if let Some(dashboard) = dashboard.as_mut() {
                if let Some(browser) = browser.as_mut() {
                    browser.reload();
                } else {
                    dashboard.refresh_runtime();
                }
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
                    let page_size =
                        usize::from(terminal.size()?.height.saturating_sub(6).clamp(1, 10));
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('q') if install_page.is_some() => {
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
                                &mut last_runtime_refresh,
                            )?;
                        }
                        KeyCode::Enter => {
                            if let Some(InstallPage::ScannedProjects(projects)) = &install_page {
                                if let Some(path) =
                                    focused_row.and_then(|index| projects.get(index)).cloned()
                                {
                                    active_manifest = Some(path.clone());
                                    dashboard = dashboard::Dashboard::load(Some(&path), text)?;
                                    install_page = None;
                                    scroll = 0;
                                    focused_row =
                                        first_selectable(dashboard.as_ref(), text, 0, None);
                                }
                            } else {
                                activate(
                                    &mut browser,
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
                                    &mut browser,
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
                                    &mut browser,
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
                            run_action(terminal, text, action, Action::CreateProject)?;
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
                            compile(terminal, text, build, rebuild)?;
                            dashboard =
                                dashboard::Dashboard::load(active_manifest.as_deref(), text)?;
                            scroll = 0;
                            focused_row =
                                first_selectable(dashboard.as_ref(), text, selected, None);
                            last_runtime_refresh = Instant::now();
                        }
                        KeyCode::Char('a') if selected == 1 => {
                            run_action(terminal, text, action, Action::Apply)?;
                            dashboard =
                                dashboard::Dashboard::load(active_manifest.as_deref(), text)?;
                        }
                        KeyCode::Char('u') if selected == 1 => {
                            run_action(terminal, text, action, Action::Uninstall)?;
                            dashboard =
                                dashboard::Dashboard::load(active_manifest.as_deref(), text)?;
                        }
                        KeyCode::Char('D') if selected == 1 => {
                            run_action(terminal, text, action, Action::DepsInstall)?;
                            dashboard =
                                dashboard::Dashboard::load(active_manifest.as_deref(), text)?;
                        }
                        KeyCode::Char('d') if selected == 1 => {
                            run_action(terminal, text, action, Action::DepsCheck)?;
                            dashboard =
                                dashboard::Dashboard::load(active_manifest.as_deref(), text)?;
                        }
                        KeyCode::Char('c') if selected == 5 => {
                            run_action(terminal, text, action, Action::Check)?;
                            dashboard =
                                dashboard::Dashboard::load(active_manifest.as_deref(), text)?;
                        }
                        KeyCode::Char('v') if selected == 5 => {
                            run_action(terminal, text, action, Action::Verify)?;
                            dashboard =
                                dashboard::Dashboard::load(active_manifest.as_deref(), text)?;
                        }
                        _ => {}
                    }
                }
                Event::Mouse(mouse) => {
                    // Shift-drag remains terminal text selection instead of a TUI action.
                    if mouse.modifiers.contains(KeyModifiers::SHIFT) {
                        continue;
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
                            MouseEventKind::Down(MouseButton::Left) if mouse.row < 2 => {
                                log.select(mouse.column >= size.width / 2);
                            }
                            _ => {}
                        }
                        continue;
                    }
                    match mouse.kind {
                        MouseEventKind::ScrollDown => scroll = scroll.saturating_add(3),
                        MouseEventKind::ScrollUp => scroll = scroll.saturating_sub(3),
                        MouseEventKind::Down(MouseButton::Left)
                        | MouseEventKind::Up(MouseButton::Left)
                            if mouse.row == 1 =>
                        {
                            selected = tab_at(mouse.column, size.width);
                            install_page = None;
                            scroll = 0;
                            focused_row =
                                first_selectable(dashboard.as_ref(), text, selected, None);
                        }
                        MouseEventKind::Down(MouseButton::Left)
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
                            )?;
                            if quit {
                                break Ok(());
                            }
                        }
                        MouseEventKind::Down(MouseButton::Left) => {
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
                                            active_manifest = Some(path.clone());
                                            dashboard =
                                                dashboard::Dashboard::load(Some(&path), text)?;
                                            install_page = None;
                                            scroll = 0;
                                            focused_row =
                                                first_selectable(dashboard.as_ref(), text, 0, None);
                                        }
                                    } else {
                                        activate(
                                            &mut browser,
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
    last_runtime_refresh: &mut Instant,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    let result = match install_page.as_ref() {
        Some(InstallPage::Ros) => install_ros(),
        Some(InstallPage::Conda(ToolInstallation::Needed(bootstrap))) => {
            bootstrap.perform(&mut io::stdout()).map_err(Into::into)
        }
        Some(InstallPage::Components) => install_components(),
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

#[allow(clippy::too_many_arguments)]
fn activate_footer<B: ratatui::backend::Backend + Write>(
    action: view::FooterAction,
    terminal: &mut Terminal<B>,
    manifest: Option<&Path>,
    browser: &mut Option<logs::Browser>,
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
) -> Result<bool> {
    match action {
        view::FooterAction::Select => {
            if !move_focus(
                focused_row,
                scroll,
                dashboard.as_ref(),
                text,
                *selected,
                install_page.as_ref(),
                true,
            ) {
                *scroll = scroll.saturating_add(1);
            }
        }
        view::FooterAction::Open => activate(
            browser,
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
            last_runtime_refresh,
        )?,
        view::FooterAction::Build | view::FooterAction::Rebuild => {
            compile(terminal, text, build, action == view::FooterAction::Rebuild)?;
            *dashboard = dashboard::Dashboard::load(manifest, text)?;
            *scroll = 0;
            *focused_row = first_selectable(dashboard.as_ref(), text, *selected, None);
            *last_runtime_refresh = Instant::now();
        }
        view::FooterAction::Execute(operation) => {
            run_action(terminal, text, execute_action, operation)?;
            *dashboard = dashboard::Dashboard::load(manifest, text)?;
            *scroll = 0;
            *focused_row = first_selectable(dashboard.as_ref(), text, *selected, None);
            *last_runtime_refresh = Instant::now();
        }
        view::FooterAction::CreateProject => {
            run_action(terminal, text, execute_action, Action::CreateProject)?;
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
    text: &Text,
    action: &mut impl FnMut(Action) -> Result<()>,
    operation: Action,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
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
    io::stdin().read_line(&mut answer)?;
    if matches!(answer.trim(), "y" | "Y" | "yes") {
        if let Err(error) = action(operation) {
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

fn compile<B: ratatui::backend::Backend + Write>(
    terminal: &mut Terminal<B>,
    text: &Text,
    build: &mut impl FnMut(bool) -> Result<()>,
    rebuild: bool,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
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
        if let Err(error) = build(rebuild) {
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

fn move_focus(
    focused: &mut Option<usize>,
    scroll: &mut usize,
    dashboard: Option<&dashboard::Dashboard>,
    text: &Text,
    tab: usize,
    install_page: Option<&InstallPage>,
    forward: bool,
) -> bool {
    let rows = pages::rows(dashboard, text, tab, install_page);
    let selectable: Vec<_> = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| row.selectable.then_some(index))
        .collect();
    let Some(current) = (*focused).and_then(|index| selectable.iter().position(|&i| i == index))
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
    *focused = Some(selectable[next]);
    *scroll = if next == 0 { 0 } else { usize::MAX };
    true
}

fn activate(
    browser: &mut Option<logs::Browser>,
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
    if !rows.get(index).is_some_and(|row| row.selectable) {
        return;
    }
    if let Some(row) = rows.get(index).filter(|row| row.detail.is_some()) {
        *install_page = Some(InstallPage::Detail {
            title: row.cells[0].clone(),
            body: row.detail.clone().unwrap_or_default(),
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
        _ => {
            if let Some(dashboard) = dashboard {
                *browser = Some(logs::Browser::new(&dashboard.root, selected));
            }
            return;
        }
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
    use super::tab_at;

    #[test]
    fn mouse_tab_hit_testing_handles_narrow_and_wide_windows() {
        assert_eq!(tab_at(0, 80), 0);
        assert_eq!(tab_at(10, 80), 1);
        assert_eq!(tab_at(79, 80), 7);
        assert_eq!(tab_at(2, 3), 5);
        assert_eq!(tab_at(0, 0), 0);
    }
}
