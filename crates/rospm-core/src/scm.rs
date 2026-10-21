//! Source-code managers. rospm wraps them, it does not replace them.

use std::path::Path;

use crate::plan::Command;

pub trait Scm {
    /// Import the repositories listed in `sources_file` into `<ws_path>/src`.
    fn import(&self, ws_path: &Path, sources_file: &Path) -> Command;
    /// Emit an exact (commit-pinned) description of `<ws_path>/src` on stdout.
    fn export_exact(&self, ws_path: &Path) -> Command;
}

/// vcstool (`vcs`), the ROS-standard multi-repo tool.
#[derive(Debug, Default, Clone, Copy)]
pub struct Vcstool;

impl Scm for Vcstool {
    fn import(&self, ws_path: &Path, sources_file: &Path) -> Command {
        Command::new("vcs")
            .args(["import", "--recursive", "--input"])
            .arg(sources_file.to_string_lossy())
            .arg("src")
            .cwd(ws_path)
    }

    fn export_exact(&self, ws_path: &Path) -> Command {
        Command::new("vcs")
            .args(["export", "--exact", "src"])
            .cwd(ws_path)
    }
}
