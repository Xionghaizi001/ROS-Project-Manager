//! Read-only facts about the host machine (native provider).
//!
//! Nothing in this module modifies the system. Everything that shells out
//! (`dpkg-query`, `git`) does so through a [`Host`] so callers can substitute a
//! synthetic host in tests, and so `check` / `init --from-env` / `doctor` all
//! see the same picture.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use serde::Serialize;

use crate::message::Message;
use crate::platform::Platform;

/// Tools rospm wraps, with the message key of the purpose shown by `doctor`.
pub const TOOLS: &[(&str, &str)] = &[
    ("git", "tool.git"),
    ("vcs", "tool.vcs"),
    ("colcon", "tool.colcon"),
    ("rosdep", "tool.rosdep"),
    ("apt-get", "tool.apt_get"),
    ("dpkg-query", "tool.dpkg_query"),
    ("python3", "tool.python3"),
    ("pixi", "tool.pixi"),
    ("docker", "tool.docker"),
];

/// Tools the native provider cannot work without.
pub const NATIVE_CORE_TOOLS: &[&str] = &["git", "vcs", "colcon", "rosdep"];

/// rosdep's default sources list, written by `sudo rosdep init`.
pub const ROSDEP_DEFAULT_LIST: &str = "/etc/ros/rosdep/sources.list.d/20-default.list";

/// The official ROS 2 apt repository (live; snapshots arrive in P2; mirrors in [`crate::mirror`]).
pub const ROS_APT_URL: &str = crate::mirror::OFFICIAL_ROS_APT;
/// Substring that identifies the ROS 2 repository in any apt source file,
/// whatever its form (`ros2.list` written by hand or by rospm, `ros2.sources`
/// written by the `ros2-apt-source` package).
pub const ROS_APT_NEEDLE: &str = "packages.ros.org/ros2";
/// Where the official install guide puts the signing key.
pub const ROS_APT_KEYRING: &str = "/usr/share/keyrings/ros-archive-keyring.gpg";
/// Where rospm writes the source entry (same path as the official guide).
pub const ROS_APT_LIST: &str = "/etc/apt/sources.list.d/ros2.list";
/// Signing key for `packages.ros.org` (dearmored; the snapshot key differs, see P2).
/// Mirrors serve the same file under `<rosdistro>/ros.key` ([`crate::mirror::Mirror::ros_key_url`]).
pub const ROS_KEY_URL: &str = "https://raw.githubusercontent.com/ros/rosdistro/master/ros.key";
/// Full fingerprint of the Open Robotics signing key (`info@osrfoundation.org`,
/// RSA 4096, created 2019-05-30, valid until 2030-06-01). `apply` refuses a
/// downloaded `ros.key` whose fingerprint differs.
pub const ROS_KEY_FINGERPRINT: &str = "C1CF6E31E6BADE8868B172B4F42ED6FBAB17C654";
/// Where rospm writes the Ubuntu snapshot source entries (`apply --locked`,
/// P2); a file of its own, never Ubuntu's own `sources.list` / `ubuntu.sources`
/// (decision #17).
pub const UBUNTU_SNAPSHOT_LIST: &str = "/etc/apt/sources.list.d/rospm-ubuntu-snapshot.list";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct OsRelease {
    pub id: Option<String>,
    pub version_id: Option<String>,
    pub codename: Option<String>,
    pub pretty_name: Option<String>,
}

impl OsRelease {
    pub fn parse(text: &str) -> OsRelease {
        let mut os = OsRelease::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim().trim_matches('"').to_string();
            match key.trim() {
                "ID" => os.id = Some(value),
                "VERSION_ID" => os.version_id = Some(value),
                "VERSION_CODENAME" | "UBUNTU_CODENAME" if os.codename.is_none() => {
                    os.codename = Some(value)
                }
                "PRETTY_NAME" => os.pretty_name = Some(value),
                _ => {}
            }
        }
        os
    }

    pub fn load() -> Option<OsRelease> {
        fs::read_to_string("/etc/os-release")
            .ok()
            .map(|t| OsRelease::parse(&t))
    }

    pub fn is_ubuntu(&self) -> bool {
        self.id.as_deref() == Some("ubuntu")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RosdepState {
    /// `sudo rosdep init` has been run (default sources list exists).
    pub initialized: bool,
    /// `rosdep update` has been run for this user (sources cache exists).
    pub updated: bool,
    pub cache_dir: PathBuf,
}

impl RosdepState {
    pub fn probe(ros_home: &Path) -> RosdepState {
        let cache_dir = ros_home.join("rosdep").join("sources.cache");
        RosdepState {
            initialized: Path::new(ROSDEP_DEFAULT_LIST).is_file(),
            updated: cache_dir.join("index").is_file(),
            cache_dir,
        }
    }
}

/// What apt is configured to pull from, as far as ROS needs to know.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AptSources {
    /// First source file that references a ROS 2 apt repository (official or
    /// a known mirror, see [`crate::mirror`]), if any.
    pub ros2: Option<PathBuf>,
    /// Name of the source set that file points at (`official`, `tuna`, …).
    pub ros2_mirror: Option<&'static str>,
    /// The signing key at [`ROS_APT_KEYRING`] exists.
    pub keyring: bool,
    /// `universe` is enabled in some source. `None` when no source file
    /// could be read (non-Debian host).
    pub universe: Option<bool>,
    /// Base URL of a ROS snapshot (`http://snapshots.ros.org/<distro>/<date>/ubuntu`)
    /// found in whichever file `ros2` points at, detected by the
    /// `snapshots.ros.org` substring (`apply --locked`, P2).
    #[serde(default)]
    pub ros2_snapshot: Option<String>,
    /// The signing key at [`crate::snapshot::ROS_SNAPSHOT_KEYRING`] exists.
    #[serde(default)]
    pub snapshot_keyring: bool,
    /// That file is ASCII-armoured, which apt rejects at a `*.gpg` path
    /// (written by rospm before the 2026-09-26 fix): `apply` reinstalls it,
    /// `uninstall` still removes it.
    #[serde(default)]
    pub snapshot_keyring_armored: bool,
    /// Base URL of the Ubuntu snapshot (`https://snapshot.ubuntu.com/ubuntu/<timestamp>`)
    /// if rospm's own [`UBUNTU_SNAPSHOT_LIST`] file exists.
    #[serde(default)]
    pub ubuntu_snapshot: Option<String>,
    /// The signing key at [`crate::snapshot::UBUNTU_ARCHIVE_KEYRING`] exists
    /// (shipped by the `ubuntu-keyring` package; usually present, but the
    /// Ubuntu snapshot file still gets written when it is not — apt will
    /// complain loudly, which is clearer than rospm silently refusing).
    #[serde(default)]
    pub ubuntu_archive_keyring: bool,
}

