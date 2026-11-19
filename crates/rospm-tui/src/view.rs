//! Three horizontal regions: project title, workflow tabs, and page content.
use crate::{dashboard::Dashboard, pages, InstallPage, Text};
use ratatui::{
    layout::{Alignment, Constraint, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Paragraph},
};

pub(crate) fn draw(
    frame: &mut ratatui::Frame<'_>,
    dashboard: Option<&Dashboard>,
    text: &Text,
    selected: usize,
    install_page: Option<&InstallPage>,
    scroll: &mut usize,
    focused: Option<usize>,
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
    let block = Block::default()
        .borders(Borders::ALL)
        .title(match install_page {
            Some(InstallPage::Ros) => format!("{} / ROS", text.tabs[1]),
            Some(InstallPage::Conda(_)) => format!("{} / {}", text.tabs[1], text.conda),
            None => text.tabs[selected].clone(),
        });
    let inner = block.inner(content);
    frame.render_widget(block, content);
    let rows = pages::rows(dashboard, text, selected, install_page);
    if !rows.is_empty() {
        crate::table::draw(frame, inner, rows, text, scroll, focused);
    }
    frame.render_widget(
        Paragraph::new(if let Some(page) = install_page {
            let enter = if matches!(page, InstallPage::Ros) {
                "Enter   "
            } else {
                ""
            };
            let install = if page.can_install() {
                format!("i {}   ", text.tabs[1])
            } else {
                String::new()
            };
            format!(
                "↑↓ {enter}{install}PgUp/PgDn   r {}   {}   Esc ↩",
                text.refresh, text.mouse_help
            )
        } else if selected == 1 {
            format!(
                "↑↓ Enter   ←→ / Tab   r {}   {}   {}",
                text.refresh, text.mouse_help, text.quit
            )
        } else {
            format!(
                "↑↓ Enter   ←→ / Tab   PgUp/PgDn   r {}   {}   {}",
                text.refresh, text.mouse_help, text.quit
            )
        })
        .style(Style::default().fg(Color::Gray)),
        areas[3],
    );
}

pub(crate) fn table_area(area: ratatui::layout::Rect) -> ratatui::layout::Rect {
    let areas = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(area);
    Block::default().borders(Borders::ALL).inner(areas[2])
}
