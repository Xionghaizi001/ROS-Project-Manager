//! Shared table styling and width-aware cell wrapping.
use crate::{pages::Entry, Text};
use ratatui::{
    layout::{Alignment, Constraint, Flex, Rect},
    style::{Color, Modifier, Style},
    text::{Span, Text as CellText},
    widgets::{Cell, Row, Table},
    Frame,
};
use rospm_core::probe::Status;

pub(crate) fn draw(
    frame: &mut Frame<'_>,
    area: Rect,
    entries: Vec<Entry>,
    text: &Text,
    scroll: u16,
) {
    let entries: Vec<_> = entries
        .into_iter()
        .map(|mut entry| {
            if entry.running {
                entry.cells[0] = format!("→ {}", entry.cells[0]);
                entry.cells[1] = text.running.clone();
            }
            entry
        })
        .collect();
    let widths = column_widths(&entries, text, area.width);
    let rows: Vec<_> = entries
        .into_iter()
        .skip(scroll as usize)
        .map(|entry| {
            let mut height = 1;
            let cells: Vec<_> = entry
                .cells
                .into_iter()
                .enumerate()
                .map(|(i, value)| {
                    let wrapped = wrap(&value, widths[i].max(1) as usize);
                    let wrapped = if entry.heading {
                        format!("\n{wrapped}\n ")
                    } else {
                        wrapped
                    };
                    height = height.max(wrapped.lines().count() as u16);
                    let alignment = if i < 2 {
                        Alignment::Center
                    } else {
                        Alignment::Left
                    };
                    let mut cell = Cell::from(CellText::from(wrapped).alignment(alignment));
                    if i == 1 && entry.running {
                        cell = cell.style(Style::default().fg(Color::Blue));
                    } else if i == 1 {
                        if let Some(status) = entry.status {
                            let color = match status {
                                Status::Ok => Color::Green,
                                Status::Missing | Status::Mismatch => Color::Red,
                                Status::Unknown => Color::Yellow,
                                Status::Info => Color::Cyan,
                            };
                            cell = cell.style(Style::default().fg(color));
                        }
                    }
                    cell
                })
                .collect();
            Row::new(cells).height(height).style(if entry.heading {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            })
        })
        .collect();
    frame.render_widget(
        Table::new(rows, widths.map(Constraint::Length))
            .flex(Flex::Start)
            .column_spacing(1)
            .header(
                Row::new(
                    [text.item.clone(), text.status.clone(), text.actual.clone()].map(|label| {
                        Cell::from(CellText::from(label).alignment(Alignment::Center))
                    }),
                )
                .style(Style::default().fg(Color::Black).bg(Color::Gray))
                .height(1),
            ),
        area,
    );
}

/// Measure terminal cells (including wide Chinese characters), with two cells
/// of breathing room. Shrink the widest columns first when the window is narrow.
fn column_widths(entries: &[Entry], text: &Text, available: u16) -> [u16; 3] {
    let headers = [&text.item, &text.status, &text.actual];
    let mut widths = std::array::from_fn(|i| {
        let longest = entries
            .iter()
            .map(|row| row.cells[i].as_str())
            .chain(std::iter::once(headers[i].as_str()))
            .flat_map(str::lines)
            .map(|line| Span::raw(line).width())
            .max()
            .unwrap_or(0);
        longest.saturating_add(2).min(u16::MAX as usize) as u16
    });
    let budget = usize::from(available.saturating_sub(2));
    while widths.iter().map(|w| usize::from(*w)).sum::<usize>() > budget {
        let index = (0..3).max_by_key(|&i| widths[i]).unwrap();
        widths[index] = widths[index].saturating_sub(1);
    }
    widths
}

fn wrap(value: &str, width: usize) -> String {
    let mut output = String::new();
    let mut column = 0;
    for ch in value.chars() {
        if ch == '\n' {
            output.push(ch);
            column = 0;
            continue;
        }
        let size = Span::raw(ch.to_string()).width();
        if column + size > width && column > 0 {
            output.push('\n');
            column = 0;
        }
        output.push(ch);
        column += size;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn chinese_and_paths_wrap_within_columns() {
        assert_eq!(wrap("语言环境", 4), "语言\n环境");
        assert_eq!(wrap("/usr/bin/git", 5), "/usr/\nbin/g\nit");
    }

    #[test]
    fn running_row_has_arrow_and_blue_status_without_background() {
        let mut terminal = Terminal::new(TestBackend::new(80, 5)).unwrap();
        let text = Text {
            running: "正在执行".into(),
            ..Text::default()
        };
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    frame.area(),
                    vec![Entry {
                        cells: ["pkg".into(), "✓".into(), "1".into()],
                        status: Some(Status::Ok),
                        heading: false,
                        running: true,
                    }],
                    &text,
                    0,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(1, 1)].symbol(), "→");
        assert_eq!(buffer[(9, 1)].symbol(), "正");
        assert_eq!(buffer[(9, 1)].fg, Color::Blue);
        assert_eq!(buffer[(9, 1)].bg, Color::Reset);
    }

    #[test]
    fn status_and_tool_path_render_in_separate_columns() {
        let mut terminal = Terminal::new(TestBackend::new(100, 10)).unwrap();
        terminal
            .draw(|frame| {
                draw(
                    frame,
                    frame.area(),
                    vec![Entry {
                        cells: ["git".into(), "✓".into(), "/usr/bin/git".into()],
                        status: Some(Status::Ok),
                        heading: false,
                        running: false,
                    }],
                    &Text::default(),
                    0,
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(7, 1)].symbol(), "✓");
        assert_eq!(buffer[(7, 1)].fg, Color::Green);
        assert_eq!(buffer[(7, 1)].bg, Color::Reset);
        assert_eq!(buffer[(10, 1)].symbol(), "/");
    }
}