/// The file exists and is ASCII-armoured: unusable for apt at a `*.gpg`
/// path (NO_PUBKEY), so `apply` must reinstall it.
fn is_armored_file(path: &Path) -> bool {
    fs::read(path).is_ok_and(|bytes| crate::plan::is_ascii_armored(&bytes))
}

impl AptSources {
    pub fn probe() -> AptSources {
        Self::probe_in(
            Path::new("/etc/apt"),
            Path::new(ROS_APT_KEYRING),
            Path::new(crate::snapshot::ROS_SNAPSHOT_KEYRING),
            Path::new(crate::snapshot::UBUNTU_ARCHIVE_KEYRING),
        )
    }

    /// Scan `<etc_apt>/sources.list` and `<etc_apt>/sources.list.d/*`.
    pub fn probe_in(
        etc_apt: &Path,
        keyring: &Path,
        snapshot_keyring: &Path,
        ubuntu_archive_keyring: &Path,
    ) -> AptSources {
        let mut files: Vec<PathBuf> = vec![etc_apt.join("sources.list")];
        if let Ok(rd) = fs::read_dir(etc_apt.join("sources.list.d")) {
            let mut extra: Vec<PathBuf> = rd
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    matches!(
                        p.extension().and_then(|e| e.to_str()),
                        Some("list") | Some("sources")
                    )
                })
                .collect();
            extra.sort();
            files.extend(extra);
        }
        let mut out = AptSources {
            ros2: None,
            ros2_mirror: None,
            keyring: keyring.is_file(),
            universe: None,
            ros2_snapshot: None,
            snapshot_keyring: snapshot_keyring.is_file(),
            snapshot_keyring_armored: is_armored_file(snapshot_keyring),
            ubuntu_snapshot: None,
            ubuntu_archive_keyring: ubuntu_archive_keyring.is_file(),
        };
        let ubuntu_snapshot_name = Path::new(UBUNTU_SNAPSHOT_LIST).file_name();
        for file in files {
            let Ok(text) = fs::read_to_string(&file) else {
                continue;
            };
            if out.universe.is_none() {
                out.universe = Some(false);
            }
            if out.ros2.is_none() {
                if let Some(m) = crate::mirror::mirror_for_apt_source(&text) {
                    out.ros2 = Some(file.clone());
                    out.ros2_mirror = Some(m.name);
                }
            }
            if out.ros2_snapshot.is_none() {
                if let Some(base) = extract_url_containing(&text, "snapshots.ros.org") {
                    out.ros2_snapshot = Some(base);
                    if out.ros2.is_none() {
                        out.ros2 = Some(file.clone());
                    }
                }
            }
            if out.ubuntu_snapshot.is_none() && file.file_name() == ubuntu_snapshot_name {
                out.ubuntu_snapshot = extract_url_containing(&text, "snapshot.ubuntu.com/ubuntu");
            }
            if source_text_enables_universe(&text) {
                out.universe = Some(true);
            }
        }
        out
    }
}

/// The first whitespace-separated token on a non-comment line that contains
/// `needle`, e.g. the snapshot base URL in a `deb [...] <url> <suite> main`
/// line.
fn extract_url_containing(text: &str, needle: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| {
            l.split_whitespace()
                .find(|w| w.contains(needle))
                .map(str::to_string)
        })
}

/// Whether an apt source file (one-line or deb822) has a non-comment line
/// containing `needle`.
pub fn source_text_mentions(text: &str, needle: &str) -> bool {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .any(|l| l.contains(needle))
}

