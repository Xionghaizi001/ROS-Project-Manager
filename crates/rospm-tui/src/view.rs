//! Three horizontal regions: project title, workflow tabs, and page content.
use crate::{dashboard::Dashboard, pages, Action, InstallPage, Text, HELP_TAB, PROCESS_KEY};
use ratatui::{
    layout::{Alignment, Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FooterAction {
    Select,
    Open,
    ChangeTab,
    ScrollUp,
    ScrollDown,
    Refresh,
    Install,
    Back,
    Quit,
    Build,
    Rebuild,
    Execute(Action),
    CreateProject,
    ScanProjects,
    RescanProjects,
}

pub(crate) fn footer_items(
    install_page: Option<&InstallPage>,
    selected: usize,
    text: &Text,
) -> Vec<(String, FooterAction)> {
    if matches!(install_page, Some(InstallPage::CliHelp)) {
        return vec![
            (text.footer_scroll_up.clone(), FooterAction::ScrollUp),
            (text.footer_scroll_down.clone(), FooterAction::ScrollDown),
            (text.footer_back.clone(), FooterAction::Back),
        ];
    }
    let mut items = vec![
        (text.footer_select.clone(), FooterAction::Select),
        (text.footer_open.clone(), FooterAction::Open),
    ];
    if install_page.is_none() {
        items.extend([
            (text.footer_tabs.clone(), FooterAction::ChangeTab),
            (text.footer_scroll_up.clone(), FooterAction::ScrollUp),
            (text.footer_scroll_down.clone(), FooterAction::ScrollDown),
        ]);
    }
    if !matches!(selected, 0 | HELP_TAB) || install_page.is_some() {
        items.push((text.footer_refresh.clone(), FooterAction::Refresh));
    }
    if let Some(page) = install_page {
        if page.can_install() {
            items.push((text.footer_install.clone(), FooterAction::Install));
        }
        items.push((text.footer_back.clone(), FooterAction::Back));
    } else {
        items.push((text.footer_quit.clone(), FooterAction::Quit));
    }
    items
}

fn action_items(selected: usize, has_project: bool, text: &Text) -> Vec<(String, FooterAction)> {
    match selected {
        0 if has_project => vec![(
            text.footer_rescan_projects.clone(),
            FooterAction::RescanProjects,
        )],
        0 => vec![
            (
                text.footer_create_project.clone(),
                FooterAction::CreateProject,
            ),
            (
                text.footer_scan_projects.clone(),
                FooterAction::ScanProjects,
            ),
        ],
        1 => vec![
            (
                text.footer_apply.clone(),
                FooterAction::Execute(Action::Apply),
            ),
            (
                text.footer_uninstall.clone(),
                FooterAction::Execute(Action::Uninstall),
            ),
            (
                text.footer_deps_check.clone(),
                FooterAction::Execute(Action::DepsCheck),
            ),
            (
                text.footer_deps_install.clone(),
                FooterAction::Execute(Action::DepsInstall),
            ),
        ],
        2 => vec![
            (text.footer_build.clone(), FooterAction::Build),
            (text.footer_rebuild.clone(), FooterAction::Rebuild),
        ],
        5 => vec![
            (
                text.footer_check.clone(),
                FooterAction::Execute(Action::Check),
            ),
            (
                text.footer_verify.clone(),
                FooterAction::Execute(Action::Verify),
            ),
        ],
        _ => Vec::new(),
    }
}

fn footer_spans(label: &str, key_bg: Color, label_bg: Color) -> Vec<Span<'static>> {
    let style = |bg| Style::default().fg(Color::Black).bg(bg);
    let Some((key, label)) = label.split_once(' ') else {
        return vec![Span::styled(format!(" {label} "), style(label_bg))];
    };
    vec![
        Span::styled(format!(" {key} "), style(key_bg)),
        Span::styled(format!("{label} "), style(label_bg)),
    ]
}

pub(crate) fn footer_action_at(
    column: u16,
    width: u16,
    install_page: Option<&InstallPage>,
    selected: usize,
    has_project: bool,
    text: &Text,
) -> Option<FooterAction> {
    let items = footer_items(install_page, selected, text);
    let measure = |label: &str| Span::raw(label).width() + 2;
    if install_page.is_none() && matches!(selected, 0 | 1 | 2 | 5) {
        let left_width = items.iter().map(|(label, _)| measure(label)).sum::<usize>();
        let right_items = action_items(selected, has_project, text);
        let right_width = right_items
            .iter()
            .map(|(label, _)| measure(label))
            .sum::<usize>();
        let column = usize::from(column);
        if (usize::from(width).saturating_sub(right_width)..usize::from(width)).contains(&column) {
            let mut offset = usize::from(width).saturating_sub(right_width);
            for (label, action) in right_items {
                let end = offset + measure(&label);
                if (offset..end).contains(&column) {
                    return Some(action);
                }
                offset = end;
            }
        }
        if column < left_width {
            let mut offset = 0;
            for (label, action) in items {
                let end = offset + measure(&label);
                if (offset..end).contains(&column) {
                    return Some(action);
                }
                offset = end;
            }
        }
        return None;
    }
    let total = items.iter().map(|(label, _)| measure(label)).sum::<usize>();
    let start = usize::from(width).saturating_sub(total) / 2;
    let column = usize::from(column);
    if column < start {
        return None;
    }
    let mut offset = start;
    for (label, action) in items {
        let end = offset + measure(&label);
        if (offset..end).contains(&column) {
            return Some(action);
        }
        offset = end;
    }
    None
}

pub(crate) fn draw(
    frame: &mut ratatui::Frame<'_>,
    dashboard: Option<&Dashboard>,
    text: &Text,
    selected: usize,
    install_page: Option<&InstallPage>,
    scroll: &mut usize,
    focused: Option<usize>,
    process_count: Option<usize>,
) {
    let areas = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(frame.area());
    let counter = process_count.map(|count| process_counter(count, text));
    let header = Layout::horizontal([
        Constraint::Length(
            counter
                .as_ref()
                .map_or(10, |label| 8 + label.width() as u16),
        ),
        Constraint::Min(1),
        Constraint::Length(10),
    ])
    .split(areas[0]);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("◆ rospm", Style::default().fg(Color::Cyan)),
            Span::raw(" "),
            counter
                .unwrap_or_default()
                .style(Style::default().fg(Color::Black).bg(Color::Cyan)),
        ])),
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
            Some(InstallPage::Components) => format!("{} / {}", text.tabs[1], text.components),
            Some(InstallPage::Detail { title, .. }) => format!("{} / {title}", text.tabs[selected]),
            Some(InstallPage::ScannedProjects(_)) => {
                format!("{} / {}", text.tabs[0], text.project_scan_title)
            }
            Some(InstallPage::Run(page)) => page.title(text),
            Some(InstallPage::Shell) => "Shell".into(),
            Some(InstallPage::CliHelp) => format!("{} / {}", text.tabs[HELP_TAB], text.cli_help),
            None => text.tabs[selected].clone(),
        });
    let inner = block.inner(content);
    frame.render_widget(block, content);
    let rows = pages::rows(dashboard, text, selected, install_page);
    if !rows.is_empty() {
        crate::table::draw(frame, inner, rows, text, scroll, focused);
    }
    let footer_items = footer_items(install_page, selected, text);
    let footer = if install_page.is_none() && matches!(selected, 0 | 1 | 2 | 5) {
        let nav_width = footer_items
            .iter()
            .map(|(label, _)| Span::raw(label.as_str()).width() + 2)
            .sum::<usize>();
        let action_items = action_items(selected, dashboard.is_some(), text);
        let action_width = action_items
            .iter()
            .map(|(label, _)| Span::raw(label.as_str()).width() + 2)
            .sum::<usize>();
        footer_items
            .into_iter()
            .flat_map(|(label, _)| {
                footer_spans(&label, Color::Rgb(225, 230, 234), Color::Rgb(175, 175, 175))
            })
            .chain(std::iter::once(Span::raw(" ".repeat(
                usize::from(areas[3].width).saturating_sub(nav_width + action_width),
            ))))
            .chain(action_items.into_iter().flat_map(|(label, _)| {
                footer_spans(&label, Color::Rgb(220, 240, 242), Color::Cyan)
            }))
            .collect::<Vec<_>>()
    } else {
        footer_items
            .into_iter()
            .flat_map(|(label, _)| footer_spans(&label, Color::Rgb(220, 240, 242), Color::Cyan))
            .collect::<Vec<_>>()
    };
    frame.render_widget(
        Paragraph::new(Line::from(footer))
            .alignment(Alignment::Center)
            .style(Style::default().bg(Color::Rgb(32, 35, 39))),
        areas[3],
    );
}

