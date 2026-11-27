use std::path::PathBuf;

use anyhow::{Context, Result};
use rospm_core::{
    conda::{PathAction, ToolInstallation},
    worker::WorkspaceLock,
};

use crate::actions::PendingConfirmation;
use crate::{dashboard, pages, runs, table, view, Text, DEBUG_TOOLS};

pub(super) const DEBUG_TAB: usize = 4;
pub(super) const HELP_TAB: usize = 6;

pub(super) enum InstallPage {
    Ros,
    Conda {
        tool: ToolInstallation,
        action: Option<PathAction>,
        notice: Option<String>,
    },
    Components,
    Detail {
        title: String,
        body: String,
        return_to_ros: bool,
    },
    ScannedProjects(Vec<std::path::PathBuf>),
    Run(runs::Page),
    DebugTool(&'static str),
    DebugToolMissing {
        tool: &'static str,
        package: String,
        install: bool,
    },
    Shell,
    CliHelp,
}

impl InstallPage {
    pub(super) fn can_install(&self) -> bool {
        matches!(
            self,
            Self::Ros
                | Self::Components
                | Self::Conda {
                    tool: ToolInstallation::Needed(_),
                    ..
                }
        )
    }

    pub(super) fn refresh(&mut self) {
        if matches!(self, Self::Conda { .. }) {
            *self = Self::conda();
        } else if let Self::ScannedProjects(projects) = self {
            if let Ok(root) = std::env::current_dir() {
                *projects = dashboard::Dashboard::scan_projects(&root);
            }
        }
    }

    fn conda() -> Self {
        let tool = ToolInstallation::probe();
        let (action, notice) = match PathAction::available() {
            Ok(action) => (action, None),
            Err(error) => (None, Some(error.to_string())),
        };
        Self::Conda {
            tool,
            action,
            notice,
        }
    }
}

pub(super) fn lock_project(project: &dashboard::Dashboard, text: &Text) -> Result<WorkspaceLock> {
    WorkspaceLock::try_acquire_tui(&project.root)?
        .with_context(|| format!("{} ({})", text.tui_already_running, project.root.display()))
}

#[allow(clippy::too_many_arguments)] // Switching projects updates the lock and navigation state together.
pub(super) fn open_project(
    path: PathBuf,
    text: &Text,
    active_manifest: &mut Option<PathBuf>,
    dashboard: &mut Option<dashboard::Dashboard>,
    project_lock: &mut Option<WorkspaceLock>,
    install_page: &mut Option<InstallPage>,
    focused: &mut Option<usize>,
    scroll: &mut usize,
) -> Result<()> {
    if let Some(project) = dashboard::Dashboard::load(Some(&path), text)? {
        match lock_project(&project, text) {
            Ok(lease) => {
                *active_manifest = Some(path);
                *dashboard = Some(project);
                *project_lock = Some(lease);
                *install_page = None;
            }
            Err(error) => {
                *install_page = Some(InstallPage::Detail {
                    title: text.project_scan_title.clone(),
                    body: format!("{error:#}"),
                    return_to_ros: false,
                });
            }
        }
        *focused = first_selectable(dashboard.as_ref(), text, 0, install_page.as_ref());
        *scroll = 0;
    }
    Ok(())
}

pub(super) fn back_run(
    install_page: &mut Option<InstallPage>,
    focused: &mut Option<usize>,
    scroll: &mut usize,
    text: &Text,
) -> bool {
    if let Some(InstallPage::Run(page)) = install_page {
        if page.back() {
            *focused = page.rows(text).iter().position(|row| row.selectable);
            *scroll = 0;
            return true;
        }
    }
    false
}

pub(super) fn first_selectable(
    dashboard: Option<&dashboard::Dashboard>,
    text: &Text,
    tab: usize,
    install_page: Option<&InstallPage>,
) -> Option<usize> {
    pages::rows(dashboard, text, tab, install_page)
        .iter()
        .position(|row| row.selectable)
}

#[allow(clippy::too_many_arguments)] // Focus and scrolling use the current page and viewport together.
pub(super) fn move_focus(
    focused: &mut Option<usize>,
    scroll: &mut usize,
    dashboard: Option<&dashboard::Dashboard>,
    text: &Text,
    tab: usize,
    install_page: Option<&InstallPage>,
    size: ratatui::layout::Size,
    forward: bool,
) -> bool {
    let rows = pages::rows(dashboard, text, tab, install_page);
    let selectable: Vec<_> = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| row.selectable.then_some(index))
        .collect();
    let Some(current) = focused.and_then(|index| selectable.iter().position(|&i| i == index))
    else {
        *focused = selectable.first().copied();
        *scroll = if forward { 0 } else { usize::MAX };
        return !selectable.is_empty();
    };
    let next = if forward {
        (current + 1) % selectable.len()
    } else {
        (current + selectable.len() - 1) % selectable.len()
    };
    let index = selectable[next];
    *focused = Some(index);
    if (matches!(tab, 3 | DEBUG_TAB) && install_page.is_none()
        || matches!(install_page, Some(InstallPage::Run(page)) if page.is_processes()))
        && next != 0
    {
        let area = view::table_area(ratatui::layout::Rect::new(0, 0, size.width, size.height));
        table::reveal_row(area, &rows, text, scroll, index);
    } else {
        *scroll = if next == 0 { 0 } else { usize::MAX };
    }
    true
}