/// Whether an apt source file enables the `universe` component, in either
/// the one-line (`deb … main universe`) or deb822 (`Components: main universe`) form.
pub fn source_text_enables_universe(text: &str) -> bool {
    text.lines().map(str::trim).any(|l| {
        let one_line = l.starts_with("deb ") || l.starts_with("deb-src ");
        let deb822 = l.starts_with("Components:");
        (one_line || deb822) && l.split_whitespace().any(|w| w == "universe")
    })
}

/// Debian/Ubuntu's system-wide locale defaults, read by PAM at login and
/// written by `update-locale`.
pub const DEFAULT_LOCALE_FILE: &str = "/etc/default/locale";

/// The variables that decide the character encoding, in the order glibc
/// consults them (also how ROS's install guide judges "is UTF-8").
const LOCALE_VARS: [&str; 3] = ["LC_ALL", "LC_CTYPE", "LANG"];

/// Whether a locale name (`zh_CN.UTF-8`, `C.utf8`, `POSIX`, …) selects UTF-8.
/// `None` (unset) means POSIX, which is not.
pub fn locale_is_utf8(value: Option<&str>) -> bool {
    value.is_some_and(|v| {
        let v = v.to_ascii_lowercase();
        v.contains("utf-8") || v.contains("utf8")
    })
}

/// The first of `LC_ALL` / `LC_CTYPE` / `LANG` that `get` resolves to a
/// non-empty value, with the variable it came from.
fn pick_locale(get: impl Fn(&str) -> Option<String>) -> Option<(&'static str, String)> {
    LOCALE_VARS
        .iter()
        .find_map(|k| get(k).filter(|v| !v.is_empty()).map(|v| (*k, v)))
}

/// The locale ROS needs to be UTF-8, seen from the two places it can come
/// from. `apply` only ever fixes the *system* default: the process value is
/// whatever the caller chose (`LANG=C rospm apply` in a script or CI must not
/// rewrite `/etc/default/locale`, which happened before this split).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LocaleState {
    /// [`DEFAULT_LOCALE_FILE`] exists and was readable.
    pub system_file: bool,
    /// What that file selects (`LC_ALL`, then `LC_CTYPE`, then `LANG`).
    /// `None` when the file is unreadable or sets none of them (POSIX).
    pub system: Option<String>,
    /// Which variable in the file `system` came from.
    pub system_var: Option<&'static str>,
    /// The same lookup in the current process environment.
    pub process: Option<String>,
}

impl LocaleState {
    pub fn probe() -> LocaleState {
        Self::probe_in(Path::new(DEFAULT_LOCALE_FILE))
    }

    /// Read `file` as the system default and the environment as the process
    /// locale.
    pub fn probe_in(file: &Path) -> LocaleState {
        let mut out = match fs::read_to_string(file) {
            Ok(text) => Self::from_default_locale(&text),
            Err(_) => LocaleState::default(),
        };
        out.process = pick_locale(|k| std::env::var(k).ok()).map(|(_, v)| v);
        out
    }

    /// Parse the contents of `/etc/default/locale` (`KEY=value` lines, values
    /// optionally quoted, `#` comments). The process side is left unset.
    pub fn from_default_locale(text: &str) -> LocaleState {
        let vars: BTreeMap<String, String> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| {
                let v = v.trim();
                let v = v
                    .strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .or_else(|| v.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                    .unwrap_or(v);
                (k.trim().to_string(), v.to_string())
            })
            .collect();
        let (system_var, system) = match pick_locale(|k| vars.get(k).cloned()) {
            Some((var, value)) => (Some(var), Some(value)),
            None => (None, None),
        };
        LocaleState {
            system_file: true,
            system,
            system_var,
            process: None,
        }
    }

    /// A host whose `/etc/default/locale` sets `LANG=<system>`; for tests.
    pub fn with_system(system: &str) -> LocaleState {
        LocaleState {
            system_file: true,
            system: Some(system.to_string()),
            system_var: Some("LANG"),
            process: None,
        }
    }

    /// Whether the system default is UTF-8; `None` when there is no readable
    /// [`DEFAULT_LOCALE_FILE`].
    pub fn system_utf8(&self) -> Option<bool> {
        self.system_file
            .then(|| locale_is_utf8(self.system.as_deref()))
    }

    /// Whether the current process locale is UTF-8.
    pub fn process_utf8(&self) -> bool {
        locale_is_utf8(self.process.as_deref())
    }

    /// Whether `apply` has to set a UTF-8 system locale: the system default is
    /// not UTF-8, or there is no system file and the process locale is not
    /// UTF-8 either (a bare container, where the process value is all there is).
    pub fn needs_fix(&self) -> bool {
        !self.system_utf8().unwrap_or_else(|| self.process_utf8())
    }
}

/// `$ROS_HOME`, defaulting to `~/.ros` like rosdep does.
pub fn ros_home() -> PathBuf {
    if let Some(h) = std::env::var_os("ROS_HOME") {
        return PathBuf::from(h);
    }
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".ros"))
        .unwrap_or_else(|| PathBuf::from(".ros"))
}

