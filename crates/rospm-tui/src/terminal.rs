use std::{
    cell::Cell,
    io::{self, IsTerminal, Write},
    path::Path,
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture},
    execute, queue,
    terminal::{
        disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, EndSynchronizedUpdate,
        EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use ratatui::{backend::CrosstermBackend, Frame, Terminal};

use crate::{app::loop_ui, dashboard, navigation::lock_project, Action, RunBackend, Text};

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
    let drawing = Cell::new(false);
    let mut terminal = Terminal::new(CrosstermBackend::new(FrameOutput {
        writer: stdout,
        drawing: &drawing,
        buffer: Vec::new(),
    }))
    .context("failed to initialise terminal UI")?;
    let result = loop_ui(
        &mut terminal,
        &drawing,
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

struct FrameOutput<'a, W> {
    writer: W,
    drawing: &'a Cell<bool>,
    buffer: Vec<u8>,
}

impl<W: Write> Write for FrameOutput<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.drawing.get() {
            self.buffer.write(bytes)
        } else {
            self.writer.write(bytes)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.drawing.get() {
            return Ok(());
        }
        let written = self.writer.write_all(&self.buffer);
        self.buffer.clear();
        written?;
        self.writer.flush()
    }
}

pub(super) fn draw_frame<B: ratatui::backend::Backend + Write>(
    terminal: &mut Terminal<B>,
    drawing: &Cell<bool>,
    render: impl FnOnce(&mut Frame<'_>),
) -> io::Result<()> {
    // Ratatui flushes its resize clear before drawing. Keep it in the same terminal update.
    drawing.set(true);
    let result = (|| {
        queue!(terminal.backend_mut(), BeginSynchronizedUpdate)?;
        let drawn = terminal.draw(render).map(|_| ());
        queue!(terminal.backend_mut(), EndSynchronizedUpdate)?;
        drawn
    })();
    drawing.set(false);
    result.and(Write::flush(terminal.backend_mut()))
}

pub(super) fn leave_screen<B: ratatui::backend::Backend + Write>(
    terminal: &mut Terminal<B>,
) -> Result<()> {
    execute!(terminal.backend_mut(), DisableMouseCapture)?;
    // Consume input already queued for the TUI before handing the terminal back.
    while event::poll(Duration::ZERO).unwrap_or(false) && event::read().is_ok() {}
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::terminal::{Clear, ClearType};

    #[test]
    fn resize_clear_and_frame_are_sent_together() {
        let drawing = Cell::new(true);
        let mut output = FrameOutput {
            writer: Vec::new(),
            drawing: &drawing,
            buffer: Vec::new(),
        };

        execute!(output, Clear(ClearType::All)).unwrap();
        output.write_all(b"footer").unwrap();
        output.flush().unwrap();
        assert!(output.writer.is_empty());

        drawing.set(false);
        output.flush().unwrap();
        assert_eq!(output.writer, b"\x1b[2Jfooter");
    }
}
