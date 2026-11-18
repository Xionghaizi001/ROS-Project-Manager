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
use std::time::{Duration, Instant};

/// Localized strings supplied by the CLI layer.
#[derive(Default)]
pub struct Text {
    pub no_logs: String,
    pub log_error: String,
    pub log_help: String,
    pub open_logs: String,
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
    pub not_connected: String,
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
    pub quit: String,
    pub mouse_help: String,
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

pub fn run(
    manifest_path: Option<&Path>,
    text: Text,
    mut install_ros: impl FnMut() -> Result<()>,
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
) -> Result<()> {
    let mut browser: Option<logs::Browser> = None;
    let mut selected = 0usize;
    let mut ros_page = false;
    let mut workspace = 0usize;
    let mut scroll = 0usize;
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
                manifest,
                dashboard.as_ref(),
                text,
                selected,
                ros_page,
                &mut scroll,
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
                        KeyCode::Enter if selected == 1 && !ros_page && dashboard.is_some() => {
                            ros_page = true;
                            scroll = 0;
                        }
                        KeyCode::Esc | KeyCode::Char('q') if ros_page => {
                            ros_page = false;
                            scroll = 0;
                        }
                        KeyCode::Char('i') if ros_page => {
                            disable_raw_mode()?;
                            execute!(
                                terminal.backend_mut(),
                                DisableMouseCapture,
                                LeaveAlternateScreen
                            )?;
                            terminal.show_cursor()?;
                            if let Err(error) = install_ros() {
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
                            dashboard = dashboard::Dashboard::load(manifest, text)?;
                            last_runtime_refresh = Instant::now();
                        }
                        KeyCode::Char('l') => {
                            if let Some(d) = &dashboard {
                                browser = Some(logs::Browser::new(&d.root, selected));
                            }
                        }
                        KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                        KeyCode::Left | KeyCode::BackTab if !ros_page => {
                            selected = (selected + 7) % 8;
                            scroll = 0;
                        }
                        KeyCode::Right | KeyCode::Tab if !ros_page => {
                            selected = (selected + 1) % 8;
                            scroll = 0;
                        }
                        KeyCode::PageDown => scroll = scroll.saturating_add(page_size),
                        KeyCode::PageUp => scroll = scroll.saturating_sub(page_size),
                        KeyCode::Home => scroll = 0,
                        KeyCode::Down => scroll = scroll.saturating_add(1),
                        KeyCode::Up => scroll = scroll.saturating_sub(1),
                        KeyCode::Char('j') => {
                            if let Some(dashboard) = dashboard.as_ref() {
                                if !dashboard.workspaces.is_empty() {
                                    workspace = (workspace + 1) % dashboard.workspaces.len();
                                }
                            }
                        }
                        KeyCode::Char('k') => workspace = workspace.saturating_sub(1),
                        KeyCode::Char('r') => {
                            workspace = 0;
                            dashboard = dashboard::Dashboard::load(manifest, text)?;
                            scroll = 0;
                            last_runtime_refresh = Instant::now();
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
                        MouseEventKind::Down(MouseButton::Left) if mouse.row == 1 => {
                            selected = tab_at(mouse.column, size.width);
                            ros_page = false;
                            scroll = 0;
                        }
                        MouseEventKind::Down(MouseButton::Left)
                            if selected == 1
                                && !ros_page
                                && mouse.row == 4
                                && dashboard.is_some() =>
                        {
                            ros_page = true;
                            scroll = 0;
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
