//! Discovery of runnable ROS packages, nodes and launch files.
//!
//! This module deliberately only inspects a workspace.  Starting a process is
//! a frontend concern, while the discovered paths are useful to the CLI and
//! other frontends (including the TUI).

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::package_xml::{find_package_files, PackageXml};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Node,
    Launch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Target {
    pub kind: TargetKind,
    pub package: String,
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct WorkspaceRun {
    pub workspace: PathBuf,
    pub built: bool,
    pub targets: Vec<Target>,
}

/// Return `package/name` choices matching a partially typed target name.
pub fn complete_targets(targets: &[Target], prefix: &str) -> Vec<String> {
    let mut choices: Vec<String> = targets
        .iter()
        .map(|t| format!("{}/{}", t.package, t.name))
        .filter(|name| name.starts_with(prefix) || name.split('/').next_back() == Some(prefix))
        .collect();
    choices.sort();
    choices.dedup();
    choices
}

/// Scan one colcon workspace.  Nodes come from installed package executables;
/// launch files come from the source package's `launch` directory.  This keeps
/// discovery deterministic and avoids presenting build helper files as nodes.
pub fn scan_workspace(workspace: &Path) -> WorkspaceRun {
    let install = workspace.join("install");
    let built = install.join("setup.bash").is_file() || install.join("local_setup.bash").is_file();
    let mut result = WorkspaceRun {
        workspace: workspace.to_path_buf(),
        built,
        targets: Vec::new(),
    };
    let source_root = workspace.join("src");
    let scan_root = if source_root.is_dir() {
        source_root
    } else {
        workspace.to_path_buf()
    };
    for package_file in find_package_files(&scan_root) {
        let Ok(package) = PackageXml::load(&package_file) else {
            continue;
        };
        scan_package(&mut result, &package, &install);
    }
    result.targets.sort_by(|a, b| {
        (a.package.as_str(), a.kind as u8, a.name.as_str()).cmp(&(
            b.package.as_str(),
            b.kind as u8,
            b.name.as_str(),
        ))
    });
    result
}

fn scan_package(result: &mut WorkspaceRun, package: &PackageXml, install: &Path) {
    let node_dir = install.join(&package.name).join("lib").join(&package.name);
    if let Ok(entries) = fs::read_dir(&node_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && executable(&path) {
                if let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) {
                    result.targets.push(Target {
                        kind: TargetKind::Node,
                        package: package.name.clone(),
                        name,
                        path,
                    });
                }
            }
        }
    }
    let source_launch_dir = package.dir.join("launch");
    let installed_launch_dir = install.join("share").join(&package.name).join("launch");
    let launch_dir = if source_launch_dir.is_dir() {
        source_launch_dir
    } else {
        installed_launch_dir
    };
    if let Ok(entries) = fs::read_dir(launch_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let extension = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default();
            if path.is_file() && matches!(extension, "py" | "xml" | "yaml" | "yml") {
                if let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) {
                    result.targets.push(Target {
                        kind: TargetKind::Launch,
                        package: package.name.clone(),
                        name,
                        path,
                    });
                }
            }
        }
    }
}

fn executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn discovers_installed_nodes_and_launch_files() {
        let root = std::env::temp_dir().join(format!("rospm-run-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src/demo/launch")).unwrap();
        fs::write(
            root.join("src/demo/package.xml"),
            "<package><name>demo</name></package>",
        )
        .unwrap();
        fs::write(root.join("src/demo/launch/start.py"), "").unwrap();
        let node = root.join("install/demo/lib/demo/talker");
        fs::create_dir_all(node.parent().unwrap()).unwrap();
        fs::write(&node, "").unwrap();
        let mut p = fs::metadata(&node).unwrap().permissions();
        p.set_mode(0o755);
        fs::set_permissions(&node, p).unwrap();
        fs::write(root.join("install/setup.bash"), "").unwrap();
        let found = scan_workspace(&root);
        assert!(found.built);
        assert!(found.targets.iter().any(|t| t.name == "talker"));
        assert!(found.targets.iter().any(|t| t.name == "start.py"));
        let _ = fs::remove_dir_all(root);
    }
}
