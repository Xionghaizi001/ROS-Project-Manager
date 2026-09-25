//! Package-source mirrors for the native provider.
//!
//! The official ROS sources live on `packages.ros.org` (apt) and GitHub
//! (`raw.githubusercontent.com`: `ros.key`, the rosdistro index, rosdep
//! yaml). Both are unreachable or stalling from some networks. rospm ships a
//! small built-in table of well-known mirrors that carry the same files under
//! the same layout, probes them ([`crate::net`]) before the first download
//! and lets the user switch (decision #17). Whatever the source, the apt
//! signing key is still verified against the built-in fingerprint.

use std::fmt;
use std::str::FromStr;

/// Base URL of the rosdistro repository on GitHub (`ros.key`, `index-v4.yaml`,
/// `rosdep/*.yaml`, `releases/*.yaml`).
pub const OFFICIAL_ROSDISTRO: &str = "https://raw.githubusercontent.com/ros/rosdistro/master";
/// The ROS 2 apt repository.
pub const OFFICIAL_ROS_APT: &str = "http://packages.ros.org/ros2/ubuntu";

/// One source set. `rosdistro` is `None` for mirrors that only carry the apt
/// repository; the official rosdistro is used for the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mirror {
    /// Short name accepted by `--mirror` / `ROSPM_MIRROR`.
    pub name: &'static str,
    /// Human-readable label.
    pub label: &'static str,
    /// Base URL of the `ros2/ubuntu` apt repository (no trailing slash).
    pub ros_apt: &'static str,
    /// Base URL of a rosdistro mirror (no trailing slash), if the mirror has one.
    pub rosdistro: Option<&'static str>,
}

pub const OFFICIAL: Mirror = Mirror {
    name: "official",
    label: "official (packages.ros.org + GitHub)",
    ros_apt: OFFICIAL_ROS_APT,
    rosdistro: Some(OFFICIAL_ROSDISTRO),
};

pub const TUNA: Mirror = Mirror {
    name: "tuna",
    label: "Tsinghua University TUNA (mirrors.tuna.tsinghua.edu.cn)",
    ros_apt: "https://mirrors.tuna.tsinghua.edu.cn/ros2/ubuntu",
    rosdistro: Some("https://mirrors.tuna.tsinghua.edu.cn/rosdistro"),
};

pub const USTC: Mirror = Mirror {
    name: "ustc",
    label: "USTC (mirrors.ustc.edu.cn)",
    ros_apt: "https://mirrors.ustc.edu.cn/ros2/ubuntu",
    rosdistro: Some("https://mirrors.ustc.edu.cn/rosdistro"),
};

pub const ALIYUN: Mirror = Mirror {
    name: "aliyun",
    label: "Alibaba Cloud (mirrors.aliyun.com, apt only)",
    ros_apt: "https://mirrors.aliyun.com/ros2/ubuntu",
    rosdistro: None,
};

/// Every known source set, official first.
pub const ALL: &[Mirror] = &[OFFICIAL, TUNA, USTC, ALIYUN];

/// Mirrors only (everything but [`OFFICIAL`]).
pub const MIRRORS: &[Mirror] = &[TUNA, USTC, ALIYUN];

/// The names accepted by `--mirror`, for clap's value parser.
pub const NAMES: &[&str] = &["official", "tuna", "ustc", "aliyun"];

impl Mirror {
    pub fn by_name(name: &str) -> Option<Mirror> {
        ALL.iter().copied().find(|m| m.name == name)
    }

    pub fn is_official(&self) -> bool {
        self.name == OFFICIAL.name
    }

    /// rosdistro base actually used: the mirror's own, or the official one.
    pub fn rosdistro_base(&self) -> &'static str {
        self.rosdistro.unwrap_or(OFFICIAL_ROSDISTRO)
    }

    /// Whether rosdep's sources should be pointed at this mirror.
    pub fn mirrors_rosdistro(&self) -> bool {
        self.rosdistro.is_some() && !self.is_official()
    }

    pub fn ros_key_url(&self) -> String {
        format!("{}/ros.key", self.rosdistro_base())
    }

    /// `ROSDISTRO_INDEX_URL` for `rosdep update`.
    pub fn rosdistro_index_url(&self) -> String {
        format!("{}/index-v4.yaml", self.rosdistro_base())
    }

    /// File the network probe fetches from the rosdistro tree: `rosdep/base.yaml`
    /// (~270 KB) rather than the 5 KB index, so the probe measures real
    /// throughput and catches connections that open fine but stall mid-transfer
    /// (the failure mode seen on GitHub).
    pub fn rosdistro_probe_url(&self) -> String {
        format!("{}/rosdep/base.yaml", self.rosdistro_base())
    }

    /// `dists/<codename>/InRelease` of the apt repository: small, always
    /// present, and what apt fetches first.
    pub fn apt_inrelease_url(&self, codename: &str) -> String {
        format!("{}/dists/{codename}/InRelease", self.ros_apt)
    }

    /// Contents of `/etc/ros/rosdep/sources.list.d/20-default.list` pointing at
    /// this mirror: the file `rosdep init` writes, with the GitHub base
    /// replaced. (The mirrors' own copy of that file still names GitHub.)
    ///
    /// The upstream file's `gbpdistro …/releases/fuerte.yaml fuerte` line is
    /// dropped on purpose: rosdep's gbpdistro loader fetches `targets.yaml`
    /// from a hard-coded GitHub URL whatever the source says, which is exactly
    /// the host the mirror is there to avoid, and ROS 1 fuerte is irrelevant
    /// for ROS 2 (verified: `rosdep update` failed on that line alone).
    pub fn rosdep_sources_list(&self) -> String {
        let base = self.rosdistro_base();
        format!(
            "# written by rospm: rosdep sources from {} ({base})\n\
             # os-specific listings first\n\
             yaml {base}/rosdep/osx-homebrew.yaml osx\n\
             \n\
             # generic\n\
             yaml {base}/rosdep/base.yaml\n\
             yaml {base}/rosdep/python.yaml\n\
             yaml {base}/rosdep/ruby.yaml\n\
             # (no `gbpdistro … fuerte` line: it would fetch targets.yaml from GitHub)\n\
             \n\
             # newer distributions (Groovy, Hydro, ...) must not be listed anymore, they are being fetched from the rosdistro index.yaml instead\n",
            self.label
        )
    }

    /// Substring that identifies this mirror's ROS 2 apt repository in an apt
    /// source file, whatever the scheme (`packages.ros.org/ros2`, …).
    pub fn apt_needle(&self) -> &'static str {
        self.ros_apt
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or(self.ros_apt)
            .trim_end_matches("/ubuntu")
    }
}

