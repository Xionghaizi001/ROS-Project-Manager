//! Minimal `package.xml` (REP 127 / REP 140 / REP 149) reader.
//!
//! rospm needs three things from a package manifest: the package name, its
//! dependency keys and where it lives. Everything else (maintainers, licenses,
//! exports) is ignored. Dependency *resolution* stays with rosdep.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, Result};
use crate::msg;

/// Marker files that make colcon / ament / catkin skip a directory tree.
pub const IGNORE_MARKERS: &[&str] = &["COLCON_IGNORE", "AMENT_IGNORE", "CATKIN_IGNORE"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependKind {
    /// `<depend>`: build + build_export + exec.
    Depend,
    BuildDepend,
    BuildExportDepend,
    BuildtoolDepend,
    BuildtoolExportDepend,
    ExecDepend,
    TestDepend,
    DocDepend,
}

impl DependKind {
    fn from_tag(tag: &str) -> Option<DependKind> {
        Some(match tag {
            "depend" => DependKind::Depend,
            "build_depend" => DependKind::BuildDepend,
            "build_export_depend" => DependKind::BuildExportDepend,
            "buildtool_depend" => DependKind::BuildtoolDepend,
            "buildtool_export_depend" => DependKind::BuildtoolExportDepend,
            "exec_depend" | "run_depend" => DependKind::ExecDepend,
            "test_depend" => DependKind::TestDepend,
            "doc_depend" => DependKind::DocDepend,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Dependency {
    /// rosdep key (a ROS package name or a system key such as `libopencv-dev`).
    pub name: String,
    pub kind: DependKind,
    /// REP 149 `condition="..."` attribute, kept verbatim; rospm does not evaluate it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageXml {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// `format="N"` attribute; 1 when absent, as in REP 127.
    pub format: u32,
    /// Directory containing the `package.xml`.
    pub dir: PathBuf,
    pub depends: Vec<Dependency>,
}

impl PackageXml {
    pub fn parse(text: &str, path: &Path) -> Result<PackageXml> {
        let doc = roxmltree::Document::parse(text).map_err(|e| Error::PackageXml {
            path: path.to_path_buf(),
            message: msg!("package_xml.malformed", source = e),
        })?;
        let root = doc.root_element();
        if root.tag_name().name() != "package" {
            return Err(Error::PackageXml {
                path: path.to_path_buf(),
                message: msg!("package_xml.root_element", found = root.tag_name().name()),
            });
        }
        let format = root
            .attribute("format")
            .and_then(|f| f.trim().parse().ok())
            .unwrap_or(1);
        let text_of = |tag: &str| -> Option<String> {
            root.children()
                .find(|n| n.is_element() && n.tag_name().name() == tag)
                .and_then(|n| n.text())
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
        };
        let name = text_of("name").ok_or_else(|| Error::PackageXml {
            path: path.to_path_buf(),
            message: msg!("package_xml.missing_name"),
        })?;
        let mut depends = Vec::new();
        for node in root.children().filter(|n| n.is_element()) {
            if let Some(kind) = DependKind::from_tag(node.tag_name().name()) {
                if let Some(dep) = node.text().map(str::trim).filter(|t| !t.is_empty()) {
                    depends.push(Dependency {
                        name: dep.to_string(),
                        kind,
                        condition: node.attribute("condition").map(str::to_string),
                    });
                }
            }
        }
        Ok(PackageXml {
            name,
            version: text_of("version"),
            format,
            dir: path.parent().map(Path::to_path_buf).unwrap_or_default(),
            depends,
        })
    }

    pub fn load(path: &Path) -> Result<PackageXml> {
        let text = fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        PackageXml::parse(&text, path)
    }
}

/// Every `package.xml` below `dir`, using colcon's discovery rules: hidden
/// directories and trees with an ignore marker are skipped, and a directory
/// that holds a `package.xml` is not descended into further.
pub fn find_package_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(dir, 0, &mut out);
    out.sort();
    out
}

const MAX_DEPTH: usize = 12;

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH || IGNORE_MARKERS.iter().any(|m| dir.join(m).exists()) {
        return;
    }
    let manifest = dir.join("package.xml");
    if manifest.is_file() {
        out.push(manifest);
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut subdirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .filter(|p| {
            !p.file_name()
                .map(|n| n.to_string_lossy().starts_with('.'))
                .unwrap_or(true)
        })
        .collect();
    subdirs.sort();
    for sub in subdirs {
        walk(&sub, depth + 1, out);
    }
}

/// Parsed packages below `dir` plus the files that failed to parse.
#[derive(Debug, Default)]
pub struct WorkspacePackages {
    pub packages: Vec<PackageXml>,
    pub errors: Vec<(PathBuf, String)>,
}

impl WorkspacePackages {
    pub fn scan(dir: &Path) -> WorkspacePackages {
        let mut ws = WorkspacePackages::default();
        for file in find_package_files(dir) {
            match PackageXml::load(&file) {
                Ok(p) => ws.packages.push(p),
                Err(e) => ws.errors.push((file, e.to_string())),
            }
        }
        ws
    }

    pub fn names(&self) -> BTreeSet<&str> {
        self.packages.iter().map(|p| p.name.as_str()).collect()
    }

    /// Dependency keys that no package in this set provides — the keys rosdep
    /// will have to resolve. Grouped by key with the kinds that reference it.
    pub fn external_keys(&self) -> BTreeMap<String, BTreeSet<DependKind>> {
        let names = self.names();
        let mut out: BTreeMap<String, BTreeSet<DependKind>> = BTreeMap::new();
        for p in &self.packages {
            for d in &p.depends {
                if !names.contains(d.name.as_str()) {
                    out.entry(d.name.clone()).or_default().insert(d.kind);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0"?>
<?xml-model href="http://download.ros.org/schema/package_format3.xsd" schematypens="http://www.w3.org/2001/XMLSchema"?>
<package format="3">
  <name>demo_nodes_cpp</name>
  <version>0.33.11</version>
  <description>C++ nodes</description>
  <maintainer email="x@y.z">Someone</maintainer>
  <license>Apache License 2.0</license>

  <buildtool_depend>ament_cmake</buildtool_depend>
  <build_depend>example_interfaces</build_depend>
  <depend>rclcpp</depend>
  <exec_depend>launch_ros</exec_depend>
  <test_depend>ament_lint_auto</test_depend>
  <exec_depend condition="$ROS_VERSION == 2">rmw_implementation</exec_depend>
  <doc_depend>doxygen</doc_depend>

  <export>
    <build_type>ament_cmake</build_type>
  </export>
</package>
"#;

    #[test]
    fn parses_name_and_dependency_kinds() {
        let p = PackageXml::parse(
            SAMPLE,
            Path::new("/ws/src/demos/demo_nodes_cpp/package.xml"),
        )
        .unwrap();
        assert_eq!(p.name, "demo_nodes_cpp");
        assert_eq!(p.version.as_deref(), Some("0.33.11"));
        assert_eq!(p.format, 3);
        assert_eq!(p.dir, PathBuf::from("/ws/src/demos/demo_nodes_cpp"));
        let kinds: Vec<(&str, DependKind)> = p
            .depends
            .iter()
            .map(|d| (d.name.as_str(), d.kind))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("ament_cmake", DependKind::BuildtoolDepend),
                ("example_interfaces", DependKind::BuildDepend),
                ("rclcpp", DependKind::Depend),
                ("launch_ros", DependKind::ExecDepend),
                ("ament_lint_auto", DependKind::TestDepend),
                ("rmw_implementation", DependKind::ExecDepend),
                ("doxygen", DependKind::DocDepend),
            ]
        );
        assert_eq!(p.depends[5].condition.as_deref(), Some("$ROS_VERSION == 2"));
    }

    #[test]
    fn format1_defaults_and_run_depend_alias() {
        let text = "<package><name>old</name><run_depend>roscpp</run_depend></package>";
        let p = PackageXml::parse(text, Path::new("package.xml")).unwrap();
        assert_eq!(p.format, 1);
        assert_eq!(p.depends[0].kind, DependKind::ExecDepend);
    }

    #[test]
    fn rejects_non_package_roots_and_missing_names() {
        assert!(matches!(
            PackageXml::parse("<launch/>", Path::new("p")),
            Err(Error::PackageXml { .. })
        ));
        assert!(matches!(
            PackageXml::parse(
                "<package format=\"2\"><version>1</version></package>",
                Path::new("p")
            ),
            Err(Error::PackageXml { .. })
        ));
        assert!(matches!(
            PackageXml::parse("<package><name>x</name>", Path::new("p")),
            Err(Error::PackageXml { .. })
        ));
    }

    #[test]
    fn external_keys_exclude_workspace_packages() {
        let a = PackageXml::parse(
            "<package format=\"3\"><name>a</name><depend>b</depend><depend>rclcpp</depend><test_depend>rclcpp</test_depend></package>",
            Path::new("a/package.xml"),
        )
        .unwrap();
        let b = PackageXml::parse(
            "<package format=\"3\"><name>b</name><buildtool_depend>ament_cmake</buildtool_depend></package>",
            Path::new("b/package.xml"),
        )
        .unwrap();
        let ws = WorkspacePackages {
            packages: vec![a, b],
            errors: vec![],
        };
        let keys = ws.external_keys();
        assert_eq!(
            keys.keys().collect::<Vec<_>>(),
            vec!["ament_cmake", "rclcpp"]
        );
        assert_eq!(keys["rclcpp"].len(), 2);
    }
}
