//! Page data stays independent of terminal drawing and column widths.
use crate::{dashboard::Dashboard, InstallPage, Text};
use rospm_core::{conda::ToolInstallation, probe::Status};

pub(crate) struct Entry {
    pub cells: [String; 3],
    pub status: Option<Status>,
    pub heading: bool,
    pub selectable: bool,
    /// Set only when the executor identifies this exact dependency/package row.
    pub running: bool,
}
impl Entry {
    fn value(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            cells: [name.into(), String::new(), value.into()],
            status: None,
            heading: false,
            selectable: false,
            running: false,
        }
    }

    fn link(name: impl Into<String>) -> Self {
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
    if let Some(InstallPage::Conda(tool)) = install_page {
        let mut row = Entry::value(&text.conda, tool.render());
        let (label, status) = if matches!(tool, ToolInstallation::Ready(_)) {
            (&text.status_ok, Status::Ok)
        } else {
            (&text.not_installed, Status::Missing)
        };
        row.cells[1] = label.clone();
        row.status = Some(status);
        return vec![row];
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
                running: false,
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
                    running: false,
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
            rows.push(Entry::link(&text.logs));
        }
        1 => {
            let ros_installed = dashboard.is_some_and(|d| d.ros_installed);
            let mut components = Entry::value(&text.components, "");
            components.cells[1] = if ros_installed {
                text.status_ok.clone()
            } else {
                text.not_connected.clone()
            };
            components.selectable = ros_installed;
            rows.push(components);
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
                    running: false,
                });
            }
            rows.push(Entry::link(&text.logs));
        }
        2 => {
            heading(&mut rows, &text.build_order);
            for package in &d.compile_packages {
                rows.push(Entry::value(&package.name, &package.detail));
            }
            rows.push(Entry::link(&text.logs));
        }
        5 => {
            section(&mut rows, &text.verify_result, &d.verify);
            section(&mut rows, &text.diagnostics, &d.diagnostics);
            rows.push(Entry::link(&text.logs));
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
    heading(rows, title);
    for (i, value) in values.iter().enumerate() {
        rows.push(Entry::value((i + 1).to_string(), value));
    }
    if values.is_empty() {
        rows.push(Entry::value("", "—"));
    }
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
            diagnostics: vec![],
            compile_packages: vec![],
            root: ".".into(),
        };
        let text = Text {
            conda: "Conda".into(),
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
        assert!(child.iter().any(|row| row.cells[2] == "install ROS"));
        assert!(!child.iter().any(|row| row.cells[2] == "remove ROS"));
        assert!(child.last().is_some_and(|row| row.selectable));
        dashboard.ros_installed = true;
        let child = rows(Some(&dashboard), &text, 1, Some(&InstallPage::Ros));
        assert_eq!(child[0].cells[2], "ok");
        assert!(child.iter().any(|row| row.cells[2] == "remove ROS"));
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
            diagnostics: vec![],
            compile_packages: vec![],
            root: ".".into(),
        };
        let text = Text {
            components: "components".into(),
            not_connected: "not connected".into(),
            not_installed: "missing".into(),
            status_missing: "missing".into(),
            status_unknown: "unknown".into(),
            status_mismatch: "mismatch".into(),
            status_info: "info".into(),
            ..Text::default()
        };
        let parent = rows(Some(&dashboard), &text, 1, None);
        assert!(!parent[2].selectable);
        dashboard.ros_installed = true;
        let parent = rows(Some(&dashboard), &text, 1, None);
        assert!(parent[2].selectable);
        let child = rows(Some(&dashboard), &text, 1, Some(&InstallPage::Components));
        assert_eq!(child[0].cells[0], "deb libfoo-dev");
        assert!(child[0].cells[1].contains("missing"));
    }
}