impl fmt::Display for Mirror {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name)
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("unknown mirror `{0}` (expected one of: official, tuna, ustc, aliyun)")]
pub struct ParseMirrorError(pub String);

impl FromStr for Mirror {
    type Err = ParseMirrorError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Mirror::by_name(s).ok_or_else(|| ParseMirrorError(s.to_string()))
    }
}

/// Which source set an apt source file refers to, if any.
pub fn mirror_for_apt_source(text: &str) -> Option<Mirror> {
    ALL.iter().copied().find(|m| {
        text.lines()
            .map(str::trim)
            .filter(|l| !l.starts_with('#'))
            .any(|l| l.contains(m.apt_needle()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_follow_the_shared_layout() {
        assert_eq!(
            OFFICIAL.ros_key_url(),
            "https://raw.githubusercontent.com/ros/rosdistro/master/ros.key"
        );
        assert_eq!(
            TUNA.ros_key_url(),
            "https://mirrors.tuna.tsinghua.edu.cn/rosdistro/ros.key"
        );
        // apt-only mirror falls back to the official rosdistro.
        assert_eq!(ALIYUN.ros_key_url(), OFFICIAL.ros_key_url());
        assert!(!ALIYUN.mirrors_rosdistro());
        assert!(TUNA.mirrors_rosdistro());
        assert!(!OFFICIAL.mirrors_rosdistro());
        assert_eq!(
            USTC.apt_inrelease_url("jammy"),
            "https://mirrors.ustc.edu.cn/ros2/ubuntu/dists/jammy/InRelease"
        );
        assert_eq!(
            TUNA.rosdistro_index_url(),
            "https://mirrors.tuna.tsinghua.edu.cn/rosdistro/index-v4.yaml"
        );
    }

    #[test]
    fn rosdep_list_names_every_source_on_the_mirror() {
        let list = TUNA.rosdep_sources_list();
        for needle in [
            "yaml https://mirrors.tuna.tsinghua.edu.cn/rosdistro/rosdep/base.yaml",
            "yaml https://mirrors.tuna.tsinghua.edu.cn/rosdistro/rosdep/python.yaml",
            "yaml https://mirrors.tuna.tsinghua.edu.cn/rosdistro/rosdep/ruby.yaml",
        ] {
            assert!(list.contains(needle), "{list}");
        }
        assert!(!list.contains("githubusercontent"));
        assert!(!list.lines().any(|l| l.starts_with("gbpdistro")), "{list}");
    }

    #[test]
    fn apt_sources_are_recognised_for_every_mirror() {
        assert_eq!(OFFICIAL.apt_needle(), "packages.ros.org/ros2");
        assert_eq!(TUNA.apt_needle(), "mirrors.tuna.tsinghua.edu.cn/ros2");
        let one_line = "deb [arch=amd64 signed-by=/k.gpg] https://mirrors.ustc.edu.cn/ros2/ubuntu jammy main\n";
        assert_eq!(mirror_for_apt_source(one_line), Some(USTC));
        let deb822 = "Types: deb\nURIs: http://packages.ros.org/ros2/ubuntu\nSuites: noble\n";
        assert_eq!(mirror_for_apt_source(deb822), Some(OFFICIAL));
        assert_eq!(
            mirror_for_apt_source("# deb http://packages.ros.org/ros2/ubuntu jammy main\n"),
            None
        );
        assert_eq!(
            mirror_for_apt_source("deb http://archive.ubuntu.com/ubuntu jammy main\n"),
            None
        );
    }

    #[test]
    fn names_parse_and_round_trip() {
        for name in NAMES {
            assert_eq!(name.parse::<Mirror>().unwrap().name, *name);
        }
        assert!("sjtu".parse::<Mirror>().is_err());
    }
}
