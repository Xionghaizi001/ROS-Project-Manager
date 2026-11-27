//! Page data stays independent of terminal drawing and column widths.
use crate::{dashboard::Dashboard, InstallPage, Text, DEBUG_TAB, DEBUG_TOOLS, HELP_TAB};
use rospm_core::{conda::ToolInstallation, manifest::ProviderKind, probe::Status, run::TargetKind};

pub(crate) struct Entry {
    pub cells: [String; 3],
    pub status: Option<Status>,
    pub heading: bool,
    pub selectable: bool,
    pub disabled: bool,
    /// Set only when the executor identifies this exact dependency/package row.
    pub running: bool,
    pub detail: Option<String>,
    pub run_target: Option<usize>,
}
impl Entry {
    pub(crate) fn value(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            cells: [name.into(), String::new(), value.into()],
            status: None,
            heading: false,
            selectable: false,
            disabled: false,
            running: false,
            detail: None,
            run_target: None,
        }
    }

    pub(crate) fn link(name: impl Into<String>) -> Self {
        let mut entry = Self::value(name, "");
        entry.selectable = true;
        entry
    }
}

pub(crate) fn rows(
    dashboard: Option<&Dashboard>,
    text: &Text,
    tab: usize,
    install_page: Option<&InstallPage>,
) -> Vec<Entry> {
    if matches!(install_page, Some(InstallPage::CliHelp)) {
        return vec![Entry::value("CLI", &text.cli_usage)];
    }
    if tab == HELP_TAB && install_page.is_none() {
        return vec![
            Entry::link(&text.cli_help),
            Entry::value("TUI", &text.tui_usage),
        ];
    }
    if let Some(InstallPage::Detail { title, body, .. }) = install_page {
        return vec![Entry::value(title, body)];
    }
    if let Some(InstallPage::Run(page)) = install_page {
        return page.rows(text);
    }
    if let Some(InstallPage::DebugToolMissing { package, .. }) = install_page {
        let mut row = Entry::link(&text.debug_install);
        row.cells[2].clone_from(package);
        return vec![row];
    }
    if tab == DEBUG_TAB {
        return if let Some(dashboard) = dashboard.filter(|d| d.provider == ProviderKind::Native) {
            DEBUG_TOOLS
                .into_iter()
                .zip(&text.debug_tools)
                .enumerate()
                .map(|(index, (tool, description))| {
                    let mut row = Entry::link(tool);
                    row.cells[2].clone_from(description);
                    if let Some(statuses) = &dashboard.debug_tools {
                        let status = statuses[index];
                        row.cells[1] = match status {
                            Status::Ok => &text.status_ok,
                            Status::Missing => &text.not_installed,
                            _ => &text.status_unknown,
                        }
                        .clone();
                        row.status = Some(status);
                    }
                    row
                })
                .collect()
        } else {
            vec![Entry::value(&text.tabs[DEBUG_TAB], &text.debug_no_project)]
        };
    }
    if let Some(InstallPage::ScannedProjects(projects)) = install_page {
        return if projects.is_empty() {
            vec![Entry::value(
                &text.project_scan_title,
                &text.no_projects_found,
            )]
        } else {
            projects
                .iter()
                .map(|path| {
                    let mut row = Entry::link(path.parent().unwrap_or(path).display().to_string());
                    row.cells[2] = path.display().to_string();
                    row
                })
                .collect()
        };
    }
    if let Some(InstallPage::Conda {
        tool,
        action,
        notice,
    }) = install_page
    {
        let mut row = Entry::value(&text.conda, tool.render());
        let (label, status) = if matches!(tool, ToolInstallation::Ready(_)) {
            (&text.status_ok, Status::Ok)
        } else {
            (&text.not_installed, Status::Missing)
        };
        row.cells[1] = label.clone();
        row.status = Some(status);
        let mut rows = vec![row];
        if let Some(action) = action {
            rows.push(Entry::link(action.label().to_string()));
        }
        if let Some(notice) = notice {
            rows.push(Entry::value(&text.status, notice));
        }
        return rows;
    }
    if matches!(install_page, Some(InstallPage::Components)) {
        let Some(dashboard) = dashboard else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        for component in &dashboard.components {
            let status = match component.status {
                Status::Ok => "✓".to_owned(),
                Status::Missing => format!("× {}", text.not_installed),
                Status::Mismatch => format!("× {}", text.status_mismatch),
                Status::Unknown => format!("? {}", text.status_unknown),
                Status::Info => text.status_info.clone(),
            };
            rows.push(Entry {
                cells: [
                    format!("{} {}", component.kind, component.name),
                    status,
                    component.actual.clone(),
                ],
                status: Some(component.status),
                heading: false,
                selectable: false,
                disabled: false,
                running: false,
                detail: None,
                run_target: None,
            });
        }
        if rows.is_empty() {
            rows.push(Entry::value(&text.components, "—"));
        }
        return rows;
    }
    let mut rows = Vec::new();
    if tab == 1 && install_page.is_none() {
        rows.push(if dashboard.is_some() {
            Entry::link("ROS")
        } else {
            Entry::value("ROS", &text.manifest_search)
        });
        let mut conda = Entry::link(&text.conda);
        if let Some(dashboard) = dashboard {
            conda.cells[2].clone_from(&dashboard.conda);
        }
        rows.push(conda);
        let mut components = Entry::link(&text.components);
        if dashboard.is_some_and(|d| d.ros_installed) {
            components.cells[1].clone_from(&text.status_ok);
        } else {
            components.selectable = false;
            components.disabled = true;
        }
        rows.push(components);
    }
    let Some(d) = dashboard else {
        if tab == 0 {
            rows.push(Entry::value(&text.manifest_label, &text.manifest_search));
        }
        return rows;
    };
    match tab {
        0 => {
            heading(&mut rows, &text.details);
            rows.push(Entry::value(
                &text.status,
                if d.installing {
                    &text.installing
                } else {
                    &text.ready
                },
            ));
            rows.extend([
                Entry::value(&text.project, &d.project),
                Entry::value(&text.manifest_label, d.manifest.display().to_string()),
                Entry::value(&text.provider_label, d.provider.to_string()),
                Entry::value(&text.root_label, d.root.display().to_string()),
            ]);
            heading(&mut rows, &text.check_result);
            for item in &d.report.items {
                let status = match item.status {
                    Status::Ok => "✓",
                    Status::Missing => &text.missing,
                    Status::Mismatch => &text.status_mismatch,
                    Status::Unknown => &text.status_unknown,
                    Status::Info => &text.status_info,
                };
                rows.push(Entry {
                    cells: [
                        item.subject.to_string(),
                        status.into(),
                        item.actual.to_string(),
                    ],
                    status: Some(item.status),
                    heading: false,
                    selectable: false,
                    disabled: false,
                    running: false,
                    detail: None,
                    run_target: None,
                });
            }
            for error in &d.check {
                rows.push(Entry::value(&text.status, error));
            }
        }
        1 if matches!(install_page, Some(InstallPage::Ros)) => {
            rows.push(Entry::value(
                "ROS",
                if d.ros_installed {
                    &text.status_ok
                } else {
                    &text.not_installed
                },
            ));
            section(&mut rows, &text.apply_plan, &d.apply_plan);
            if d.ros_installed {
                section(&mut rows, &text.uninstall_plan, &d.uninstall_plan);
            }
        }
        1 => {
            section(&mut rows, &text.deps_result, &d.deps);
            for item in &d.deps_report.items {
                let status = match item.status {
                    Status::Ok => "✓".to_owned(),
                    Status::Missing => format!("× {}", text.not_installed),
                    Status::Mismatch => format!("× {}", text.status_mismatch),
                    Status::Unknown => format!("? {}", text.status_unknown),
                    Status::Info => text.status_info.clone(),
                };
                rows.push(Entry {
                    cells: [item.subject.to_string(), status, item.actual.to_string()],
                    status: Some(item.status),
                    heading: false,
                    selectable: false,
                    disabled: false,
                    running: false,
                    detail: None,
                    run_target: None,
                });
            }
        }
        2 => {
            heading(&mut rows, &text.build_order);
            for package in &d.compile_packages {
                rows.push(Entry::value(&package.name, &package.detail));
            }
        }
        3 => {
            rows.push(Entry::link("Shell"));
            if !d.run.built || d.run.targets.is_empty() {
                rows.push(Entry::value(
                    &text.tabs[3],
                    if d.run.built {
                        &text.run_empty
                    } else {
                        &text.run_not_built
                    },
                ));
            } else {
                for (kind, label) in [
                    (TargetKind::Node, &text.run_node),
                    (TargetKind::Launch, &text.run_launch),
                ] {
                    let mut targets = d
                        .run
                        .targets
                        .iter()
                        .enumerate()
                        .filter(|(_, target)| target.kind == kind)
                        .peekable();
                    if targets.peek().is_none() {
                        continue;
                    }
                    heading(&mut rows, label);
                    for (index, target) in targets {
                        let mut row = Entry::link(format!("{}/{}", target.package, target.name));
                        row.run_target = Some(index);
                        rows.push(row);
                    }
                }
            }
        }
        5 => {
            section(&mut rows, &text.verify_result, &d.verify);
            section(&mut rows, &text.diagnostics, &d.diagnostics);
        }
        _ => {}
    }
    rows
}
fn heading(rows: &mut Vec<Entry>, title: &str) {
    let mut row = Entry::value(title, "");
    row.heading = true;
    rows.push(row);
}
fn section(rows: &mut Vec<Entry>, title: &str, values: &[String]) {
    let mut row = Entry::link(title);
    row.heading = true;
    row.cells[2] = if values.is_empty() {
        "—".into()
    } else {
        format!("{} 项", values.len())
    };
    row.detail = Some(if values.is_empty() {
        "—".into()
    } else {
        values.join("\n")
    });
    rows.push(row);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ros_entry_opens_a_separate_plan_page() {
        let mut dashboard = Dashboard {
            manifest: "rospm.toml".into(),
            project: "test".into(),
            provider: Default::default(),
            conda: "system".into(),
            apply_plan: vec!["install ROS".into()],
            uninstall_plan: vec!["remove ROS".into()],
            check: vec![],
            report: Default::default(),
            verify: vec![],
            deps: vec![],
            deps_report: Default::default(),
            components: vec![],
            ros_installed: false,
            debug_tools: None,
            diagnostics: vec![],
            compile_packages: vec![],
            root: ".".into(),
            run: Default::default(),
            installing: false,
        };
        let text = Text {
            conda: "Conda".into(),
            apply_plan: "apply plan".into(),
            uninstall_plan: "uninstall plan".into(),
            logs: "logs".into(),
            not_installed: "missing".into(),
            status_ok: "ok".into(),
            ..Text::default()
        };
        let parent = rows(Some(&dashboard), &text, 1, None);
        assert_eq!(parent[0].cells, ["ROS", "", ""]);
        assert!(parent[0].selectable);
        assert!(!parent.iter().any(|row| row.cells[2] == "install ROS"));
        let child = rows(Some(&dashboard), &text, 1, Some(&InstallPage::Ros));
        assert_eq!(child[0].cells[2], "missing");
        assert!(!child[0].selectable);
        assert!(child.iter().any(|row| row.cells[0] == "apply plan"
            && row.selectable
            && row.heading
            && row.detail.as_deref() == Some("install ROS")));
        assert!(!child
            .iter()
            .any(|row| row.detail.as_deref() == Some("remove ROS")));
        assert!(!child.iter().any(|row| row.cells[0] == text.logs));
        dashboard.ros_installed = true;
        let child = rows(Some(&dashboard), &text, 1, Some(&InstallPage::Ros));
        assert_eq!(child[0].cells[2], "ok");
        assert!(child
            .iter()
            .any(|row| row.detail.as_deref() == Some("remove ROS")));
        let parent = rows(None, &text, 1, None);
        assert!(!parent[0].selectable);
        assert_eq!(parent[1].cells[0], "Conda");
        assert!(parent[1].selectable);
    }

    #[test]
    fn component_entry_requires_ros_and_lists_scanned_packages() {
        let mut dashboard = Dashboard {
            manifest: "rospm.toml".into(),
            project: "test".into(),
            provider: Default::default(),
            conda: "system".into(),
            apply_plan: vec![],
            uninstall_plan: vec![],
            check: vec![],
            report: Default::default(),
            verify: vec![],
            deps: vec![],
            deps_report: Default::default(),
            components: vec![crate::dashboard::Component {
                kind: "deb",
                name: "libfoo-dev".into(),
                actual: "not installed".into(),
                status: Status::Missing,
            }],
            ros_installed: false,
            debug_tools: None,
            diagnostics: vec![],
            compile_packages: vec![],
            root: ".".into(),
            run: Default::default(),
            installing: false,
        };
        let text = Text {
            components: "components".into(),
            not_installed: "missing".into(),
            status_missing: "missing".into(),
            status_unknown: "unknown".into(),
            status_mismatch: "mismatch".into(),
            status_info: "info".into(),
            ..Text::default()
        };
        let parent = rows(Some(&dashboard), &text, 1, None);
        assert_eq!(parent[2].cells, ["components", "", ""]);
        assert!(!parent[2].selectable);
        assert!(parent[2].disabled);
        assert!(rows(None, &text, 1, None)[2].disabled);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 12)).unwrap();
        terminal
            .draw(|frame| crate::table::draw(frame, frame.area(), parent, &text, &mut 0, None))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert!((0..80).any(|x| {
            let cell = &buffer[(x, 3)];
            cell.symbol() == "c" && cell.fg == ratatui::style::Color::DarkGray
        }));
        dashboard.ros_installed = true;
        let parent = rows(Some(&dashboard), &text, 1, None);
        assert!(parent[2].selectable);
        assert!(!parent[2].disabled);
        let child = rows(Some(&dashboard), &text, 1, Some(&InstallPage::Components));
        assert_eq!(child[0].cells[0], "deb libfoo-dev");
        assert!(child[0].cells[1].contains("missing"));
    }
}