fn process_counter(count: usize, text: &Text) -> Span<'static> {
    Span::raw(format!("[{count}] {PROCESS_KEY} {}", text.run_processes))
}

pub(crate) fn process_counter_at(column: u16, width: u16, count: usize, text: &Text) -> bool {
    (8..width.min(8 + process_counter(count, text).width() as u16)).contains(&column)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_counter_advertises_shortcut_and_matches_click_area() {
        let text = Text {
            run_processes: "进程管理".into(),
            ..Text::default()
        };
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 10)).unwrap();
        terminal
            .draw(|frame| draw(frame, None, &text, 0, None, &mut 0, None, Some(0)))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(12, 0)].symbol(), "p");
        assert_eq!(buffer[(20, 0)].symbol(), "理");
        assert!(process_counter_at(21, 80, 0, &text));
        assert!(!process_counter_at(22, 80, 0, &text));
    }

    #[test]
    fn footer_hit_testing_uses_rendered_width_for_chinese_labels() {
        let text = Text {
            footer_select: "↑↓ 选择".into(),
            footer_open: "Enter 打开".into(),
            footer_tabs: "←→ 切页".into(),
            footer_scroll_up: "PgUp ↑".into(),
            footer_scroll_down: "PgDn ↓".into(),
            footer_refresh: "r 刷新".into(),
            footer_build: "b 编译".into(),
            footer_rebuild: "B 重新编译".into(),
            footer_apply: "a 准备环境".into(),
            footer_uninstall: "u 卸载".into(),
            footer_check: "c 检查".into(),
            footer_verify: "v 验证".into(),
            footer_deps_check: "d 检查依赖".into(),
            footer_deps_install: "D 安装依赖".into(),
            footer_create_project: "n 创建项目".into(),
            footer_scan_projects: "s 扫描项目".into(),
            footer_rescan_projects: "r 重新扫描项目".into(),
            footer_quit: "q 退出".into(),
            ..Text::default()
        };
        let items = footer_items(None, 3, &text);
        let total = items
            .iter()
            .map(|(label, _)| Span::raw(label.as_str()).width() + 2)
            .sum::<usize>();
        let start = (100 - total) / 2;
        assert_eq!(
            footer_action_at(start as u16 + 1, 100, None, 3, false, &text),
            Some(FooterAction::Select)
        );
        assert_eq!(
            footer_action_at((start + total) as u16, 100, None, 3, false, &text),
            None
        );
        let build_width = action_items(2, false, &text)
            .iter()
            .map(|(label, _)| Span::raw(label.as_str()).width() + 2)
            .sum::<usize>();
        assert_eq!(
            footer_action_at((100 - build_width) as u16 + 1, 100, None, 2, false, &text),
            Some(FooterAction::Build)
        );
        let install_width = action_items(1, false, &text)
            .iter()
            .map(|(label, _)| Span::raw(label.as_str()).width() + 2)
            .sum::<usize>();
        assert_eq!(
            footer_action_at((100 - install_width) as u16 + 1, 100, None, 1, false, &text),
            Some(FooterAction::Execute(Action::Apply))
        );
        let project_width = action_items(0, false, &text)
            .iter()
            .map(|(label, _)| Span::raw(label.as_str()).width() + 2)
            .sum::<usize>();
        assert_eq!(
            footer_action_at((100 - project_width) as u16 + 1, 100, None, 0, false, &text),
            Some(FooterAction::CreateProject)
        );
        assert_eq!(
            action_items(0, true, &text)
                .iter()
                .map(|(_, action)| *action)
                .collect::<Vec<_>>(),
            [FooterAction::RescanProjects]
        );
        assert_eq!(
            footer_action_at(1, 100, None, 2, false, &text),
            Some(FooterAction::Select)
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 10)).unwrap();
        terminal
            .draw(|frame| draw(frame, None, &text, 2, None, &mut 0, None, None))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer()[((100 - build_width) as u16, 9)].bg,
            Color::Rgb(220, 240, 242)
        );
        assert_eq!(
            terminal.backend().buffer()[((100 - build_width + 3) as u16, 9)].bg,
            Color::Cyan
        );
    }
}
