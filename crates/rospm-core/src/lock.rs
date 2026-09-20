//! `rospm.lock` — the resolved result (spec §3).
//!
//! Layering follows decision #4: providers with their own lock (pixi) are
//! referenced by hash only; loosely managed objects (git, apt, docker images)
//! are locked here directly.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::manifest::ProviderKind;
use crate::platform::Platform;

pub const LOCK_FILE: &str = "rospm.lock";
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lock {
    pub schema_version: u32,
    /// RFC 3339 timestamp.
    pub generated_at: String,
    /// `sha256:<hex>` of the manifest text this lock was generated from.
    pub manifest_sha256: String,
    pub provider: LockedProvider,
    #[serde(default)]
    pub sources: Vec<LockedSource>,
    #[serde(default)]
    pub apt: Vec<LockedApt>,
    /// Recorded for `verify` only; never reinstalled (decision #6).
    #[serde(default)]
    pub pip: Vec<LockedPip>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedProvider {
    pub kind: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<LockedNative>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pixi: Option<LockedPixi>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker: Option<LockedDocker>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedNative {
    pub ubuntu: String,
    /// `snapshots.ros.org/<distro>/<date>/ubuntu`; `None` = live repository (degraded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ros_snapshot: Option<String>,
    /// `snapshot.ubuntu.com/ubuntu/<timestamp>`; `None` = live repository (degraded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ubuntu_snapshot: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedPixi {
    pub manifest: String,
    pub lock: String,
    pub lock_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedDocker {
    pub base_image: String,
    pub digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedSource {
    pub workspace: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
    pub rev: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedApt {
    pub platform: Platform,
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedPip {
    pub name: String,
    pub version: String,
}

impl Lock {
    pub fn parse(text: &str, path: &Path) -> Result<Lock> {
        let lock: Lock = toml::from_str(text).map_err(|source| Error::LockParse {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
        if lock.schema_version != SUPPORTED_SCHEMA_VERSION {
            return Err(Error::SchemaVersion {
                found: lock.schema_version,
                supported: SUPPORTED_SCHEMA_VERSION,
            });
        }
        Ok(lock)
    }

    pub fn load(path: &Path) -> Result<Lock> {
        let text = fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        Lock::parse(&text, path)
    }

    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|source| Error::Serialize {
            what: "lock file",
            source: Box::new(source),
        })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = self.to_toml()?;
        fs::write(path, text).map_err(|e| Error::io(path, e))
    }

    /// Whether `manifest_text` is the manifest this lock was generated from.
    pub fn matches_manifest(&self, manifest_text: &str) -> bool {
        self.manifest_sha256 == sha256_hex(manifest_text)
    }
}

/// `sha256:<hex>` of arbitrary text, the format used throughout the lock file.
pub fn sha256_hex(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    format!("sha256:{digest:x}")
}
