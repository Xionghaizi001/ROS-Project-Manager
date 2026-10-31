//! The terminal UI entry point.
//!
//! This module owns terminal setup and teardown.  Business operations stay in
//! `rospm-core` and the existing command modules so the TUI cannot acquire a
//! second implementation of CLI behavior.

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
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    widgets::{Block, Borders, Paragraph},
    Terminal,
};

/// Localized strings supplied by the CLI layer.
pub struct Text {
    pub title: String,
    pub workspace: String,
    pub manifest_search: String,
    pub quit: String,
    pub non_terminal: String,
}

/// Start the interactive UI.  Both input and output must be terminals: this
/// avoids writing control sequences into pipes and makes the fallback useful
/// in CI and scripts.
pub fn run(manifest_path: Option<&Path>, text: Text) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(anyhow!(text.non_terminal));
    }

    enable_raw_mode().context("failed to enable terminal raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("failed to initialise terminal UI")?;

    let result = loop_ui(&mut terminal, manifest_path, &text);

    // Always restore the caller's terminal, including when drawing or event
    // handling fails.  Keep the first error as the useful one to report.
    disable_raw_mode().ok();
    let stdout = terminal.backend_mut();
    execute!(stdout, LeaveAlternateScreen).ok();
    stdout.flush().ok();
    terminal.show_cursor().ok();
    result
}

fn loop_ui<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    manifest_path: Option<&Path>,
    text: &Text,
) -> Result<()> {
    loop {
        terminal.draw(|frame| {
            let area = frame.area();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Min(1)])
                .split(area);
            let title =
                Paragraph::new(text.title.as_str()).block(Block::default().borders(Borders::ALL));
            let manifest = manifest_path
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| text.manifest_search.clone());
            let body = Paragraph::new(format!("{}\n\n{}", manifest, text.quit)).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(text.workspace.as_str()),
            );
            frame.render_widget(title, chunks[0]);
            frame.render_widget(body, chunks[1]);
        })?;

        if event::poll(std::time::Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                    break Ok(());
                }
            }
        }
    }
}