/// Conda facts that need no subprocess: a `conda` executable on PATH (the
/// shell function from `conda init` is not visible here) and the per-user
/// directories rospm owns for its own Miniforge and downloads.
#[derive(Debug, Clone)]
pub struct CondaHost {
    pub on_path: Option<PathBuf>,
    /// `$XDG_DATA_HOME/rospm`, default `~/.local/share/rospm`.
    pub data_dir: PathBuf,
    /// `$XDG_CACHE_HOME/rospm`, default `~/.cache/rospm`.
    pub cache_dir: PathBuf,
}

impl CondaHost {
    pub fn probe() -> CondaHost {
        CondaHost {
            on_path: which::which("conda").ok(),
            data_dir: xdg_dir("XDG_DATA_HOME", ".local/share").join("rospm"),
            cache_dir: xdg_dir("XDG_CACHE_HOME", ".cache").join("rospm"),
        }
    }
}

/// An absolute `$var`, else `~/<default>` (relative XDG values are invalid
/// per the base directory specification and are ignored).
fn xdg_dir(var: &str, default: &str) -> PathBuf {
    if let Some(dir) = std::env::var_os(var).map(PathBuf::from) {
        if dir.is_absolute() {
            return dir;
        }
    }
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(default))
        .unwrap_or_else(|| PathBuf::from(default))
}

/// ROS distros installed under `/opt/ros` (directories with a `setup.bash`).
pub fn installed_ros_distros(opt_ros: &Path) -> Vec<String> {
    let mut out: Vec<String> = fs::read_dir(opt_ros)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().join("setup.bash").is_file())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageStatus {
    pub installed: bool,
    pub version: Option<String>,
}

/// Where installed-package questions are answered.
#[derive(Debug, Clone)]
pub enum PackageDb {
    /// Shell out to this `dpkg-query` binary.
    DpkgQuery(PathBuf),
    /// Fixed answers (tests, and later: offline bundles).
    Static(BTreeMap<String, PackageStatus>),
    /// No way to tell (`dpkg-query` missing, non-Debian host).
    Unavailable,
}

/// Parse `dpkg-query -W -f='${binary:Package}\t${db:Status-Status}\t${Version}\n'`.
/// Packages dpkg has never heard of are simply absent from the output.
pub fn parse_dpkg_query(stdout: &str) -> BTreeMap<String, PackageStatus> {
    let mut out = BTreeMap::new();
    for line in stdout.lines() {
        let mut cols = line.split('\t');
        let (Some(name), Some(status)) = (cols.next(), cols.next()) else {
            continue;
        };
        let version = cols.next().map(str::trim).filter(|v| !v.is_empty());
        // `binary:Package` includes `:arch` for foreign-arch packages; strip it.
        let name = name.split(':').next().unwrap_or(name).trim().to_string();
        if name.is_empty() {
            continue;
        }
        out.insert(
            name,
            PackageStatus {
                installed: status.trim() == "installed",
                version: version.map(str::to_string),
            },
        );
    }
    out
}

impl PackageDb {
    /// Status of every package in `names`. `None` when the database is
    /// unavailable; packages unknown to the database are reported as not installed.
    pub fn status(&self, names: &[String]) -> Option<BTreeMap<String, PackageStatus>> {
        let not_installed = || PackageStatus {
            installed: false,
            version: None,
        };
        let found: BTreeMap<String, PackageStatus> = match self {
            PackageDb::Unavailable => return None,
            PackageDb::Static(map) => map.clone(),
            PackageDb::DpkgQuery(bin) => {
                if names.is_empty() {
                    return Some(BTreeMap::new());
                }
                // Exit status is non-zero when any package is unknown, but the
                // known ones are still printed, so only the output matters.
                let output = process::Command::new(bin)
                    .arg("-W")
                    .arg("-f=${binary:Package}\t${db:Status-Status}\t${Version}\n")
                    .args(names)
                    .stderr(process::Stdio::null())
                    .output()
                    .ok()?;
                parse_dpkg_query(&String::from_utf8_lossy(&output.stdout))
            }
        };
        Some(
            names
                .iter()
                .map(|n| {
                    (
                        n.clone(),
                        found.get(n).cloned().unwrap_or_else(not_installed),
                    )
                })
                .collect(),
        )
    }

    /// Installed packages whose name starts with `prefix`, sorted. `None`
    /// when the database is unavailable. Used by `uninstall` to name every
    /// package explicitly instead of handing apt a pattern.
    pub fn installed_with_prefix(&self, prefix: &str) -> Option<Vec<String>> {
        let found: BTreeMap<String, PackageStatus> = match self {
            PackageDb::Unavailable => return None,
            PackageDb::Static(map) => map.clone(),
            PackageDb::DpkgQuery(bin) => {
                let output = process::Command::new(bin)
                    .arg("-W")
                    .arg("-f=${binary:Package}\t${db:Status-Status}\t${Version}\n")
                    .arg(format!("{prefix}*"))
                    .stderr(process::Stdio::null())
                    .output()
                    .ok()?;
                parse_dpkg_query(&String::from_utf8_lossy(&output.stdout))
            }
        };
        Some(
            found
                .into_iter()
                .filter(|(name, st)| st.installed && name.starts_with(prefix))
                .map(|(name, _)| name)
                .collect(),
        )
    }
}

