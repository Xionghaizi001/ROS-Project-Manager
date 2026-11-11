//! Page data stays independent of terminal drawing and column widths.
use crate::{dashboard::Dashboard, Text};
use rospm_core::probe::Status;

pub(crate) struct Entry {
    pub cells: [String; 3],
    pub status: Option<Status>,
    pub heading: bool,
    /// Set only when the executor identifies this exact dependency/package row.
    pub running: bool,
}
impl Entry {
    fn value(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            cells: [name.into(), String::new(), value.into()],
            status: None,
            heading: false,
            running: false,
        }
    }
}

pub(crate) fn rows(
    dashboard: Option<&Dashboard>,
    text: &Text,
    tab: usize,
    ros_page: bool,
) -> Vec<Entry> {
    let mut rows = Vec::new();
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
                    running: false,
                });
            }
            for error in &d.check {
                rows.push(Entry::value(&text.status, error));
            }
        }
        1 if ros_page => {
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
            rows.push(Entry::value(&text.logs, &text.open_logs));
        }
        1 => {
            rows.push(Entry::value("› ROS", "Enter ›"));
            for name in ["Conda", &text.components] {
                let mut row = Entry::value(name, if name == "Conda" { &d.conda } else { "" });
                row.cells[1] = text.not_connected.clone();
                rows.push(row);
            }
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
                    running: false,
                });
            }
            rows.push(Entry::value(&text.logs, &text.open_logs));
        }
        2 => {
            heading(&mut rows, &text.build_order);
            for package in &d.compile_packages {
                rows.push(Entry::value(&package.name, &package.detail));
            }
            rows.push(Entry::value(&text.logs, &text.open_logs));
        }
        5 => {
            section(&mut rows, &text.verify_result, &d.verify);
            section(&mut rows, &text.diagnostics, &d.diagnostics);
            rows.push(Entry::value(&text.logs, &text.open_logs));
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
            workspaces: vec![],
            conda: "system".into(),
            apply_plan: vec!["install ROS".into()],
            uninstall_plan: vec!["remove ROS".into()],
            check: vec![],
            report: Default::default(),
            verify: vec![],
            deps: vec![],
            deps_report: Default::default(),
            ros_installed: false,
            diagnostics: vec![],
            compile_packages: vec![],
            root: ".".into(),
        };
        let text = Text {
            not_installed: "missing".into(),
            status_ok: "ok".into(),
            ..Text::default()
        };
        let parent = rows(Some(&dashboard), &text, 1, false);
        assert_eq!(parent[0].cells, ["› ROS", "", "Enter ›"]);
        assert!(!parent.iter().any(|row| row.cells[2] == "install ROS"));
        let child = rows(Some(&dashboard), &text, 1, true);
        assert_eq!(child[0].cells[2], "missing");
        assert!(child.iter().any(|row| row.cells[2] == "install ROS"));
        assert!(!child.iter().any(|row| row.cells[2] == "remove ROS"));
        dashboard.ros_installed = true;
        let child = rows(Some(&dashboard), &text, 1, true);
        assert_eq!(child[0].cells[2], "ok");
        assert!(child.iter().any(|row| row.cells[2] == "remove ROS"));
        assert!(rows(None, &text, 1, false).is_empty());
    }
}
