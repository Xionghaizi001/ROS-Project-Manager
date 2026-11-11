//! Three horizontal regions: project title, workflow tabs, and page content.
use crate::{dashboard::Dashboard, pages, Text};
use ratatui::{
    layout::{Alignment, Constraint, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Paragraph},
};
use std::path::Path;

pub(crate) fn draw(
    frame: &mut ratatui::Frame<'_>,
    _manifest: Option<&Path>,
    dashboard: Option<&Dashboard>,
    text: &Text,
    selected: usize,
    ros_page: bool,
    scroll: &mut usize,
) {
    let areas = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(frame.area());
    let header = Layout::horizontal([
        Constraint::Length(10),
        Constraint::Min(1),
        Constraint::Length(10),
    ])
    .split(areas[0]);
    frame.render_widget(
        Paragraph::new("◆ rospm").style(Style::default().fg(Color::Cyan)),
        header[0],
    );
    frame.render_widget(
        Paragraph::new(dashboard.map(|d| d.project.as_str()).unwrap_or(&text.title))
            .alignment(Alignment::Center)
            .style(Style::default().add_modifier(Modifier::BOLD)),
        header[1],
    );
    let tabs = Layout::horizontal([Constraint::Ratio(1, 8); 8]).split(areas[1]);
    for (i, label) in text.tabs.iter().enumerate() {
        let style = if i == selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Rgb(175, 175, 175))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Rgb(215, 215, 215))
        };
        frame.render_widget(
            Paragraph::new(label.as_str())
                .alignment(Alignment::Center)
                .style(style),
            tabs[i],
        );
    }
    let content = areas[2];
    let block = Block::default().borders(Borders::ALL).title(if ros_page {
        format!("{} / ROS", text.tabs[1])
    } else {
        text.tabs[selected].clone()
    });
    let inner = block.inner(content);
    frame.render_widget(block, content);
    let rows = pages::rows(dashboard, text, selected, ros_page);
    if !rows.is_empty() {
        crate::table::draw(frame, inner, rows, text, scroll);
    }
    frame.render_widget(
        Paragraph::new(if ros_page {
            format!(
                "i {}   PgUp/PgDn   r {}   l {}   Esc ↩",
                text.tabs[1], text.refresh, text.logs
            )
        } else if selected == 1 {
            format!(
                "Enter › ROS   ←→ / Tab   r {}   {}",
                text.refresh, text.quit
            )
        } else {
            format!(
                "←→ / Tab   PgUp/PgDn   j/k {}   r {}   l {}   {}",
                text.workspace, text.refresh, text.logs, text.quit
            )
        })
        .style(Style::default().fg(Color::Gray)),
        areas[3],
    );
}