/// Snapshot of the host, gathered once per command.
#[derive(Debug, Clone)]
pub struct Host {
    pub platform: Option<Platform>,
    pub os: Option<OsRelease>,
    /// Usually `/opt/ros`; overridable for tests.
    pub opt_ros: PathBuf,
    pub ros_distros: Vec<String>,
    pub ros_home: PathBuf,
    pub rosdep: RosdepState,
    pub apt_sources: AptSources,
    /// System and process locale (ROS requires UTF-8; `apply` fixes the
    /// system default when it is not).
    pub locale: LocaleState,
    pub tools: BTreeMap<String, Option<PathBuf>>,
    pub packages: PackageDb,
    /// Unix time `/var/lib/dpkg/status` last changed, i.e. the last package
    /// install / removal. `lock` takes the Ubuntu snapshot at this instant:
    /// what the host installed from the live archive was available then.
    pub dpkg_changed_at: Option<u64>,
    /// Where Conda can come from for a managed environment.
    pub conda: CondaHost,
}

impl Host {
    pub fn probe() -> Host {
        let tools: BTreeMap<String, Option<PathBuf>> = TOOLS
            .iter()
            .map(|(t, _)| (t.to_string(), which::which(t).ok()))
            .collect();
        let packages = match tools.get("dpkg-query").cloned().flatten() {
            Some(bin) => PackageDb::DpkgQuery(bin),
            None => PackageDb::Unavailable,
        };
        let ros_home = ros_home();
        let opt_ros = PathBuf::from("/opt/ros");
        Host {
            platform: Platform::current(),
            os: OsRelease::load(),
            ros_distros: installed_ros_distros(&opt_ros),
            opt_ros,
            rosdep: RosdepState::probe(&ros_home),
            ros_home,
            apt_sources: AptSources::probe(),
            locale: LocaleState::probe(),
            tools,
            packages,
            dpkg_changed_at: fs::metadata("/var/lib/dpkg/status")
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs()),
            conda: CondaHost::probe(),
        }
    }

    /// A host with nothing installed and no way to query packages. Tests start
    /// from this and fill in what they need.
    pub fn empty() -> Host {
        Host {
            platform: Platform::current(),
            os: None,
            opt_ros: PathBuf::from("/opt/ros"),
            ros_distros: Vec::new(),
            ros_home: PathBuf::from("/nonexistent/.ros"),
            rosdep: RosdepState::default(),
            apt_sources: AptSources::default(),
            locale: LocaleState::default(),
            tools: TOOLS.iter().map(|(t, _)| (t.to_string(), None)).collect(),
            packages: PackageDb::Unavailable,
            dpkg_changed_at: None,
            conda: CondaHost {
                on_path: None,
                data_dir: PathBuf::from("/nonexistent/.local/share/rospm"),
                cache_dir: PathBuf::from("/nonexistent/.cache/rospm"),
            },
        }
    }

    /// Whether the host's Ubuntu release matches `want`. `None` when
    /// `/etc/os-release` was unreadable.
    pub fn ubuntu_matches(&self, want: &str) -> Option<bool> {
        self.os
            .as_ref()
            .map(|os| os.is_ubuntu() && os.version_id.as_deref() == Some(want))
    }

    pub fn tool(&self, name: &str) -> Option<&Path> {
        self.tools.get(name).and_then(|p| p.as_deref())
    }

    pub fn has_tool(&self, name: &str) -> bool {
        self.tool(name).is_some()
    }

    pub fn ros_setup_file(&self, distro: &str) -> PathBuf {
        self.opt_ros.join(distro).join("setup.bash")
    }

    pub fn has_ros_distro(&self, distro: &str) -> bool {
        self.ros_distros.iter().any(|d| d == distro)
    }

    /// Whether `colcon mixin` works. The `mixin` verb is a separate package
    /// (`python3-colcon-mixin`) that `python3-colcon-common-extensions` from
    /// the ROS apt repository does not pull in, so a manifest that names
    /// mixins fails at `colcon build --mixin …` unless it is installed.
    /// `None` when colcon itself is missing.
    pub fn colcon_mixin_available(&self) -> Option<bool> {
        let colcon = self.tool("colcon")?;
        let status = process::Command::new(colcon)
            .args(["mixin", "show"])
            .stdin(process::Stdio::null())
            .stdout(process::Stdio::null())
            .stderr(process::Stdio::null())
            .status()
            .ok()?;
        Some(status.success())
    }

    /// `git -C <dir> remote get-url origin`, if git is available and `dir` is a checkout.
    pub fn git_remote_url(&self, dir: &Path) -> Option<String> {
        let git = self.tool("git")?;
        let out = process::Command::new(git)
            .arg("-C")
            .arg(dir)
            .args(["remote", "get-url", "origin"])
            .stderr(process::Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!url.is_empty()).then_some(url)
    }

    /// `git -C <dir> rev-parse HEAD`.
    pub fn git_head(&self, dir: &Path) -> Option<String> {
        let git = self.tool("git")?;
        let out = process::Command::new(git)
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "HEAD"])
            .stderr(process::Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!sha.is_empty()).then_some(sha)
    }
}

