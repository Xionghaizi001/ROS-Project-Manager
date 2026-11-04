//! Secondary log screen, independent of workflow tables.
use crate::{logs::Browser, Text};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
    Frame,
};

pub(crate) fn draw(frame: &mut Frame<'_>, browser: &Browser, text: &Text) {
    let areas = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .split(frame.area());
    let title = browser
        .sources
        .get(browser.selected)
        .map(|s| {
            format!(
                "{} ({}/{}) {}\n{}",
                text.logs,
                browser.selected + 1,
                browser.sources.len(),
                s.label,
                s.path.display()
            )
        })
        .unwrap_or_else(|| text.no_logs.clone());
    frame.render_widget(Paragraph::new(title), areas[0]);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(text.logs.as_str());
    let height = block.inner(areas[1]).height as usize;
    let lines = if let Some(error) = &browser.error {
        vec![Line::styled(
            format!("{}: {error}", text.log_error),
            Style::default().fg(Color::Red),
        )]
    } else if browser.lines.is_empty() {
        vec![Line::from(text.no_logs.as_str())]
    } else {
        browser
            .lines
            .iter()
            .skip(browser.scroll)
            .take(height)
            .map(|line| Line::from(line.as_str()))
            .collect()
    };
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((0, browser.horizontal))
            .block(block),
        areas[1],
    );
    frame.render_widget(
        Paragraph::new(text.log_help.as_str()).style(Style::default().fg(Color::Gray)),
        areas[2],
    );
}
