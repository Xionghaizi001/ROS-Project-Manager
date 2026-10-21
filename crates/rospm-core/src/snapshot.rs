//! Snapshot repositories used by `rospm lock` / `apply --locked` (spec §3,
//! P0 facts in `docs/p0-external-validation.md` §1–§2).
//!
//! - ROS: `http://snapshots.ros.org/<distro>/<date>/ubuntu` (HTTP only; the
//!   HTTPS certificate does not match). Dates are irregular and must be
//!   discovered from the index, never composed from a requested date.
//!   `Release` is signed by a *different* key than `packages.ros.org`
//!   ([`ROS_SNAPSHOT_KEY_FINGERPRINT`]), distributed via `keyserver.ubuntu.com`.
//! - Ubuntu: `https://snapshot.ubuntu.com/ubuntu/<YYYYMMDDTHHMMSSZ>`, signed by
//!   the regular Ubuntu archive keyring. Any timestamp resolves to the newest
//!   snapshot at or before it.
//!
//! Network access goes through the [`Fetch`] trait so discovery is testable
//! with canned responses; [`Http`] is the real implementation.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::Path;
use std::process;
use std::time::Duration;

use crate::message::Message;
use crate::msg;
use crate::plan::{fingerprints, is_only_key};

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

/// Per-request timeout for discovery.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

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

/// `dists/<codename>/Release` under `base`.
pub fn release_url(base: &str, codename: &str) -> String {
    format!("{base}/dists/{codename}/Release")
}

/// `dists/<suite>/<component>/binary-<arch>/Packages.gz` under `base`.
pub fn packages_url(base: &str, suite: &str, component: &str, arch: &str) -> String {
    format!("{base}/dists/{suite}/{component}/binary-{arch}/Packages.gz")
}

/// `YYYYMMDDTHHMMSSZ` for a Unix time, the form `snapshot.ubuntu.com` takes.
pub fn ubuntu_timestamp_from_unix(secs: u64) -> String {
    crate::lock::rfc3339_from_unix(secs).replace(['-', ':'], "")
}

// ---------------------------------------------------------------- fetching

/// A GET that returns the status code and body. Implemented by [`Http`] and
/// by canned maps in tests.
pub trait Fetch {
    fn get(&self, url: &str) -> Result<(u16, Vec<u8>), Message>;

    /// Like [`Fetch::get`] for bodies of several megabytes (`Packages`
    /// indexes), where the short timeout meant for small files is too tight.
    fn get_large(&self, url: &str) -> Result<(u16, Vec<u8>), Message> {
        self.get(url)
    }
}

/// ureq-backed [`Fetch`] with a per-request timeout.
pub struct Http {
    pub timeout: Duration,
}