/// Package name of a pip requirement (`numpy>=1.26`, `foo[extra]==1`,
/// `Bar_baz`), normalised per PEP 503: lower-case, runs of `-`/`_`/`.` → `-`.
pub fn pip_requirement_name(requirement: &str) -> String {
    let raw = requirement
        .trim()
        .split(|c: char| c.is_whitespace() || "<>=!~;[@".contains(c))
        .next()
        .unwrap_or("");
    let mut out = String::with_capacity(raw.len());
    let mut sep = false;
    for c in raw.chars() {
        if c == '-' || c == '_' || c == '.' {
            sep = true;
        } else {
            if sep && !out.is_empty() {
                out.push('-');
            }
            sep = false;
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// Parse `pip freeze` / `pip list --format=freeze` output (`name==version`
/// lines) into normalised name → version. Editable / URL lines are skipped.
pub fn parse_pip_freeze(stdout: &str) -> BTreeMap<String, String> {
    stdout
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') || l.starts_with("-e ") {
                return None;
            }
            let (name, version) = l.split_once("==")?;
            Some((pip_requirement_name(name), version.trim().to_string()))
        })
        .collect()
}

impl Host {
    /// `python3 -m pip list --format=freeze`: every installed distribution
    /// with its version, or `None` when python3 / pip are unavailable.
    /// Recorded by `lock`, compared by `verify`, never installed (decision #6).
    pub fn pip_freeze(&self) -> Option<BTreeMap<String, String>> {
        let python = self.tool("python3")?;
        let out = process::Command::new(python)
            .args([
                "-m",
                "pip",
                "list",
                "--format=freeze",
                "--disable-pip-version-check",
            ])
            .stdin(process::Stdio::null())
            .stderr(process::Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(parse_pip_freeze(&String::from_utf8_lossy(&out.stdout)))
    }
}

/// Whether `dir` is the top level of a git checkout (a `.git` directory or
/// worktree file).
pub fn is_git_checkout(dir: &Path) -> bool {
    dir.join(".git").exists()
}

// ---------------------------------------------------------------- environment report

/// Outcome of comparing one expectation against the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Host matches the manifest.
    Ok,
    /// Something the manifest needs is absent.
    Missing,
    /// Present but different from what the manifest declares.
    Mismatch,
    /// Could not be determined (probe tool unavailable).
    Unknown,
    /// Not a requirement, shown for orientation (e.g. "workspace not built yet").
    Info,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Missing => "missing",
            Status::Mismatch => "mismatch",
            Status::Unknown => "unknown",
            Status::Info => "info",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvItem {
    /// Coarse grouping: `platform`, `os`, `tool`, `ros`, `rosdep`, `apt`, `source`, `workspace`.
    pub area: &'static str,
    /// What is being compared (a package, a repo path, a workspace name).
    pub subject: Message,
    pub expected: Message,
    pub actual: Message,
    pub status: Status,
}

/// Expected vs actual, one line per fact. Produced by `check`, never acts.
#[derive(Debug, Clone, Default, Serialize)]
pub struct EnvReport {
    pub items: Vec<EnvItem>,
}

impl EnvReport {
    pub fn push(
        &mut self,
        area: &'static str,
        subject: impl Into<Message>,
        expected: impl Into<Message>,
        actual: impl Into<Message>,
        status: Status,
    ) {
        self.items.push(EnvItem {
            area,
            subject: subject.into(),
            expected: expected.into(),
            actual: actual.into(),
            status,
        });
    }

    pub fn count(&self, status: Status) -> usize {
        self.items.iter().filter(|i| i.status == status).count()
    }

    /// Missing + mismatch.
    pub fn problems(&self) -> usize {
        self.count(Status::Missing) + self.count(Status::Mismatch)
    }

    pub fn unknown(&self) -> usize {
        self.count(Status::Unknown)
    }

