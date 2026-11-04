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
    _workspace: usize,
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
        1 => {
            heading(&mut rows, &text.tabs[1]);
            for name in ["ROS", "Conda", &text.components] {
                let mut row = Entry::value(
                    name,
                    if name == "Conda" {
                        &d.conda
                    } else {
                        &text.not_connected
                    },
                );
                row.cells[1] = text.not_connected.clone();
                rows.push(row);
            }
            if d.ros_installed {
                section(&mut rows, &text.uninstall_plan, &d.uninstall_plan);
            }
            section(&mut rows, &text.apply_plan, &d.apply_plan);
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
