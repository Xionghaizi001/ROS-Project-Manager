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

use crate::platform::Platform;

/// Tools rospm wraps, with the purpose shown by `doctor`.
pub const TOOLS: &[(&str, &str)] = &[
    ("git", "source checkout"),
    ("vcs", "vcstool: multi-repo import/export"),
    ("colcon", "workspace builds"),
    ("rosdep", "dependency resolution"),
    ("apt-get", "native provider"),
    ("dpkg-query", "native provider: installed package versions"),
    ("pixi", "pixi provider (roadmap P3)"),
    ("docker", "docker provider (roadmap P4)"),
];

/// Tools the native provider cannot work without.
pub const NATIVE_CORE_TOOLS: &[&str] = &["git", "vcs", "colcon", "rosdep"];

/// rosdep's default sources list, written by `sudo rosdep init`.
pub const ROSDEP_DEFAULT_LIST: &str = "/etc/ros/rosdep/sources.list.d/20-default.list";

/// The ROS 2 apt repository (live; snapshots arrive in P2).
pub const ROS_APT_URL: &str = "http://packages.ros.org/ros2/ubuntu";
/// Substring that identifies the ROS 2 repository in any apt source file,
/// whatever its form (`ros2.list` written by hand or by rospm, `ros2.sources`
/// written by the `ros2-apt-source` package).
pub const ROS_APT_NEEDLE: &str = "packages.ros.org/ros2";
/// Where the official install guide puts the signing key.
pub const ROS_APT_KEYRING: &str = "/usr/share/keyrings/ros-archive-keyring.gpg";
/// Where rospm writes the source entry (same path as the official guide).
pub const ROS_APT_LIST: &str = "/etc/apt/sources.list.d/ros2.list";
/// Signing key for `packages.ros.org` (dearmored; the snapshot key differs, see P2).
pub const ROS_KEY_URL: &str = "https://raw.githubusercontent.com/ros/rosdistro/master/ros.key";

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
    /// First source file that references `packages.ros.org/ros2`, if any.
    pub ros2: Option<PathBuf>,
    /// The signing key at [`ROS_APT_KEYRING`] exists.
    pub keyring: bool,
    /// `universe` is enabled in some source. `None` when no source file
    /// could be read (non-Debian host).
    pub universe: Option<bool>,
}

impl AptSources {
    pub fn probe() -> AptSources {
        Self::probe_in(Path::new("/etc/apt"), Path::new(ROS_APT_KEYRING))
    }

    /// Scan `<etc_apt>/sources.list` and `<etc_apt>/sources.list.d/*`.
    pub fn probe_in(etc_apt: &Path, keyring: &Path) -> AptSources {
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
            keyring: keyring.is_file(),
            universe: None,
        };
        for file in files {
            let Ok(text) = fs::read_to_string(&file) else {
                continue;
            };
            if out.universe.is_none() {
                out.universe = Some(false);
            }
            if out.ros2.is_none() && source_text_mentions(&text, ROS_APT_NEEDLE) {
                out.ros2 = Some(file.clone());
            }
            if source_text_enables_universe(&text) {
                out.universe = Some(true);
            }
        }
        out
    }
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

/// Whether the process locale is UTF-8, judged the way ROS's install guide
/// does (`LC_ALL`, then `LC_CTYPE`, then `LANG`). Unset means POSIX, not UTF-8.
pub fn locale_is_utf8() -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty())
        .map(|v| {
            let v = v.to_ascii_lowercase();
            v.contains("utf-8") || v.contains("utf8")
        })
        .unwrap_or(false)
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
    /// Process locale is UTF-8 (ROS requires it; `apply` fixes it otherwise).
    pub locale_utf8: bool,
    pub tools: BTreeMap<String, Option<PathBuf>>,
    pub packages: PackageDb,
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
            locale_utf8: locale_is_utf8(),
            tools,
            packages,
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
            locale_utf8: false,
            tools: TOOLS.iter().map(|(t, _)| (t.to_string(), None)).collect(),
            packages: PackageDb::Unavailable,
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
    pub subject: String,
    pub expected: String,
    pub actual: String,
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
        subject: impl Into<String>,
        expected: impl Into<String>,
        actual: impl Into<String>,
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
        let probed = AptSources::probe_in(&dir, &dir.join("missing-keyring.gpg"));
        assert_eq!(probed.ros2, Some(dir.join("sources.list.d/ros2.list")));
        assert_eq!(probed.universe, Some(true));
        assert!(!probed.keyring);

        let none = AptSources::probe_in(&dir.join("nope"), &dir);
        assert_eq!(none.ros2, None);
        assert_eq!(none.universe, None);
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
        r.push("os", "ubuntu", "24.04", "22.04", Status::Mismatch);
        r.push("tool", "git", "present", "/usr/bin/git", Status::Ok);
        r.push("apt", "x", "installed", "?", Status::Unknown);
        r.push("workspace", "main", "-", "not built", Status::Info);
        assert_eq!(r.problems(), 1);
        assert_eq!(r.unknown(), 1);
        assert!(!r.ready());
    }
}
