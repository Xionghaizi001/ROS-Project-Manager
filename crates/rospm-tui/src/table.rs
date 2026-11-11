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
    scroll: &mut usize,
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
    // Split logical entries into screen rows so even an entry taller than the viewport can scroll.
    let rows: Vec<_> = entries
        .into_iter()
        .flat_map(|entry| {
            let cells: Vec<_> = entry
                .cells
                .iter()
                .enumerate()
                .map(|(i, value)| {
                    let wrapped = wrap(value, widths[i].max(1) as usize);
                    let wrapped = if entry.heading {
                        format!("\n{wrapped}\n ")
                    } else {
                        wrapped
                    };
                    wrapped.lines().map(str::to_owned).collect::<Vec<_>>()
                })
                .collect();
            let height = cells.iter().map(Vec::len).max().unwrap_or(1).max(1);
            (0..height).map(move |line| {
                let row = cells.iter().enumerate().map(|(i, lines)| {
                    let alignment = if i < 2 {
                        Alignment::Center
                    } else {
                        Alignment::Left
                    };
                    let value = lines.get(line).cloned().unwrap_or_default();
                    let mut cell = Cell::from(CellText::from(value).alignment(alignment));
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
                });
                Row::new(row).style(if entry.heading {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                })
            })
        })
        .collect();
    let visible = usize::from(area.height.saturating_sub(1));
    *scroll = (*scroll).min(rows.len().saturating_sub(visible.max(1)));
    frame.render_widget(
        Table::new(
            rows.into_iter().skip(*scroll),
            widths.map(Constraint::Length),
        )
        .flex(Flex::Start)
        .column_spacing(1)
        .header(
            Row::new(
                [text.item.clone(), text.status.clone(), text.actual.clone()]
                    .map(|label| Cell::from(CellText::from(label).alignment(Alignment::Center))),
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
    fn oversized_entry_and_following_entry_remain_reachable() {
        let text = Text {
            item: "I".into(),
            status: "S".into(),
            actual: "A".into(),
            ..Text::default()
        };
        let expected: Vec<_> = (0..30).map(|i| format!("{i:012}")).collect();
        let entries = || {
            vec![
                Entry {
                    cells: ["1".into(), String::new(), expected.concat()],
                    status: None,
                    heading: false,
                    running: false,
                },
                Entry {
                    cells: ["2".into(), String::new(), "tail".into()],
                    status: None,
                    heading: false,
                    running: false,
                },
            ]
        };
        for height in [4, 8, 13] {
            let mut terminal = Terminal::new(TestBackend::new(20, height)).unwrap();
            let mut scroll = 0;
            let mut seen = std::collections::BTreeSet::new();
            for _ in 0..32 {
                terminal
                    .draw(|frame| draw(frame, frame.area(), entries(), &text, &mut scroll))
                    .unwrap();
                let buffer = terminal.backend().buffer();
                assert!((8..20).any(|x| buffer[(x, 0)].symbol() == "A"));
                for y in 1..height {
                    let value: String = (8..20).map(|x| buffer[(x, y)].symbol()).collect();
                    seen.insert(value.trim_end().to_owned());
                }
                if seen.contains("tail") {
                    break;
                }
                scroll += usize::from((height - 1).min(10));
            }
            assert!(seen.contains("tail"));
            assert!(expected.iter().all(|line| seen.contains(line)));

            scroll = usize::MAX;
            terminal
                .draw(|frame| draw(frame, frame.area(), entries(), &text, &mut scroll))
                .unwrap();
            assert_eq!(scroll, 31 - usize::from(height - 1));

            terminal.backend_mut().resize(40, 40);
            terminal
                .draw(|frame| draw(frame, frame.area(), entries(), &text, &mut scroll))
                .unwrap();
            assert_eq!(
                scroll, 0,
                "resize should clamp the offset to the new content height"
            );
        }
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
                    &mut 0,
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
                    &mut 0,
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
