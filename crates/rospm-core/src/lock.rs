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
use crate::message::Message;
use crate::platform::Platform;

pub const LOCK_FILE: &str = "rospm.lock";
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedPixi {
    pub manifest: String,
    pub lock: String,
    pub lock_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedDocker {
    pub base_image: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedSource {
    pub workspace: String,
    /// Path under `<workspace>/src`, i.e. the key in the `.repos` file.
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
    /// Exact revision (`vcs export --exact`): the commit SHA for git.
    pub rev: String,
    /// Branch / tag the `.repos` file asked for, for humans; `rev` is what counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedApt {
    pub platform: Platform,
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

    /// Whether two locks describe the same result (everything but `generated_at`).
    pub fn same_content(&self, other: &Lock) -> bool {
        let mut a = self.clone();
        let mut b = other.clone();
        a.generated_at.clear();
        b.generated_at.clear();
        a == b
    }

    /// Carry over the apt entries `previous` recorded for platforms other than
    /// `resolved`: a lock accumulates platforms from several machines, and a
    /// host can only resolve its own (roadmap P5 adds cross-platform queries).
    pub fn merge_other_platforms(&mut self, previous: &Lock, resolved: Platform) {
        let carried: Vec<LockedApt> = previous
            .apt
            .iter()
            .filter(|a| a.platform != resolved)
            .cloned()
            .collect();
        self.apt.retain(|a| a.platform == resolved);
        self.apt.extend(carried);
        sort_apt(&mut self.apt);
    }

    /// Entries for one platform, in file order.
    pub fn apt_for(&self, platform: Platform) -> Vec<&LockedApt> {
        self.apt.iter().filter(|a| a.platform == platform).collect()
    }
}

/// Deterministic order: by platform (x86_64 first), then name.
pub fn sort_apt(apt: &mut [LockedApt]) {
    apt.sort_by(|a, b| {
        let pa = Platform::ALL.iter().position(|p| *p == a.platform);
        let pb = Platform::ALL.iter().position(|p| *p == b.platform);
        pa.cmp(&pb).then_with(|| a.name.cmp(&b.name))
    });
}

/// What a provider hands back from `resolve_lock`: the lock plus the notes
/// the user must see (degradations, packages that could not be recorded).
#[derive(Debug, Clone)]
pub struct Resolution {
    pub lock: Lock,
    pub notes: Vec<Message>,
}

/// Current time as RFC 3339 UTC with second precision (`2026-09-15T08:00:00Z`).
pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    rfc3339_from_unix(secs)
}

/// Unix seconds → RFC 3339 UTC (proleptic Gregorian, Howard Hinnant's
/// civil-from-days). Avoids a chrono dependency for one timestamp.
pub fn rfc3339_from_unix(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// `sha256:<hex>` of arbitrary text, the format used throughout the lock file.
pub fn sha256_hex(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    format!("sha256:{digest:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_matches_known_instants() {
        assert_eq!(rfc3339_from_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_from_unix(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339_from_unix(1_789_495_200), "2026-09-15T18:00:00Z");
        assert_eq!(rfc3339_from_unix(1_709_164_799), "2024-02-28T23:59:59Z");
    }

    fn apt(platform: Platform, name: &str, version: &str) -> LockedApt {
        LockedApt {
            platform,
            name: name.into(),
            version: version.into(),
            sha256: None,
        }
    }

    fn lock(apt_entries: Vec<LockedApt>) -> Lock {
        Lock {
            schema_version: SUPPORTED_SCHEMA_VERSION,
            generated_at: "2026-09-15T08:00:00Z".into(),
            manifest_sha256: sha256_hex("m"),
            provider: LockedProvider {
                kind: ProviderKind::Native,
                native: Some(LockedNative {
                    ubuntu: "24.04".into(),
                    ros_snapshot: None,
                    ubuntu_snapshot: None,
                }),
                pixi: None,
                docker: None,
            },
            sources: Vec::new(),
            apt: apt_entries,
            pip: Vec::new(),
        }
    }

    #[test]
    fn merge_keeps_other_platforms_and_replaces_the_resolved_one() {
        let previous = lock(vec![
            apt(Platform::LinuxAarch64, "ros-jazzy-rclcpp", "1"),
            apt(Platform::LinuxX86_64, "ros-jazzy-old", "1"),
        ]);
        let mut fresh = lock(vec![apt(Platform::LinuxX86_64, "ros-jazzy-rclcpp", "2")]);
        fresh.merge_other_platforms(&previous, Platform::LinuxX86_64);
        let names: Vec<(Platform, &str)> = fresh
            .apt
            .iter()
            .map(|a| (a.platform, a.name.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                (Platform::LinuxX86_64, "ros-jazzy-rclcpp"),
                (Platform::LinuxAarch64, "ros-jazzy-rclcpp")
            ]
        );
        assert_eq!(fresh.apt_for(Platform::LinuxAarch64).len(), 1);
    }

    #[test]
    fn same_content_ignores_generated_at_and_round_trips() {
        let a = lock(vec![apt(Platform::LinuxX86_64, "x", "1")]);
        let mut b = a.clone();
        b.generated_at = "2027-01-01T00:00:00Z".into();
        assert!(a.same_content(&b));
        b.apt[0].version = "2".into();
        assert!(!a.same_content(&b));
        let text = a.to_toml().unwrap();
        let back = Lock::parse(&text, Path::new("rospm.lock")).unwrap();
        assert_eq!(back, a);
    }
}
