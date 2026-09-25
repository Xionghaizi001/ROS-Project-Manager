//! Snapshot repositories used by `rospm lock` / `apply --locked` (spec §3,
//! P0 facts in `docs/p0-external-validation.md` §1–§2).
//!
//! - ROS: `http://snapshots.ros.org/<distro>/<date>/ubuntu` (HTTP only; the
//!   HTTPS certificate does not match). Dates are irregular and must be
//!   discovered from the index, never composed from a requested date.
//!   `Release` is signed by a *different* key than `packages.ros.org`
//!   ([`ROS_SNAPSHOT_KEY_FINGERPRINT`]), distributed via `keyserver.ubuntu.com`.
//! - Ubuntu: `https://snapshot.ubuntu.com/ubuntu/<YYYYMMDDTHHMMSSZ>`, signed by
//!   the regular Ubuntu archive keyring.

/// Root of the ROS snapshot service (no trailing slash).
pub const ROS_SNAPSHOT_ROOT: &str = "http://snapshots.ros.org";
/// Root of the Ubuntu snapshot service (no trailing slash).
pub const UBUNTU_SNAPSHOT_ROOT: &str = "https://snapshot.ubuntu.com/ubuntu";
/// Keyring the regular Ubuntu archive is signed with (shipped by `ubuntu-keyring`).
pub const UBUNTU_ARCHIVE_KEYRING: &str = "/usr/share/keyrings/ubuntu-archive-keyring.gpg";

/// Full fingerprint of the key that signs `snapshots.ros.org` (`ROS Snapshot
/// builder <rosbuild@ros.org>`, RSA 3072, created 2018-11-19, currently valid
/// until 2027-06-01; it has expired and been extended before, so `EXPKEYSIG`
/// means "refresh from the keyserver", never "disable verification").
pub const ROS_SNAPSHOT_KEY_FINGERPRINT: &str = "4B63CF8FDE49746E98FA01DDAD19BAB3CBF125EA";
/// Where rospm installs that key (same path as OSRF's official Dockerfiles).
pub const ROS_SNAPSHOT_KEYRING: &str = "/usr/share/keyrings/ros2-snapshots-archive-keyring.gpg";
/// The keyserver the ROS wiki names for the snapshot key; `keys.openpgp.org`
/// does not carry it.
pub const KEYSERVER: &str = "keyserver.ubuntu.com";

/// HTTP URL that returns the ASCII-armored public key for `fingerprint` from
/// [`KEYSERVER`] (HKP over HTTPS); usable with the plan's `FetchKey` step.
pub fn keyserver_key_url(fingerprint: &str) -> String {
    format!("https://{KEYSERVER}/pks/lookup?op=get&options=mr&search=0x{fingerprint}")
}

/// Base URL of one ROS snapshot: `http://snapshots.ros.org/<distro>/<date>/ubuntu`.
pub fn ros_snapshot_url(distro: &str, date: &str) -> String {
    format!("{ROS_SNAPSHOT_ROOT}/{distro}/{date}/ubuntu")
}

/// Directory index listing the available snapshot dates of `distro`.
pub fn ros_snapshot_index_url(distro: &str) -> String {
    format!("{ROS_SNAPSHOT_ROOT}/{distro}/")
}

/// Base URL of one Ubuntu snapshot: `https://snapshot.ubuntu.com/ubuntu/<timestamp>`.
pub fn ubuntu_snapshot_url(timestamp: &str) -> String {
    format!("{UBUNTU_SNAPSHOT_ROOT}/{timestamp}")
}

/// `dists/<codename>/InRelease` under `base`.
pub fn inrelease_url(base: &str, codename: &str) -> String {
    format!("{base}/dists/{codename}/InRelease")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_follow_the_verified_layout() {
        assert_eq!(
            ros_snapshot_url("jazzy", "2026-09-11"),
            "http://snapshots.ros.org/jazzy/2026-09-11/ubuntu"
        );
        assert_eq!(
            inrelease_url(&ros_snapshot_url("jazzy", "2026-09-11"), "noble"),
            "http://snapshots.ros.org/jazzy/2026-09-11/ubuntu/dists/noble/InRelease"
        );
        assert_eq!(
            ubuntu_snapshot_url("20250901T000000Z"),
            "https://snapshot.ubuntu.com/ubuntu/20250901T000000Z"
        );
        assert!(keyserver_key_url(ROS_SNAPSHOT_KEY_FINGERPRINT)
            .ends_with("search=0x4B63CF8FDE49746E98FA01DDAD19BAB3CBF125EA"));
    }
}
