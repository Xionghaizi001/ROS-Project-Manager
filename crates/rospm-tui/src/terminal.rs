use std::{
    cell::Cell,
    io::{self, IsTerminal, Write},
    path::Path,
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use crossterm::{
    cursor::{Hide, MoveTo, SetCursorStyle},
    event::{self, DisableBracketedPaste, DisableMouseCapture, EnableMouseCapture},
    execute, queue,
    style::{Attribute, ResetColor, SetAttribute},
    terminal::{
        disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, Clear, ClearType,
        EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
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
        .map(|project| lock_project(&project.root, &text))
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
    Command,
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

    fn clear_region(&mut self, clear_type: ratatui::backend::ClearType) -> io::Result<()> {
        self.backend.clear_region(clear_type)
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
    let previous = screen.replace(Screen::Drawing);
    let result = (|| {
        queue!(terminal.backend_mut(), BeginSynchronizedUpdate)?;
        let drawn = (|| {
            if previous == Screen::Suspended {
                queue!(terminal.backend_mut(), EnterAlternateScreen)?;
            }
            if matches!(previous, Screen::Suspended | Screen::Command) {
                reset_command_modes(terminal.backend_mut())?;
                queue!(
                    terminal.backend_mut(),
                    SetAttribute(Attribute::Reset),
                    ResetColor,
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

fn reset_command_modes(writer: &mut impl Write) -> io::Result<()> {
    queue!(
        writer,
        DisableMouseCapture,
        DisableBracketedPaste,
        SetCursorStyle::DefaultUserShape
    )?;
    // Restore normal cursor keys and the numeric keypad after an interactive shell.
    writer.write_all(b"\x1b[?1l\x1b>")
}

/// Give a shell or task a clean full-screen surface without exposing the caller's screen.
pub(super) fn command_screen<B: ratatui::backend::Backend + Write>(
    terminal: &mut Terminal<B>,
    screen: &Cell<Screen>,
) -> Result<()> {
    if screen.get() == Screen::Suspended {
        execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    }
    screen.set(Screen::Command);
    execute!(terminal.backend_mut(), DisableMouseCapture)?;
    // Do not pass queued TUI shortcuts or mouse events to the command.
    while event::poll(Duration::ZERO).unwrap_or(false) && event::read().is_ok() {}
    execute!(
        terminal.backend_mut(),
        SetAttribute(Attribute::Reset),
        ResetColor,
        Clear(ClearType::All),
        MoveTo(0, 0)
    )?;
    terminal.show_cursor()?;
    Ok(())
}

pub(super) fn leave_screen<B: ratatui::backend::Backend + Write>(
    terminal: &mut Terminal<B>,
    screen: &Cell<Screen>,
) -> Result<()> {
    if screen.get() == Screen::Suspended {
        return Ok(());
    }
    reset_command_modes(terminal.backend_mut())?;
    Write::flush(terminal.backend_mut())?;
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
    use ratatui::{layout::Rect, widgets::Paragraph, TerminalOptions, Viewport};
    use std::{cell::RefCell, rc::Rc};

    #[derive(Clone, Default)]
    struct Capture(Rc<RefCell<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Capture {
        fn apply(&self, display: &mut vt100::Parser) {
            display.process(&std::mem::take(&mut *self.0.borrow_mut()));
        }
    }

    #[test]
    fn commands_and_dashboard_share_only_the_alternate_screen() {
        let screen = Cell::new(Screen::Suspended);
        let capture = Capture::default();
        let mut terminal = Terminal::with_options(
            ScreenBackend {
                backend: CrosstermBackend::new(FrameOutput {
                    writer: capture.clone(),
                    screen: &screen,
                    buffer: Vec::new(),
                }),
                footer_row: Cell::new(23),
            },
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, 80, 24)),
            },
        )
        .unwrap();
        let mut display = vt100::Parser::new(24, 80, 100);
        display.process(b"caller history\r\n$ rospm tui");
        let caller = display.screen().contents();
        let cursor = display.screen().cursor_position();

        for content in [
            "Shell output",
            "task output",
            "Shell reattached",
            "task failed",
        ] {
            draw_frame(&mut terminal, &screen, |frame| {
                frame.render_widget(Paragraph::new("dashboard"), frame.area());
            })
            .unwrap();
            capture.apply(&mut display);
            assert!(display.screen().alternate_screen());
            assert_eq!(display.screen().contents(), "dashboard");
            assert!(!display.screen().application_cursor());
            assert!(!display.screen().bracketed_paste());

            command_screen(&mut terminal, &screen).unwrap();
            capture.apply(&mut display);
            assert!(display.screen().alternate_screen());
            assert!(display.screen().contents().is_empty());
            assert_eq!(display.screen().cursor_position(), (0, 0));
            assert!(!display.screen().hide_cursor());
            terminal
                .backend_mut()
                .write_all(content.as_bytes())
                .unwrap();
            // A shell may change input modes before returning to the dashboard.
            terminal
                .backend_mut()
                .write_all(b"\x1b[?1h\x1b[?2004h")
                .unwrap();
            capture.apply(&mut display);
            assert_eq!(display.screen().contents(), content);
        }
        // Cleanup also works when a command fails before the next dashboard frame.
        leave_screen(&mut terminal, &screen).unwrap();
        capture.apply(&mut display);
        assert!(!display.screen().alternate_screen());
        assert_eq!(display.screen().contents(), caller);
        assert_eq!(display.screen().cursor_position(), cursor);
        assert!(!display.screen().application_cursor());
        assert!(!display.screen().bracketed_paste());
    }

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