    /// Everything the manifest requires is present and nothing is undetermined.
    pub fn ready(&self) -> bool {
        self.problems() == 0 && self.unknown() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_release_parses_ubuntu() {
        let os = OsRelease::parse(
            "PRETTY_NAME=\"Ubuntu 22.04.5 LTS\"\nNAME=\"Ubuntu\"\nVERSION_ID=\"22.04\"\nID=ubuntu\nVERSION_CODENAME=jammy\nUBUNTU_CODENAME=jammy\n",
        );
        assert!(os.is_ubuntu());
        assert_eq!(os.version_id.as_deref(), Some("22.04"));
        assert_eq!(os.codename.as_deref(), Some("jammy"));
    }

    #[test]
    fn default_locale_file_parses_like_glibc_reads_it() {
        // Ubuntu's usual file: LANG set, LC_ALL present but empty (ignored).
        let l = LocaleState::from_default_locale("LANG=zh_CN.UTF-8\n#LANGUAGE=zh_CN:zh\nLC_ALL=\n");
        assert_eq!(l.system.as_deref(), Some("zh_CN.UTF-8"));
        assert_eq!(l.system_var, Some("LANG"));
        assert_eq!(l.system_utf8(), Some(true));
        assert!(!l.needs_fix());

        // Quoted values; LC_ALL wins over LANG and can make the system non-UTF-8.
        let l = LocaleState::from_default_locale("LANG=\"en_US.UTF-8\"\nLC_ALL='C'\n");
        assert_eq!(l.system.as_deref(), Some("C"));
        assert_eq!(l.system_var, Some("LC_ALL"));
        assert_eq!(l.system_utf8(), Some(false));
        assert!(l.needs_fix());

        // File exists but selects nothing: POSIX.
        let l = LocaleState::from_default_locale("#  File generated by update-locale\n");
        assert!(l.system_file);
        assert_eq!(l.system, None);
        assert_eq!(l.system_utf8(), Some(false));
        assert!(l.needs_fix());

        assert!(locale_is_utf8(Some("C.utf8")));
        assert!(!locale_is_utf8(Some("POSIX")));
        assert!(!locale_is_utf8(None));
    }

    #[test]
    fn process_locale_only_matters_without_a_system_file() {
        // No /etc/default/locale (bare container): the process decides.
        let mut l = LocaleState::default();
        assert_eq!(l.system_utf8(), None);
        assert!(l.needs_fix());
        l.process = Some("C.UTF-8".into());
        assert!(!l.needs_fix());

        // With a UTF-8 system default, `LANG=C rospm apply` changes nothing.
        let mut l = LocaleState::with_system("en_US.UTF-8");
        l.process = Some("C".into());
        assert!(!l.process_utf8());
        assert!(!l.needs_fix());

        // And a UTF-8 shell does not excuse a POSIX system default.
        let mut l = LocaleState::with_system("C");
        l.process = Some("en_US.UTF-8".into());
        assert!(l.needs_fix());
    }

    #[test]
    fn probe_in_reads_the_file_and_falls_back_to_the_environment() {
        let dir = std::env::temp_dir().join(format!("rospm-locale-{}", process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("locale");
        fs::write(&file, "LANG=en_US.UTF-8\n").unwrap();
        let l = LocaleState::probe_in(&file);
        assert_eq!(l.system_utf8(), Some(true));
        let l = LocaleState::probe_in(&dir.join("missing"));
        assert!(!l.system_file);
        assert_eq!(l.system_utf8(), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dpkg_output_parses_and_missing_packages_are_not_installed() {
        let parsed = parse_dpkg_query(
            "curl\tinstalled\t7.81.0-1ubuntu1.27\ngit\tinstalled\t1:2.34.1-1ubuntu1.17\nlibfoo:amd64\tconfig-files\t1.0\n",
        );
        assert!(parsed["git"].installed);
        assert_eq!(
            parsed["curl"].version.as_deref(),
            Some("7.81.0-1ubuntu1.27")
        );
        assert!(!parsed["libfoo"].installed);

        let db = PackageDb::Static(parsed);
        let status = db.status(&["git".to_string(), "nope".to_string()]).unwrap();
        assert!(status["git"].installed);
        assert!(!status["nope"].installed);
        assert!(PackageDb::Unavailable.status(&["git".into()]).is_none());
    }

    #[test]
    fn pip_names_normalise_and_freeze_parses() {
        assert_eq!(pip_requirement_name("numpy>=1.26"), "numpy");
        assert_eq!(
            pip_requirement_name("Foo_Bar.baz[extra]==1.0"),
            "foo-bar-baz"
        );
        assert_eq!(
            pip_requirement_name("  ruamel.yaml ; python_version<'3'"),
            "ruamel-yaml"
        );
        let f = parse_pip_freeze("numpy==1.26.4\nPyYAML==6.0.1\n-e git+https://x#egg=y\n\n");
        assert_eq!(f["numpy"], "1.26.4");
        assert_eq!(f["pyyaml"], "6.0.1");
        assert_eq!(f.len(), 2);
    }

    #[test]
    fn apt_source_text_detection() {
        let one_line = "# comment packages.ros.org/ros2\ndeb [arch=amd64 signed-by=/k.gpg] http://packages.ros.org/ros2/ubuntu noble main\n";
        assert!(source_text_mentions(one_line, ROS_APT_NEEDLE));
        assert!(!source_text_mentions(
            "# packages.ros.org/ros2 only in a comment\n",
            ROS_APT_NEEDLE
        ));
        assert!(!source_text_enables_universe(one_line));

        let ubuntu_deb822 = "Types: deb\nURIs: http://archive.ubuntu.com/ubuntu/\nSuites: noble noble-updates\nComponents: main universe restricted\n";
        assert!(source_text_enables_universe(ubuntu_deb822));
        assert!(source_text_enables_universe(
            "deb http://archive.ubuntu.com/ubuntu jammy main universe\n"
        ));
        assert!(!source_text_enables_universe(
            "deb http://archive.ubuntu.com/ubuntu jammy main\n# deb … universe\n"
        ));
    }

    #[test]
    fn apt_sources_probe_scans_a_directory() {
        let dir = std::env::temp_dir().join(format!("rospm-apt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sources.list.d")).unwrap();
        fs::write(
            dir.join("sources.list"),
            "deb http://archive.ubuntu.com/ubuntu noble main universe\n",
        )
        .unwrap();
        fs::write(
            dir.join("sources.list.d/ros2.list"),
            "deb [signed-by=/k] http://packages.ros.org/ros2/ubuntu noble main\n",
        )
        .unwrap();
        fs::write(
            dir.join("sources.list.d/ignored.txt"),
            "packages.ros.org/ros2\n",
        )
        .unwrap();
        let probed = AptSources::probe_in(
            &dir,
            &dir.join("missing-keyring.gpg"),
            &dir.join("missing-snapshot-keyring.gpg"),
            &dir.join("missing-ubuntu-keyring.gpg"),
        );
        assert_eq!(probed.ros2, Some(dir.join("sources.list.d/ros2.list")));
        assert_eq!(probed.universe, Some(true));
        assert!(!probed.keyring);
        assert_eq!(probed.ros2_snapshot, None);
        assert!(!probed.snapshot_keyring);
        assert_eq!(probed.ubuntu_snapshot, None);
        assert!(!probed.ubuntu_archive_keyring);

        let none = AptSources::probe_in(&dir.join("nope"), &dir, &dir, &dir);
        assert_eq!(none.ros2, None);
        assert_eq!(none.universe, None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn apt_sources_probe_detects_a_ros_snapshot_and_its_keyring() {
        let dir = std::env::temp_dir().join(format!("rospm-apt-snap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sources.list.d")).unwrap();
        fs::write(
            dir.join("sources.list.d/ros2.list"),
            "deb [arch=amd64 signed-by=/usr/share/keyrings/ros2-snapshots-archive-keyring.gpg] http://snapshots.ros.org/jazzy/2026-09-11/ubuntu noble main\n",
        )
        .unwrap();
        let keyring = dir.join("ros2-snapshots-archive-keyring.gpg");
        fs::write(&keyring, [0x99u8, 0x02, 0x0d, 0x04]).unwrap();
        let probed = AptSources::probe_in(
            &dir,
            &dir.join("missing-ros-keyring.gpg"),
            &keyring,
            &dir.join("missing-ubuntu-keyring.gpg"),
        );
        assert_eq!(probed.ros2, Some(dir.join("sources.list.d/ros2.list")));
        assert_eq!(probed.ros2_mirror, None);
        assert_eq!(
            probed.ros2_snapshot.as_deref(),
            Some("http://snapshots.ros.org/jazzy/2026-09-11/ubuntu")
        );
        assert!(probed.snapshot_keyring);
        // An armoured file at a `*.gpg` path is unusable for apt (NO_PUBKEY):
        // still present (uninstall removes it) but flagged for reinstall.
        fs::write(&keyring, "-----BEGIN PGP PUBLIC KEY BLOCK-----\n").unwrap();
        let armoured = AptSources::probe_in(
            &dir,
            &dir.join("missing-ros-keyring.gpg"),
            &keyring,
            &dir.join("missing-ubuntu-keyring.gpg"),
        );
        assert!(armoured.snapshot_keyring);
        assert!(armoured.snapshot_keyring_armored);
        assert!(!probed.snapshot_keyring_armored);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn apt_sources_probe_detects_rospms_ubuntu_snapshot_file() {
        let dir = std::env::temp_dir().join(format!("rospm-apt-usnap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sources.list.d")).unwrap();
        fs::write(
            dir.join("sources.list.d/rospm-ubuntu-snapshot.list"),
            "deb [arch=amd64 signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] https://snapshot.ubuntu.com/ubuntu/20260911T182146Z noble main restricted universe multiverse\n",
        )
        .unwrap();
        let ubuntu_keyring = dir.join("ubuntu-archive-keyring.gpg");
        fs::write(&ubuntu_keyring, b"").unwrap();
        let probed = AptSources::probe_in(
            &dir,
            &dir.join("missing"),
            &dir.join("missing"),
            &ubuntu_keyring,
        );
        assert_eq!(
            probed.ubuntu_snapshot.as_deref(),
            Some("https://snapshot.ubuntu.com/ubuntu/20260911T182146Z")
        );
        assert!(probed.ubuntu_archive_keyring);
        let none = AptSources::probe_in(&dir.join("nope"), &dir, &dir, &dir.join("missing"));
        assert_eq!(none.ubuntu_snapshot, None);
        assert!(!none.ubuntu_archive_keyring);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn installed_with_prefix_filters_static_db() {
        let db = PackageDb::Static(parse_dpkg_query(
            "ros-jazzy-rclcpp\tinstalled\t1\nros-jazzy-ros-base\tinstalled\t1\nros-jazzy-old\tconfig-files\t1\nros-humble-rclcpp\tinstalled\t1\n",
        ));
        assert_eq!(
            db.installed_with_prefix("ros-jazzy-").unwrap(),
            vec![
                "ros-jazzy-rclcpp".to_string(),
                "ros-jazzy-ros-base".to_string()
            ]
        );
        assert!(PackageDb::Unavailable
            .installed_with_prefix("ros-")
            .is_none());
    }

    #[test]
    fn report_counts() {
        let mut r = EnvReport::default();
        let t = Message::text;
        r.push("os", t("ubuntu"), t("24.04"), t("22.04"), Status::Mismatch);
        r.push(
            "tool",
            t("git"),
            t("present"),
            t("/usr/bin/git"),
            Status::Ok,
        );
        r.push("apt", t("x"), t("installed"), t("?"), Status::Unknown);
        r.push("workspace", t("main"), t("-"), t("not built"), Status::Info);
        assert_eq!(r.problems(), 1);
        assert_eq!(r.unknown(), 1);
        assert!(!r.ready());
    }
}