pub(super) fn activate(
    install_page: &mut Option<InstallPage>,
    focused: &mut Option<usize>,
    scroll: &mut usize,
    dashboard: Option<&dashboard::Dashboard>,
    text: &Text,
    selected: usize,
    confirmation: &mut Option<PendingConfirmation>,
) {
    let rows = pages::rows(dashboard, text, selected, install_page.as_ref());
    let Some(index) = *focused else {
        return;
    };
    let Some(row) = rows.get(index).filter(|row| row.selectable) else {
        return;
    };
    if let Some(InstallPage::Conda {
        action: Some(action),
        ..
    }) = install_page
    {
        let message = match action.perform() {
            Ok(message) => message.to_string(),
            Err(error) => error.to_string(),
        };
        let mut page = InstallPage::conda();
        if let InstallPage::Conda { notice, .. } = &mut page {
            *notice = Some(message);
        }
        *install_page = Some(page);
        *focused = first_selectable(dashboard, text, selected, install_page.as_ref());
        return;
    }
    if let Some(InstallPage::DebugToolMissing { package, .. }) = install_page {
        *confirmation = Some(PendingConfirmation::InstallDebugTool {
            package: package.clone(),
        });
        return;
    }
    if let Some(InstallPage::Run(page)) = install_page {
        *focused = page.activate(index);
        *scroll = 0;
        return;
    }
    if selected == 3 && install_page.is_none() && index == 0 {
        *install_page = Some(InstallPage::Shell);
        return;
    }
    if selected == DEBUG_TAB && install_page.is_none() {
        *install_page = Some(InstallPage::DebugTool(DEBUG_TOOLS[index]));
        return;
    }
    if let (Some(index), Some(dashboard)) = (row.run_target, dashboard) {
        *install_page = Some(InstallPage::Run(runs::Page::form(
            dashboard.run.targets[index].clone(),
        )));
        *focused = Some(0);
        *scroll = 0;
        return;
    }
    if let Some(body) = &row.detail {
        *install_page = Some(InstallPage::Detail {
            title: row.cells[0].clone(),
            body: body.clone(),
            return_to_ros: matches!(install_page.as_ref(), Some(InstallPage::Ros)),
        });
        *focused = None;
        *scroll = 0;
        return;
    }
    *install_page = match (selected, install_page.as_ref(), index) {
        (1, None, 0) => Some(InstallPage::Ros),
        (1, None, 1) => Some(InstallPage::conda()),
        (1, None, 2) if dashboard.is_some_and(|d| d.ros_installed) => Some(InstallPage::Components),
        (HELP_TAB, None, 0) => Some(InstallPage::CliHelp),
        _ => return,
    };
    *focused = first_selectable(dashboard, text, selected, install_page.as_ref());
    *scroll = 0;
}

pub(super) fn tab_at(column: u16, width: u16) -> usize {
    if width == 0 {
        return 0;
    }
    (usize::from(column).saturating_mul(8) / usize::from(width)).min(7)
}

#[cfg(test)]
mod tests {
    use super::tab_at;

    #[test]
    fn help_opens_cli_guide_without_a_project() {
        use super::{activate, first_selectable, pages, InstallPage, Text, HELP_TAB};

        let text = Text {
            tui_usage: "Launch with rospm tui".into(),
            cli_help: "CLI usage".into(),
            cli_usage: "List commands with rospm --help".into(),
            ..Text::default()
        };
        let main = pages::rows(None, &text, HELP_TAB, None);
        assert_eq!(main[0].cells[0], text.cli_help);
        assert_eq!(main[1].cells[2], text.tui_usage);

        let mut focus = first_selectable(None, &text, HELP_TAB, None);
        let mut page = None;
        let mut scroll = 5;
        activate(
            &mut page,
            &mut focus,
            &mut scroll,
            None,
            &text,
            HELP_TAB,
            &mut None,
        );
        assert!(matches!(&page, Some(InstallPage::CliHelp)));
        assert_eq!(focus, None);
        assert_eq!(scroll, 0);
        let guide = pages::rows(None, &text, HELP_TAB, page.as_ref());
        assert_eq!(guide[0].cells[2], text.cli_usage);
    }

    #[test]
    fn mouse_tab_hit_testing_handles_narrow_and_wide_windows() {
        assert_eq!(tab_at(0, 80), 0);
        assert_eq!(tab_at(10, 80), 1);
        assert_eq!(tab_at(79, 80), 7);
        assert_eq!(tab_at(2, 3), 5);
        assert_eq!(tab_at(0, 0), 0);
    }
}
