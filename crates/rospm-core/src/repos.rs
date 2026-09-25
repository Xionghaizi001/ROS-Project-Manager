//! vcstool `.repos` files.
//!
//! ```yaml
//! repositories:
//!   ros2/demos:
//!     type: git
//!     url: https://github.com/ros2/demos.git
//!     version: jazzy
//! ```
//!
//! rospm only reads these files (spec §1: they are referenced from
//! `rospm.toml`, never generated from it). Keys are repository paths relative
//! to `<workspace>/src`.

use std::fs;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    /// `git`, `hg`, `svn`, `bzr`, `tar`, `zip` — vcstool accepts all of these.
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
    /// Branch, tag or commit. Absent = the remote's default branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReposFile {
    /// Path (relative to `src/`) → repository. Insertion order is preserved.
    #[serde(default)]
    pub repositories: IndexMap<String, Repo>,
}

impl ReposFile {
    pub fn parse(text: &str, path: &Path) -> Result<ReposFile> {
        // An empty file (or one that only holds comments) is a valid, empty list.
        if text.trim().is_empty() {
            return Ok(ReposFile::default());
        }
        let file: ReposFile = serde_yaml::from_str(text).map_err(|source| Error::ReposParse {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
        Ok(file)
    }

    pub fn load(path: &Path) -> Result<ReposFile> {
        let text = fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        ReposFile::parse(&text, path)
    }

    /// Where `vcs import` would place each repository under `src_dir`.
    pub fn checkout_dirs(&self, src_dir: &Path) -> Vec<(String, PathBuf)> {
        self.repositories
            .keys()
            .map(|rel| (rel.clone(), src_dir.join(rel)))
            .collect()
    }
}

/// Compare two git URLs loosely: ignore a trailing `.git`, trailing slashes
/// and the `ssh://git@host/` vs `git@host:` spelling difference.
pub fn same_git_url(a: &str, b: &str) -> bool {
    normalize_git_url(a) == normalize_git_url(b)
}

fn normalize_git_url(url: &str) -> String {
    let mut s = url.trim().trim_end_matches('/').to_string();
    if let Some(stripped) = s.strip_suffix(".git") {
        s = stripped.to_string();
    }
    // git@github.com:org/repo  →  github.com/org/repo
    if let Some(rest) = s.strip_prefix("git@") {
        s = rest.replacen(':', "/", 1);
    }
    for scheme in ["ssh://git@", "https://", "http://", "git://", "ssh://"] {
        if let Some(rest) = s.strip_prefix(scheme) {
            s = rest.to_string();
            break;
        }
    }
    s.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vcstool_format() {
        let text = r#"
repositories:
  ros2/demos:
    type: git
    url: https://github.com/ros2/demos.git
    version: jazzy
  # a comment
  ros2/examples:
    type: git
    url: https://github.com/ros2/examples.git
"#;
        let f = ReposFile::parse(text, Path::new("x.repos")).unwrap();
        assert_eq!(f.repositories.len(), 2);
        let demos = &f.repositories["ros2/demos"];
        assert_eq!(demos.kind, "git");
        assert_eq!(demos.version.as_deref(), Some("jazzy"));
        assert!(f.repositories["ros2/examples"].version.is_none());
        let dirs = f.checkout_dirs(Path::new("/ws/src"));
        assert_eq!(dirs[0].1, PathBuf::from("/ws/src/ros2/demos"));
    }

    #[test]
    fn empty_and_comment_only_files_are_empty_lists() {
        assert!(ReposFile::parse("", Path::new("x"))
            .unwrap()
            .repositories
            .is_empty());
        assert!(ReposFile::parse("# nothing\n", Path::new("x"))
            .unwrap()
            .repositories
            .is_empty());
        assert!(ReposFile::parse("repositories:\n", Path::new("x"))
            .unwrap()
            .repositories
            .is_empty());
    }

    #[test]
    fn malformed_file_is_an_error() {
        let err = ReposFile::parse("repositories: [1, 2", Path::new("bad.repos")).unwrap_err();
        assert!(matches!(err, Error::ReposParse { .. }), "{err}");
    }

    #[test]
    fn git_urls_compare_loosely() {
        assert!(same_git_url(
            "https://github.com/ros2/demos.git",
            "https://github.com/ros2/demos"
        ));
        assert!(same_git_url(
            "git@github.com:ros2/demos.git",
            "ssh://git@github.com/ros2/demos"
        ));
        assert!(same_git_url(
            "https://GitHub.com/ros2/demos/",
            "https://github.com/ros2/demos"
        ));
        assert!(!same_git_url(
            "https://github.com/ros2/demos",
            "https://github.com/ros2/examples"
        ));
    }
}