impl Default for Http {
    fn default() -> Self {
        Http {
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl Fetch for Http {
    fn get(&self, url: &str) -> Result<(u16, Vec<u8>), Message> {
        self.get_with_timeout(url, self.timeout)
    }

    fn get_large(&self, url: &str) -> Result<(u16, Vec<u8>), Message> {
        self.get_with_timeout(url, self.timeout * 6)
    }
}

impl Http {
    fn get_with_timeout(&self, url: &str, timeout: Duration) -> Result<(u16, Vec<u8>), Message> {
        let agent = crate::net::agent(timeout);
        let mut resp = agent
            .get(url)
            .call()
            .map_err(|e| crate::net::short_error(&e, timeout))?;
        let status = resp.status().as_u16();
        // ureq caps `read_to_vec` at 10 MB; `universe` indexes are larger.
        let body = resp
            .body_mut()
            .with_config()
            .limit(256 * 1024 * 1024)
            .read_to_vec()
            .map_err(|e| crate::net::short_error(&e, timeout))?;
        Ok((status, body))
    }
}

/// Canned responses keyed by URL; anything else is a 404. For tests.
#[derive(Default)]
pub struct Canned(pub BTreeMap<String, (u16, Vec<u8>)>);

impl Canned {
    pub fn with(mut self, url: impl Into<String>, status: u16, body: impl AsRef<[u8]>) -> Self {
        self.0.insert(url.into(), (status, body.as_ref().to_vec()));
        self
    }
}

impl Fetch for Canned {
    fn get(&self, url: &str) -> Result<(u16, Vec<u8>), Message> {
        Ok(self.0.get(url).cloned().unwrap_or((404, Vec::new())))
    }
}

fn get_text(fetch: &dyn Fetch, url: &str) -> Result<String, Message> {
    let (status, body) = fetch.get(url)?;
    if !(200..300).contains(&status) {
        return Err(msg!("snapshot.http_status", url = url, status = status));
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

// ---------------------------------------------------------------- parsing

/// `(Package, Version)` of every stanza in an apt `Packages` index.
pub fn parse_packages(text: &str) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    let (mut name, mut version): (Option<&str>, Option<&str>) = (None, None);
    for line in text.lines().chain(std::iter::once("")) {
        if line.trim().is_empty() {
            if let (Some(n), Some(v)) = (name.take(), version.take()) {
                out.insert((n.to_string(), v.to_string()));
            }
            name = None;
            version = None;
        } else if let Some(v) = line.strip_prefix("Package:") {
            name = Some(v.trim());
        } else if let Some(v) = line.strip_prefix("Version:") {
            version = Some(v.trim());
        }
    }
    out
}

/// Download one `Packages.gz` and parse it. A body that is not gzip is
/// parsed as is (canned test servers serve plain text).
pub fn fetch_packages(fetch: &dyn Fetch, url: &str) -> Result<BTreeSet<(String, String)>, Message> {
    let (status, body) = fetch.get_large(url)?;
    if !(200..300).contains(&status) {
        return Err(msg!("snapshot.http_status", url = url, status = status));
    }
    let body = if body.starts_with(&[0x1f, 0x8b]) {
        gunzip(&body)?
    } else {
        body
    };
    Ok(parse_packages(&String::from_utf8_lossy(&body)))
}

/// `gzip -dc` (always present on Ubuntu; avoids a compression dependency).
fn gunzip(bytes: &[u8]) -> Result<Vec<u8>, Message> {
    decompress(bytes, "gzip")
}

pub(crate) fn decompress(bytes: &[u8], program: &str) -> Result<Vec<u8>, Message> {
    let mut child = process::Command::new(program)
        .arg("-dc")
        .stdin(process::Stdio::piped())
        .stdout(process::Stdio::piped())
        .stderr(process::Stdio::piped())
        .spawn()
        .map_err(|e| Message::text(format!("{program}: {e}")))?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let input = bytes.to_vec();
    // Written from another thread: a large index would otherwise fill the
    // stdout pipe while we are still blocked writing stdin.
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let out = child
        .wait_with_output()
        .map_err(|e| Message::text(format!("{program}: {e}")))?;
    let _ = writer.join();
    if !out.status.success() {
        return Err(Message::text(format!(
            "{program}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

/// Snapshot dates (`YYYY-MM-DD`) linked from a directory index, ascending.
pub fn parse_index_dates(html: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find("href=\"") {
        rest = &rest[i + 6..];
        let Some(end) = rest.find('"') else { break };
        let href = rest[..end].trim_end_matches('/');
        let href = href.rsplit('/').next().unwrap_or(href);
        if is_date(href) && !out.iter().any(|d| d == href) {
            out.push(href.to_string());
        }
        rest = &rest[end..];
    }
    out.sort();
    out
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// The fields of an apt `Release` / `InRelease` file rospm cares about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Release {
    pub suite: Option<String>,
    pub codename: Option<String>,
    pub architectures: Vec<String>,
    pub components: Vec<String>,
    /// `Date:` header verbatim (`Fri, 11 Sep 2026 18:21:46 UTC`).
    pub date: Option<String>,
}

pub fn parse_release(text: &str) -> Release {
    let mut r = Release::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim();
        match k.trim() {
            "Suite" => r.suite = Some(v.to_string()),
            "Codename" => r.codename = Some(v.to_string()),
            "Architectures" => r.architectures = v.split_whitespace().map(String::from).collect(),
            "Components" => r.components = v.split_whitespace().map(String::from).collect(),
            "Date" => r.date = Some(v.to_string()),
            _ => {}
        }
    }
    r
}

/// `Fri, 11 Sep 2026 18:21:46 UTC` → `20260911T182146Z`, the timestamp form
/// `snapshot.ubuntu.com` takes. `None` when the header is not in that shape.
pub fn ubuntu_timestamp_from_date(date: &str) -> Option<String> {
    let parts: Vec<&str> = date
        .split(',')
        .nth(1)
        .unwrap_or(date)
        .split_whitespace()
        .collect();
    // [day, month, year, hh:mm:ss, zone]
    if parts.len() < 4 {
        return None;
    }
    let day: u32 = parts[0].parse().ok()?;
    let month = match parts[1] {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: u32 = parts[2].parse().ok()?;
    let mut hms = parts[3].split(':');
    let h: u32 = hms.next()?.parse().ok()?;
    let m: u32 = hms.next()?.parse().ok()?;
    let s: u32 = hms.next()?.parse().ok()?;
    if !matches!(
        parts.get(4).copied(),
        None | Some("UTC") | Some("GMT") | Some("+0000")
    ) {
        return None;
    }
    Some(format!("{year:04}{month:02}{day:02}T{h:02}{m:02}{s:02}Z"))
}

// ---------------------------------------------------------------- discovery

/// Outcome of a signature check that does not stop discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signature {
    Verified,
    /// Could not be checked here (tool or keyring missing); the reason.
    Skipped(Message),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosSnapshot {
    /// `YYYY-MM-DD` as listed by the index.
    pub date: String,
    pub base: String,
    pub release: Release,
    pub signature: Signature,
}

/// Newest ROS snapshot of `distro` whose `Release` serves `codename` for
/// `arch`, with its signature verified against the keyserver copy of
/// [`ROS_SNAPSHOT_KEY_FINGERPRINT`]. A snapshot whose signature *fails* is
/// rejected (the next older date is tried); one that cannot be checked
/// because gpg is missing is accepted with [`Signature::Skipped`].
///
/// `accept` also has to approve each candidate (`lock` uses it
/// to require that the snapshot carries the installed versions); a rejected
/// candidate is skipped like one with a bad signature.
pub fn discover_ros(
    fetch: &dyn Fetch,
    distro: &str,
    codename: &str,
    arch: &str,
    accept: &mut dyn FnMut(&RosSnapshot) -> Result<(), Message>,
) -> Result<RosSnapshot, Message> {
    let index = get_text(fetch, &ros_snapshot_index_url(distro))?;
    let dates = parse_index_dates(&index);
    if dates.is_empty() {
        return Err(msg!("snapshot.index_empty", distro = distro));
    }
    // The error reported is the newest date's: that is the one a fresh lock wanted.
    let mut first_err: Option<Message> = None;
    // Only the newest few: an old snapshot is not what a fresh lock should pin.
    for date in dates.iter().rev().take(3) {
        match ros_snapshot_on(fetch, distro, date, codename, arch, accept) {
            Ok(snapshot) => return Ok(snapshot),
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    Err(first_err.unwrap_or_else(|| msg!("snapshot.no_usable_date", distro = distro)))
}

/// The ROS snapshot of `distro` on `date`, checked like each candidate of
/// [`discover_ros`]: `Release` serves `codename`/`arch`, `accept` approves
/// it, and the signature does not fail.
pub fn ros_snapshot_on(
    fetch: &dyn Fetch,
    distro: &str,
    date: &str,
    codename: &str,
    arch: &str,
    accept: &mut dyn FnMut(&RosSnapshot) -> Result<(), Message>,
) -> Result<RosSnapshot, Message> {
    let base = ros_snapshot_url(distro, date);
    let release_text = get_text(fetch, &release_url(&base, codename))?;
    let release = parse_release(&release_text);
    let suite_ok =
        release.suite.as_deref() == Some(codename) || release.codename.as_deref() == Some(codename);
    if !suite_ok || !release.architectures.iter().any(|a| a == arch) {
        return Err(msg!(
            "snapshot.release_mismatch",
            url = release_url(&base, codename),
            codename = codename,
            arch = arch,
            suite = release.suite.clone().unwrap_or_default(),
            architectures = release.architectures.join(" ")
        ));
    }
    // `accept` runs first: it only reads, and a candidate it rejects
    // needs no keyserver round trip. Both must pass either way.
    let mut snapshot = RosSnapshot {
        date: date.to_string(),
        base,
        release,
        signature: Signature::Skipped(Message::text("")),
    };
    accept(&snapshot)?;
    snapshot.signature = verify_ros_signature(fetch, &snapshot.base, codename, &release_text)?;
    Ok(snapshot)
}

/// Verify `Release.gpg` of one ROS snapshot with a keyring built from the
/// keyserver's copy of the snapshot key (fingerprint checked before use).
fn verify_ros_signature(
    fetch: &dyn Fetch,
    base: &str,
    codename: &str,
    release_text: &str,
) -> Result<Signature, Message> {
    let (Some(gpg), Some(gpgv)) = (which::which("gpg").ok(), which::which("gpgv").ok()) else {
        return Ok(Signature::Skipped(msg!("snapshot.sig_no_gpg")));
    };
    let sig_url = format!("{}.gpg", release_url(base, codename));
    let (status, sig) = fetch.get(&sig_url)?;
    if !(200..300).contains(&status) {
        return Err(msg!("snapshot.http_status", url = sig_url, status = status));
    }
    let key_url = keyserver_key_url(ROS_SNAPSHOT_KEY_FINGERPRINT);
    let (status, key) = fetch.get(&key_url)?;
    if !(200..300).contains(&status) {
        return Err(msg!(
            "snapshot.keyserver_failed",
            url = key_url,
            status = status
        ));
    }
    let dir = TempDir::new("rospm-snapshot")?;
    let key_file = dir.path().join("key.asc");
    let keyring = dir.path().join("keyring.gpg");
    let release_file = dir.path().join("Release");
    let sig_file = dir.path().join("Release.gpg");
    for (p, bytes) in [
        (&key_file, key.as_slice()),
        (&release_file, release_text.as_bytes()),
        (&sig_file, sig.as_slice()),
    ] {
        std::fs::write(p, bytes).map_err(|e| Message::text(format!("{}: {e}", p.display())))?;
    }
    // Fingerprint of what the keyserver returned, before trusting it.
    let shown = process::Command::new(&gpg)
        .args([
            "--batch",
            "--no-default-keyring",
            "--with-colons",
            "--with-fingerprint",
            "--show-keys",
        ])
        .arg(&key_file)
        .stdin(process::Stdio::null())
        .output()
        .map_err(|e| Message::text(format!("gpg: {e}")))?;
    let found = fingerprints(&String::from_utf8_lossy(&shown.stdout));
    if !is_only_key(&found, ROS_SNAPSHOT_KEY_FINGERPRINT) {
        return Err(msg!(
            "snapshot.key_fingerprint_mismatch",
            url = key_url,
            found = if found.is_empty() {
                "<none>".to_string()
            } else {
                found.join(", ")
            },
            expected = ROS_SNAPSHOT_KEY_FINGERPRINT
        ));
    }
    let imported = process::Command::new(&gpg)
        .args(["--batch", "--no-default-keyring", "--keyring"])
        .arg(&keyring)
        .arg("--import")
        .arg(&key_file)
        .stdin(process::Stdio::null())
        .output()
        .map_err(|e| Message::text(format!("gpg: {e}")))?;
    if !imported.status.success() {
        return Err(Message::text(format!(
            "gpg --import: {}",
            String::from_utf8_lossy(&imported.stderr).trim()
        )));
    }
    let verified = process::Command::new(&gpgv)
        .arg("--keyring")
        .arg(&keyring)
        .arg(&sig_file)
        .arg(&release_file)
        .stdin(process::Stdio::null())
        .output()
        .map_err(|e| Message::text(format!("gpgv: {e}")))?;
    if verified.status.success() {
        Ok(Signature::Verified)
    } else {
        let stderr = String::from_utf8_lossy(&verified.stderr);
        let hint = if stderr.contains("EXPKEYSIG") || stderr.contains("expired") {
            msg!("snapshot.sig_expired")
        } else {
            Message::text(stderr.trim().to_string())
        };
        Err(msg!("snapshot.sig_failed", url = sig_url, error = hint))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UbuntuSnapshot {
    /// `YYYYMMDDTHHMMSSZ` as accepted by `snapshot.ubuntu.com`.
    pub timestamp: String,
    pub base: String,
    pub signature: Signature,
}

/// Confirm that `snapshot.ubuntu.com` serves `codename` at `timestamp`
/// (InRelease answers 200) and verify its signature with the host's Ubuntu
/// archive keyring when `keyring` exists.
pub fn discover_ubuntu(
    fetch: &dyn Fetch,
    codename: &str,
    timestamp: &str,
    keyring: &Path,
) -> Result<UbuntuSnapshot, Message> {
    let base = ubuntu_snapshot_url(timestamp);
    let url = inrelease_url(&base, codename);
    let text = get_text(fetch, &url)?;
    let release = parse_release(&text);
    if release.suite.as_deref() != Some(codename) && release.codename.as_deref() != Some(codename) {
        return Err(msg!(
            "snapshot.release_mismatch",
            url = url,
            codename = codename,
            arch = "",
            suite = release.suite.clone().unwrap_or_default(),
            architectures = release.architectures.join(" ")
        ));
    }
    let signature = match which::which("gpgv").ok() {
        Some(gpgv) if keyring.is_file() => {
            let dir = TempDir::new("rospm-ubuntu-snapshot")?;
            let inrelease = dir.path().join("InRelease");
            std::fs::write(&inrelease, text.as_bytes())
                .map_err(|e| Message::text(format!("{}: {e}", inrelease.display())))?;
            let out = process::Command::new(gpgv)
                .arg("--keyring")
                .arg(keyring)
                .arg(&inrelease)
                .stdin(process::Stdio::null())
                .output()
                .map_err(|e| Message::text(format!("gpgv: {e}")))?;
            if out.status.success() {
                Signature::Verified
            } else {
                return Err(msg!(
                    "snapshot.sig_failed",
                    url = url,
                    error = String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
        }
        Some(_) => Signature::Skipped(msg!("snapshot.sig_no_keyring", keyring = keyring.display())),
        None => Signature::Skipped(msg!("snapshot.sig_no_gpg")),
    };
    Ok(UbuntuSnapshot {
        timestamp: timestamp.to_string(),
        base,
        signature,
    })
}

/// A directory under the system temp dir, removed on drop.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Result<TempDir, Message> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir)
            .map_err(|e| Message::text(format!("{}: {e}", dir.display())))?;
        Ok(TempDir(dir))
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACKAGES: &str = "Package: curl\nVersion: 7.81.0-1ubuntu1.25\nArchitecture: amd64\n\nPackage: libcurl4\nArchitecture: amd64\nVersion: 7.81.0-1ubuntu1.25\n\nPackage: no-version\n\nPackage: last\nVersion: 1.0";

    #[test]
    fn packages_index_yields_name_version_pairs() {
        let set = parse_packages(PACKAGES);
        let pairs: Vec<(&str, &str)> = set.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
        assert_eq!(
            pairs,
            vec![
                ("curl", "7.81.0-1ubuntu1.25"),
                ("last", "1.0"),
                ("libcurl4", "7.81.0-1ubuntu1.25"),
            ]
        );
    }

    #[test]
    fn fetch_packages_gunzips_and_accepts_plain_text() {
        let url = packages_url("http://x/ubuntu", "jammy-updates", "main", "amd64");
        assert_eq!(
            url,
            "http://x/ubuntu/dists/jammy-updates/main/binary-amd64/Packages.gz"
        );
        let mut gzip = process::Command::new("gzip")
            .arg("-c")
            .stdin(process::Stdio::piped())
            .stdout(process::Stdio::piped())
            .spawn()
            .unwrap();
        gzip.stdin
            .take()
            .unwrap()
            .write_all(PACKAGES.as_bytes())
            .unwrap();
        let gz = gzip.wait_with_output().unwrap().stdout;
        assert_eq!(&gz[..2], &[0x1f, 0x8b]);
        let fetch = Canned::default().with(url.clone(), 200, &gz);
        assert_eq!(
            fetch_packages(&fetch, &url).unwrap(),
            parse_packages(PACKAGES)
        );
        let plain = Canned::default().with(url.clone(), 200, PACKAGES);
        assert_eq!(fetch_packages(&plain, &url).unwrap().len(), 3);
        let err = fetch_packages(&Canned::default(), &url).unwrap_err();
        assert_eq!(err.key_name(), Some("snapshot.http_status"));
        let broken = Canned::default().with(url.clone(), 200, [0x1f, 0x8b, 0, 1, 2]);
        assert!(fetch_packages(&broken, &url).is_err());
    }

    #[test]
    fn ubuntu_timestamp_from_unix_matches_the_snapshot_form() {
        assert_eq!(ubuntu_timestamp_from_unix(0), "19700101T000000Z");
        assert_eq!(
            ubuntu_timestamp_from_unix(1_789_150_906),
            "20260911T182146Z"
        );
    }

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

    #[test]
    fn index_dates_are_extracted_and_sorted() {
        let html = r#"<html><body><h1>Index of /jazzy/</h1><hr><pre><a href="../">../</a>
<a href="2026-09-11/">2026-09-11/</a>   11-Sep-2026 18:21    -
<a href="2024-05-23/">2024-05-23/</a>   23-May-2024 10:00    -
<a href="2025-01-15/">2025-01-15/</a>   15-Jan-2025 10:00    -
<a href="latest/">latest/</a>
</pre></body></html>"#;
        assert_eq!(
            parse_index_dates(html),
            vec!["2024-05-23", "2025-01-15", "2026-09-11"]
        );
        assert!(parse_index_dates("<html>nothing</html>").is_empty());
    }

    #[test]
    fn release_parses_and_date_becomes_an_ubuntu_timestamp() {
        let r = parse_release(
            "Origin: ROS\nLabel: jazzy/2026-09-11/ubuntu noble\nSuite: noble\nCodename: noble\nDate: Fri, 11 Sep 2026 18:21:46 UTC\nArchitectures: amd64 arm64 armhf i386\nComponents: main\n",
        );
        assert_eq!(r.suite.as_deref(), Some("noble"));
        assert_eq!(r.architectures, vec!["amd64", "arm64", "armhf", "i386"]);
        assert_eq!(
            ubuntu_timestamp_from_date(r.date.as_deref().unwrap()).as_deref(),
            Some("20260911T182146Z")
        );
        assert_eq!(ubuntu_timestamp_from_date("garbage"), None);
        assert_eq!(
            ubuntu_timestamp_from_date("Sat, 1 Mar 2025 00:00:00 UTC").as_deref(),
            Some("20250301T000000Z")
        );
    }

    fn release_text(suite: &str, arches: &str, date: &str) -> String {
        format!("Suite: {suite}\nCodename: {suite}\nDate: {date}\nArchitectures: {arches}\nComponents: main\n")
    }

    #[test]
    fn discovery_picks_the_newest_date_that_serves_the_codename_and_arch() {
        let index = "<a href=\"2026-09-11/\">x</a><a href=\"2026-08-01/\">y</a><a href=\"2026-07-01/\">z</a>";
        // Newest lacks arm64, the middle one is fine. No gpg in the canned
        // world: the signature step is exercised separately (needs the network).
        let fetch = Canned::default()
            .with(ros_snapshot_index_url("jazzy"), 200, index)
            .with(
                release_url(&ros_snapshot_url("jazzy", "2026-09-11"), "noble"),
                200,
                release_text("noble", "amd64", "Fri, 11 Sep 2026 18:21:46 UTC"),
            )
            .with(
                release_url(&ros_snapshot_url("jazzy", "2026-08-01"), "noble"),
                200,
                release_text("noble", "amd64 arm64", "Sat, 1 Aug 2026 00:00:00 UTC"),
            );
        match discover_ros(&fetch, "jazzy", "noble", "arm64", &mut |_| Ok(())) {
            Ok(s) => {
                assert_eq!(s.date, "2026-08-01");
                assert!(matches!(
                    s.signature,
                    Signature::Verified | Signature::Skipped(_)
                ));
            }
            // With gpg installed the canned world has no Release.gpg: the
            // signature fetch of 2026-08-01 fails and discovery must not pick
            // an unverified date. The error reported is the newest date's.
            Err(e) => assert_eq!(e.key_name(), Some("snapshot.release_mismatch"), "{e}"),
        }
        // Every listed date lacks the architecture: the mismatch is reported.
        let none = discover_ros(&fetch, "jazzy", "noble", "riscv64", &mut |_| Ok(())).unwrap_err();
        assert_eq!(none.key_name(), Some("snapshot.release_mismatch"));
        // No Release for that codename at all.
        let none = discover_ros(&fetch, "jazzy", "jammy", "amd64", &mut |_| Ok(())).unwrap_err();
        assert_eq!(none.key_name(), Some("snapshot.http_status"));
        let empty = Canned::default().with(ros_snapshot_index_url("rolling"), 404, "");
        let e = discover_ros(&empty, "rolling", "noble", "amd64", &mut |_| Ok(())).unwrap_err();
        assert_eq!(e.key_name(), Some("snapshot.http_status"));
    }

    #[test]
    fn ubuntu_discovery_requires_a_matching_inrelease() {
        let ts = "20260911T182146Z";
        let fetch = Canned::default().with(
            inrelease_url(&ubuntu_snapshot_url(ts), "noble"),
            200,
            release_text("noble", "amd64 arm64", "Fri, 11 Sep 2026 18:21:46 UTC"),
        );
        let s =
            discover_ubuntu(&fetch, "noble", ts, Path::new("/nonexistent/keyring.gpg")).unwrap();
        assert_eq!(s.timestamp, ts);
        assert!(matches!(s.signature, Signature::Skipped(_)));
        let e = discover_ubuntu(&fetch, "jammy", ts, Path::new("/nonexistent")).unwrap_err();
        assert_eq!(e.key_name(), Some("snapshot.http_status"));
    }

    /// Needs the network, gpg and gpgv: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn real_ros_snapshot_is_discovered_and_signed() {
        let s = discover_ros(&Http::default(), "jazzy", "noble", "amd64", &mut |_| Ok(())).unwrap();
        assert!(is_date(&s.date), "{}", s.date);
        assert_eq!(s.signature, Signature::Verified);
        let ts = ubuntu_timestamp_from_date(s.release.date.as_deref().unwrap()).unwrap();
        let u = discover_ubuntu(
            &Http::default(),
            "noble",
            &ts,
            Path::new(UBUNTU_ARCHIVE_KEYRING),
        )
        .unwrap();
        assert_eq!(u.timestamp, ts);
    }
}
