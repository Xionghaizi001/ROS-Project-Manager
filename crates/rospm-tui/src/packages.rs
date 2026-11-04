//! Read-only package ordering delegated to colcon, in manifest workspace order.
use rospm_core::manifest::Manifest;
use std::{path::Path, process::Command};

pub(crate) struct BuildPackage {
    pub name: String,
    pub detail: String,
}

pub(crate) fn load(manifest: &Manifest, root: &Path) -> Vec<BuildPackage> {
    let mut rows = Vec::new();
    let order = match manifest.build_order() {
        Ok(order) => order,
        Err(error) => {
            return vec![BuildPackage {
                name: "!".into(),
                detail: error.to_string(),
            }]
        }
    };
    for workspace in order {
        let ws = &manifest.workspaces[&workspace];
        let cwd = root.join(&ws.path);
        let mut command = Command::new("colcon");
        command
            .args(["list", "--topological-order"])
            .current_dir(&cwd);
        if let Some(defaults) = &ws.colcon.defaults {
            command.env(
                "COLCON_DEFAULTS_FILE",
                root.join(defaults)
                    .canonicalize()
                    .unwrap_or_else(|_| root.join(defaults)),
            );
        }
        match command.output() {
            Ok(output) if output.status.success() => {
                for line in String::from_utf8_lossy(&output.stdout).lines() {
                    let mut parts = line.split('\t');
                    let Some(name) = parts.next().filter(|s| !s.is_empty()) else {
                        continue;
                    };
                    let path = parts.next().unwrap_or_default();
                    rows.push(BuildPackage {
                        name: name.into(),
                        detail: format!("{} · {workspace} · {path}", rows.len() + 1),
                    });
                }
            }
            Ok(output) => rows.push(BuildPackage {
                name: workspace,
                detail: format!(
                    "colcon list: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            }),
            Err(error) => rows.push(BuildPackage {
                name: workspace,
                detail: format!("colcon list: {error}"),
            }),
        }
    }
    rows
}
