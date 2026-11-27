//! Secondary log screen, independent of workflow tables.
use crate::{logs::Browser, view, Text};
use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
    Frame,
};

pub(crate) const SOURCE_ROW: u16 = 1;

fn source_layout() -> Layout {
    Layout::horizontal([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(3),
    ])
}

pub(crate) fn source_direction_at(column: u16, width: u16) -> Option<bool> {
    let areas = source_layout().split(Rect::new(0, SOURCE_ROW, width, 1));
    [(0, false), (2, true)]
        .into_iter()
        .find_map(|(index, forward)| {
            (areas[index].left()..areas[index].right())
                .contains(&column)
                .then_some(forward)
        })
}

pub(crate) fn draw(frame: &mut Frame<'_>, browser: &Browser, text: &Text) {
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
    frame.render_widget(Paragraph::new(view::brand()), header[0]);
    frame.render_widget(
        Paragraph::new(text.logs.as_str())
            .alignment(Alignment::Center)
            .style(Style::default().add_modifier(Modifier::BOLD)),
        header[1],
    );
    let mut block = Block::default().borders(Borders::ALL);
    if let Some(source) = browser.sources.get(browser.selected) {
        let selector = source_layout().split(areas[1]);
        let style = Style::default().fg(Color::Black).bg(Color::Gray);
        frame.render_widget(
            Paragraph::new(format!(
                "{}  ({}/{})",
                source.label,
                browser.selected + 1,
                browser.sources.len()
            ))
            .alignment(Alignment::Center)
            .style(style.add_modifier(Modifier::BOLD)),
            selector[1],
        );
        for (index, arrow) in [(0, "‹"), (2, "›")] {
            frame.render_widget(
                Paragraph::new(arrow)
                    .alignment(Alignment::Center)
                    .style(style),
                selector[index],
            );
        }
        if !source.process {
            block = block.title(source.path.display().to_string());
        }
    }
    let height = block.inner(areas[2]).height as usize;
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
        areas[2],
    );
    view::draw_footer(
        frame,
        areas[3],
        text.log_help.lines(),
        Some(std::iter::once(text.footer_refresh.as_str())),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn log_shortcuts_use_the_home_footer_styles() {
        let text = Text {
            logs: "日志".into(),
            log_help: "Esc/q 返回\nTab/Shift+Tab/‹› 切换来源".into(),
            footer_refresh: "r 刷新".into(),
            footer_rescan_projects: "r 重新扫描项目".into(),
            footer_create_project: "n 创建项目".into(),
            footer_scan_projects: "s 扫描已有项目".into(),
            ..Text::default()
        };
        let browser = Browser::new(None, &[], None);
        let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
        terminal
            .draw(|frame| view::draw(frame, None, &text, 0, None, &mut 0, None, None, None))
            .unwrap();
        let home = terminal.backend().buffer().clone();
        terminal.draw(|frame| draw(frame, &browser, &text)).unwrap();
        let log = terminal.backend().buffer();

        assert_eq!(log[(1, 9)].symbol(), "E");
        assert_eq!(log[(7, 9)].symbol(), "返");
        assert_eq!(log[(73, 9)].symbol(), "r");
        assert_eq!(log[(75, 9)].symbol(), "刷");
        assert_eq!(log[(1, 9)].style(), home[(1, 9)].style());
        assert_eq!(log[(7, 9)].style(), home[(3, 9)].style());
        assert_eq!(log[(73, 9)].style(), home[(65, 9)].style());
        assert_eq!(log[(75, 9)].style(), home[(79, 9)].style());
        assert_eq!(log[(40, 8)].symbol(), "─");
    }
}
