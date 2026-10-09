//! Target platforms and ROS distributions.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A target platform. Only Linux is supported (decision: Ubuntu / ROS 2 only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Platform {
    #[serde(rename = "linux-x86_64")]
    LinuxX86_64,
    #[serde(rename = "linux-aarch64")]
    LinuxAarch64,
}

impl Platform {
    pub const ALL: [Platform; 2] = [Platform::LinuxX86_64, Platform::LinuxAarch64];

    /// Platform of the running binary, if it is one we support.
    pub fn current() -> Option<Platform> {
        if !cfg!(target_os = "linux") {
            return None;
        }
        if cfg!(target_arch = "x86_64") {
            Some(Platform::LinuxX86_64)
        } else if cfg!(target_arch = "aarch64") {
            Some(Platform::LinuxAarch64)
        } else {
            None
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Platform::LinuxX86_64 => "linux-x86_64",
            Platform::LinuxAarch64 => "linux-aarch64",
        }
    }

    /// Architecture name as used by dpkg / apt (`dpkg --add-architecture`).
    pub fn dpkg_arch(&self) -> &'static str {
        match self {
            Platform::LinuxX86_64 => "amd64",
            Platform::LinuxAarch64 => "arm64",
        }
    }

    /// Platform string as used by `docker buildx --platform`.
    pub fn docker_platform(&self) -> &'static str {
        match self {
            Platform::LinuxX86_64 => "linux/amd64",
            Platform::LinuxAarch64 => "linux/arm64",
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("unknown platform `{0}` (expected one of: linux-x86_64, linux-aarch64)")]
pub struct ParsePlatformError(pub String);

impl FromStr for Platform {
    type Err = ParsePlatformError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "aarch64" | "arm64" => return Ok(Self::LinuxAarch64),
            "x86_64" | "amd64" => return Ok(Self::LinuxX86_64),
            _ => {}
        }
        Platform::ALL
            .iter()
            .copied()
            .find(|p| p.as_str() == s)
            .ok_or_else(|| ParsePlatformError(s.to_string()))
    }
}

/// Supported ROS 2 distributions (decision #5). EOL does not remove a distro
/// from this list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RosDistro {
    Humble,
    Jazzy,
    Rolling,
}

impl RosDistro {
    pub const ALL: [RosDistro; 3] = [RosDistro::Humble, RosDistro::Jazzy, RosDistro::Rolling];

    pub fn as_str(&self) -> &'static str {
        match self {
            RosDistro::Humble => "humble",
            RosDistro::Jazzy => "jazzy",
            RosDistro::Rolling => "rolling",
        }
    }

    /// Ubuntu release the distro is officially built for.
    pub fn default_ubuntu(&self) -> &'static str {
        match self {
            RosDistro::Humble => "22.04",
            RosDistro::Jazzy | RosDistro::Rolling => "24.04",
        }
    }

    /// Whether this distro is a rolling target (CI smoke only, decision #5).
    pub fn is_rolling(&self) -> bool {
        matches!(self, RosDistro::Rolling)
    }
}

/// Ubuntu version number to codename, used when composing apt sources.
pub fn ubuntu_codename(version: &str) -> Option<&'static str> {
    match version {
        "22.04" => Some("jammy"),
        "24.04" => Some("noble"),
        _ => None,
    }
}

impl fmt::Display for RosDistro {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("unknown ROS distro `{0}` (expected one of: humble, jazzy, rolling)")]
pub struct ParseDistroError(pub String);

impl FromStr for RosDistro {
    type Err = ParseDistroError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        RosDistro::ALL
            .iter()
            .copied()
            .find(|d| d.as_str() == s)
            .ok_or_else(|| ParseDistroError(s.to_string()))
    }
}
