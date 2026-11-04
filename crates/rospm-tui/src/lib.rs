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
    event::{self, Event, KeyCode},
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

pub fn run(manifest_path: Option<&Path>, text: Text) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(anyhow!(text.non_terminal));
    }
    let dashboard = dashboard::Dashboard::load(manifest_path, &text)?;
    enable_raw_mode().context("failed to enable terminal raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("failed to enter alternate screen")?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(stdout)).context("failed to initialise terminal UI")?;
    let result = loop_ui(&mut terminal, manifest_path, dashboard, &text);
    disable_raw_mode().ok();
    let stdout = terminal.backend_mut();
    execute!(stdout, LeaveAlternateScreen).ok();
    stdout.flush().ok();
    terminal.show_cursor().ok();
    result
}

fn loop_ui<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    manifest: Option<&Path>,
    mut dashboard: Option<dashboard::Dashboard>,
    text: &Text,
) -> Result<()> {
    let mut browser: Option<logs::Browser> = None;
    let mut selected = 0usize;
    let mut workspace = 0usize;
    let mut scroll = 0u16;
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
                workspace,
                scroll,
            )
        })?;
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
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
                            log.scroll = (log.scroll + 10).min(log.lines.len().saturating_sub(1))
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
                match key.code {
                    KeyCode::Char('l') => {
                        if let Some(d) = &dashboard {
                            browser = Some(logs::Browser::new(&d.root, selected));
                        }
                    }
                    KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                    KeyCode::Left | KeyCode::BackTab => {
                        selected = (selected + 7) % 8;
                        scroll = 0;
                    }
                    KeyCode::Right | KeyCode::Tab => {
                        selected = (selected + 1) % 8;
                        scroll = 0;
                    }
                    KeyCode::PageDown => scroll = scroll.saturating_add(10),
                    KeyCode::PageUp => scroll = scroll.saturating_sub(10),
                    KeyCode::Home => scroll = 0,
                    KeyCode::Char('j') => {
                        if let Some(dashboard) = dashboard.as_ref() {
                            if !dashboard.workspaces.is_empty() {
                                workspace = (workspace + 1) % dashboard.workspaces.len();
                            }
                        }
                    }
                    KeyCode::Char('k') => workspace = workspace.saturating_sub(1),
                    KeyCode::Char('r') => {
                        dashboard = dashboard::Dashboard::load(manifest, text)?;
                        workspace = 0;
                        last_runtime_refresh = Instant::now();
                    }
                    _ => {}
                }
            }
        }
    }
}
