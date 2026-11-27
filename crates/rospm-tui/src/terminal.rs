use std::{
    cell::Cell,
    io::{self, IsTerminal, Write},
    path::Path,
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use crossterm::{
    cursor::Hide,
    event::{self, DisableMouseCapture, EnableMouseCapture},
    execute, queue,
    terminal::{
        disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, EndSynchronizedUpdate,
        EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use ratatui::{
    backend::{Backend, CrosstermBackend, WindowSize},
    buffer::Cell as BufferCell,
    layout::{Position, Size},
    Frame, Terminal,
};

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
    let screen = Cell::new(Screen::Suspended);
    let mut terminal = Terminal::new(ScreenBackend {
        backend: CrosstermBackend::new(FrameOutput {
            writer: io::stdout(),
            screen: &screen,
            buffer: Vec::new(),
        }),
        footer_row: Cell::new(0),
    })
    .context("failed to initialise terminal UI")?;
    let result = loop_ui(
        &mut terminal,
        &screen,
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
    leave_screen(&mut terminal, &screen).ok();
    disable_raw_mode().ok();
    result
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Screen {
    Suspended,
    Drawing,
    Active,
}

struct ScreenBackend<W: Write> {
    backend: CrosstermBackend<W>,
    footer_row: Cell<u16>,
}

impl<W: Write> Backend for ScreenBackend<W> {
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a BufferCell)>,
    {
        // Terminals without synchronized updates can paint before the frame has arrived.
        let updates: Vec<_> = content.collect();
        let footer_start = updates.partition_point(|&(_, row, _)| row < self.footer_row.get());
        let (page, footer) = updates.split_at(footer_start);
        self.backend.draw(footer.iter().chain(page).copied())
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.backend.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.backend.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.backend.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.backend.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.backend.clear()
    }

    fn size(&self) -> io::Result<Size> {
        let size = self.backend.size()?;
        self.footer_row.set(size.height.saturating_sub(1));
        Ok(size)
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.backend.window_size()
    }

    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.backend)
    }
}

impl<W: Write> Write for ScreenBackend<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.backend.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Write::flush(&mut self.backend)
    }
}

struct FrameOutput<'a, W> {
    writer: W,
    screen: &'a Cell<Screen>,
    buffer: Vec<u8>,
}

impl<W: Write> Write for FrameOutput<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.screen.get() == Screen::Drawing {
            self.buffer.write(bytes)
        } else {
            self.writer.write(bytes)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.screen.get() == Screen::Drawing {
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
    screen: &Cell<Screen>,
    render: impl FnOnce(&mut Frame<'_>),
) -> io::Result<()> {
    // Keep screen entry and any clear in the same terminal update as the frame.
    let resume = screen.replace(Screen::Drawing) == Screen::Suspended;
    let result = (|| {
        queue!(terminal.backend_mut(), BeginSynchronizedUpdate)?;
        let drawn = (|| {
            if resume {
                queue!(
                    terminal.backend_mut(),
                    EnterAlternateScreen,
                    EnableMouseCapture,
                    Hide
                )?;
                terminal.clear()?;
            }
            terminal.draw(render).map(|_| ())
        })();
        queue!(terminal.backend_mut(), EndSynchronizedUpdate)?;
        drawn
    })();
    screen.set(Screen::Active);
    result.and(Write::flush(terminal.backend_mut()))
}

pub(super) fn leave_screen<B: ratatui::backend::Backend + Write>(
    terminal: &mut Terminal<B>,
    screen: &Cell<Screen>,
) -> Result<()> {
    if screen.get() == Screen::Suspended {
        return Ok(());
    }
    execute!(terminal.backend_mut(), DisableMouseCapture)?;
    // Consume input already queued for the TUI before handing the terminal back.
    while event::poll(Duration::ZERO).unwrap_or(false) && event::read().is_ok() {}
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    screen.set(Screen::Suspended);
    terminal.show_cursor()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::terminal::{Clear, ClearType};

    #[test]
    fn footer_diff_is_sent_before_page_changes() {
        let mut output = Vec::new();
        let mut backend = ScreenBackend {
            backend: CrosstermBackend::new(&mut output),
            footer_row: Cell::new(24),
        };
        let page = BufferCell::new("P");
        let footer = BufferCell::new("底");
        let next = BufferCell::new("栏");
        backend
            .draw([(0, 2, &page), (0, 24, &footer), (2, 24, &next)].into_iter())
            .unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.starts_with("\x1b[25;1H底\x1b[25;3H栏\x1b[3;1HP"));
    }

    #[test]
    fn resize_clear_and_frame_are_sent_together() {
        let screen = Cell::new(Screen::Drawing);
        let mut output = FrameOutput {
            writer: Vec::new(),
            screen: &screen,
            buffer: Vec::new(),
        };

        execute!(output, Clear(ClearType::All)).unwrap();
        output.write_all(b"footer").unwrap();
        output.flush().unwrap();
        assert!(output.writer.is_empty());

        screen.set(Screen::Active);
        output.flush().unwrap();
        assert_eq!(output.writer, b"\x1b[2Jfooter");
    }
}
