//! Read-only target package availability. This is not a dependency solver or
//! signature verification: installation remains the package manager's job.
use std::{collections::BTreeSet, path::Path};

use crate::{
    manifest::{Manifest, ProviderKind},
    msg, pixi,
    platform::{ubuntu_codename, Platform},
    probe::{EnvReport, Status},
    snapshot::{self, Fetch},
    Message,
};

/// Query explicitly declared packages, after applying target overrides.
/// Failed indexes make absent packages unknown, never falsely missing.
pub fn check(m: &Manifest, root: &Path, target: Platform, fetch: &(dyn Fetch + Sync)) -> EnvReport {
    let mut report = EnvReport::default();
    let (packages, urls, conda) = match m.environment.provider {
        ProviderKind::Native => {
            let mut packages = m.effective_apt(target);
            packages.extend([
                m.ros_base_package(),
                "python3-rosdep".into(),
                "python3-vcstool".into(),
                "python3-colcon-common-extensions".into(),
            ]);
            let ubuntu = m.ubuntu();
            let Some(suite) = ubuntu_codename(&ubuntu) else {
                report.push(
                    "repository",
                    Message::text(ubuntu),
                    msg!("repository.available"),
                    msg!("repository.unsupported"),
                    Status::Unknown,
                );
                return report;
            };
            let arch = target.dpkg_arch();
            let mut urls = vec![snapshot::packages_url(
                crate::mirror::OFFICIAL_ROS_APT,
                suite,
                "main",
                arch,
            )];
            for pocket in [
                suite.to_string(),
                format!("{suite}-updates"),
                format!("{suite}-security"),
            ] {
                let base = match target {
                    Platform::LinuxAarch64 => "https://ports.ubuntu.com/ubuntu-ports",
                    _ if pocket.ends_with("-security") => "https://security.ubuntu.com/ubuntu",
                    _ => "https://archive.ubuntu.com/ubuntu",
                };
                for component in ["main", "restricted", "universe", "multiverse"] {
                    urls.push(snapshot::packages_url(base, &pocket, component, arch));
                }
            }
            (packages, urls, false)
        }
        ProviderKind::Pixi => {
            let path = root.join(
                &m.environment
                    .pixi
                    .as_ref()
                    .expect("validated pixi config")
                    .manifest,
            );
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => Ok(text),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    pixi::generate(m).map_err(|e| e.to_string())
                }
                Err(e) => Err(e.to_string()),
            };
            let parsed = text.and_then(|text| conda_config(&text, target));
            match parsed {
                Ok((packages, channels)) => {
                    let urls = channels
                        .into_iter()
                        .flat_map(|channel| {
                            let base = if channel.contains("://") {
                                channel.trim_end_matches('/').to_owned()
                            } else {
                                format!("https://conda.anaconda.org/{channel}")
                            };
                            [pixi::conda_platform(target), "noarch"]
                                .map(|subdir| format!("{base}/{subdir}/repodata.json.bz2"))
                        })
                        .collect();
                    (packages, urls, true)
                }
                Err(error) => {
                    report.push(
                        "repository",
                        Message::text(path.display().to_string()),
                        msg!("repository.available"),
                        Message::text(error),
                        Status::Unknown,
                    );
                    return report;
                }
            }
        }
        ProviderKind::Docker => {
            report.push(
                "repository",
                Message::text("docker"),
                msg!("repository.available"),
                msg!("repository.unsupported"),
                Status::Unknown,
            );
            return report;
        }
    };
    let disabled = std::env::var_os("ROSPM_NO_NETWORK").is_some_and(|v| !v.is_empty() && v != "0");
    let mut available = BTreeSet::new();
    let mut failed = false;
    // Bound concurrency and memory: conda indexes can be much larger than apt.
    for batch in urls.chunks(if conda { 1 } else { 4 }) {
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|url| {
                    scope.spawn(move || {
                        if disabled {
                            Err(msg!("repository.offline"))
                        } else if conda {
                            conda_index(fetch, url)
                        } else {
                            apt_index(fetch, url, target.dpkg_arch())
                        }
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_else(|_| Err(msg!("repository.invalid"))))
                .collect::<Vec<_>>()
        });
        for (url, result) in batch.iter().zip(results) {
            match result {
                Ok(names) => available.extend(names),
                Err(error) => {
                    failed = true;
                    report.push(
                        "repository",
                        Message::text(url.clone()),
                        msg!("repository.available"),
                        error,
                        Status::Unknown,
                    );
                }
            }
        }
    }
    for package in packages.into_iter().collect::<BTreeSet<_>>() {
        let status = if available.contains(&package) {
            Status::Ok
        } else if failed {
            Status::Unknown
        } else {
            Status::Missing
        };
        let actual = match status {
            Status::Ok => msg!("repository.found", platform = target),
            Status::Missing => msg!("repository.missing", platform = target),
            _ => msg!("repository.incomplete", platform = target),
        };
        report.push(
            "repository",
            Message::text(package),
            msg!("repository.available"),
            actual,
            status,
        );
    }
    report
}

fn apt_index(fetch: &dyn Fetch, url: &str, arch: &str) -> Result<BTreeSet<String>, Message> {
    let (status, body) = fetch.get_large(url)?;
    if !(200..300).contains(&status) {
        return Err(msg!("snapshot.http_status", url = url, status = status));
    }
    let body = snapshot::decompress(&body, "gzip")?;
    let text = std::str::from_utf8(&body).map_err(|_| msg!("repository.invalid"))?;
    let mut names = BTreeSet::new();
    for stanza in text.split("\n\n").filter(|s| !s.trim().is_empty()) {
        let field = |key: &str| {
            stanza
                .lines()
                .find_map(|line| line.strip_prefix(key))
                .map(str::trim)
        };
        let (Some(name), Some(version), Some(actual_arch)) =
            (field("Package:"), field("Version:"), field("Architecture:"))
        else {
            return Err(msg!("repository.invalid"));
        };
        if name.is_empty() || version.is_empty() {
            return Err(msg!("repository.invalid"));
        }
        if actual_arch == arch || actual_arch == "all" {
            names.insert(name.to_owned());
        }
    }
    Ok(names)
}

fn conda_config(text: &str, target: Platform) -> Result<(Vec<String>, Vec<String>), String> {
    let summary = pixi::read_summary(text).map_err(|e| e.to_string())?;
    let root: toml::Value = text.parse::<toml::Value>().map_err(|e| e.to_string())?;
    let invalid = || msg!("repository.pixi_invalid").to_string();
    let channels = root
        .get("workspace")
        .or_else(|| root.get("project"))
        .and_then(|w| w.get("channels"))
        .and_then(toml::Value::as_array)
        .ok_or_else(invalid)?;
    if channels.len() != summary.channels.len()
        || summary.channels.iter().any(|c| c.trim().is_empty())
    {
        return Err(invalid());
    }
    let mut packages = Vec::new();
    let mut collect = |table: &toml::Value| -> Result<(), String> {
        if !table.is_table() {
            return Err(invalid());
        }
        for section in ["dependencies", "host-dependencies", "build-dependencies"] {
            if let Some(value) = table.get(section) {
                packages.extend(value.as_table().ok_or_else(invalid)?.keys().cloned());
            }
        }
        Ok(())
    };
    collect(&root)?;
    if let Some(targets) = root.get("target") {
        let targets = targets.as_table().ok_or_else(invalid)?;
        for selector in ["unix", "linux", pixi::conda_platform(target)] {
            if let Some(table) = targets.get(selector) {
                collect(table)?;
            }
        }
    }
    if summary.channels.is_empty() || root.get("feature").is_some() {
        return Err(msg!("repository.pixi_unsupported").to_string());
    }
    Ok((packages, summary.channels))
}

fn conda_index(fetch: &dyn Fetch, url: &str) -> Result<BTreeSet<String>, Message> {
    let (status, body) = fetch.get_large(url)?;
    if !(200..300).contains(&status) {
        return Err(msg!("snapshot.http_status", url = url, status = status));
    }
    let body = snapshot::decompress(&body, "bzip2")?;
    #[derive(serde::Deserialize)]
    struct Package {
        name: String,
    }
    #[derive(serde::Deserialize)]
    struct Index {
        packages: std::collections::BTreeMap<String, Package>,
        #[serde(default, rename = "packages.conda")]
        conda: std::collections::BTreeMap<String, Package>,
    }
    let index: Index = serde_json::from_slice(&body).map_err(|_| msg!("repository.invalid"))?;
    Ok(index
        .packages
        .into_values()
        .chain(index.conda.into_values())
        .map(|p| p.name)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        process::{Command, Stdio},
        sync::Mutex,
    };

    fn compress(text: &str, program: &str) -> Vec<u8> {
        let mut child = Command::new(program)
            .arg("-c")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        out.stdout
    }

    struct Apt {
        urls: Mutex<Vec<String>>,
        fail_ros: bool,
        body: Vec<u8>,
    }
    impl Fetch for Apt {
        fn get(&self, url: &str) -> Result<(u16, Vec<u8>), Message> {
            self.urls.lock().unwrap().push(url.to_owned());
            if self.fail_ros && url.contains("packages.ros.org") {
                return Ok((503, vec![]));
            }
            Ok((200, self.body.clone()))
        }
    }

    fn manifest() -> Manifest {
        Manifest::parse(
            r#"
schema_version = 1
[project]
name = "target-check"
ros_distro = "jazzy"
platforms = ["linux-x86_64", "linux-aarch64"]
[environment]
provider = "native"
[environment.native]
[workspaces.main]
path = "ws"
[dependencies]
apt = ["original", "excluded", "missing"]
[target.linux-aarch64.dependencies]
apt = ["extra"]
[target.linux-aarch64.replace]
original = "replacement"
[target.linux-aarch64.exclude]
apt = ["excluded"]
"#,
            Path::new("test.toml"),
        )
        .unwrap()
    }

    #[test]
    fn apt_repository_checks_the_builtin_or_user_replacement_not_the_original() {
        let mut m = manifest();
        m.dependencies.apt = vec!["gcc-x86-64-linux-gnu".into()];
        m.target.clear();
        let fetch = Apt {
            urls: Mutex::new(vec![]),
            fail_ros: false,
            body: compress(
                "Package: gcc-aarch64-linux-gnu\nVersion: 1\nArchitecture: arm64\n",
                "gzip",
            ),
        };
        let report = check(&m, Path::new("."), Platform::LinuxAarch64, &fetch);
        assert!(report
            .items
            .iter()
            .any(|i| i.subject.to_string() == "gcc-aarch64-linux-gnu" && i.status == Status::Ok));
        assert!(!report
            .items
            .iter()
            .any(|i| i.subject.to_string() == "gcc-x86-64-linux-gnu"));

        m.target
            .entry("linux-aarch64".into())
            .or_default()
            .replace
            .insert("gcc-x86-64-linux-gnu".into(), "custom-compiler".into());
        let report = check(&m, Path::new("."), Platform::LinuxAarch64, &fetch);
        assert!(report
            .items
            .iter()
            .any(|i| i.subject.to_string() == "custom-compiler" && i.status == Status::Missing));
        assert!(!report
            .items
            .iter()
            .any(|i| i.subject.to_string() == "gcc-aarch64-linux-gnu"));
    }

    #[test]
    fn apt_uses_target_ports_and_all_pockets_and_distinguishes_failure() {
        let fetch = Apt { urls: Mutex::new(vec![]), fail_ros: false,
            body: compress("Package: extra\nVersion: 1\nArchitecture: all\n\nPackage: wrong\nVersion: 1\nArchitecture: amd64\n", "gzip") };
        let m = manifest();
        let report = check(&m, Path::new("."), Platform::LinuxAarch64, &fetch);
        assert!(report
            .items
            .iter()
            .any(|i| i.subject.to_string() == "extra" && i.status == Status::Ok));
        assert!(report
            .items
            .iter()
            .any(|i| i.subject.to_string() == "missing" && i.status == Status::Missing));
        assert!(!report
            .items
            .iter()
            .any(|i| ["original", "excluded"].contains(&i.subject.to_string().as_str())));
        assert!(report
            .items
            .iter()
            .any(|i| i.subject.to_string() == "replacement"));
        let urls = fetch.urls.lock().unwrap();
        assert_eq!(urls.len(), 13);
        assert!(urls.iter().all(|u| u.contains("binary-arm64")));
        assert!(urls
            .iter()
            .any(|u| u.contains("ubuntu-ports/dists/noble-security/universe")));
        drop(urls);
        let fetch = Apt {
            fail_ros: true,
            ..fetch
        };
        let report = check(&m, Path::new("."), Platform::LinuxAarch64, &fetch);
        assert!(report
            .items
            .iter()
            .any(|i| i.subject.to_string() == "missing" && i.status == Status::Unknown));
        assert!(report
            .items
            .iter()
            .any(|i| i.subject.to_string() == "extra" && i.status == Status::Ok));
        assert!(!report.ready());
    }

    #[test]
    fn apt_empty_valid_index_and_malformed_or_wrong_architecture() {
        let url = "https://test/Packages.gz";
        let fetch = snapshot::Canned::default().with(url, 200, compress("", "gzip"));
        assert!(apt_index(&fetch, url, "arm64").unwrap().is_empty());
        let fetch =
            snapshot::Canned::default().with(url, 200, compress("<html>bad</html>", "gzip"));
        assert!(apt_index(&fetch, url, "arm64").is_err());
        let fetch = snapshot::Canned::default().with(
            url,
            200,
            compress("Package: wrong\nVersion: 1\nArchitecture: amd64\n", "gzip"),
        );
        assert!(apt_index(&fetch, url, "arm64").unwrap().is_empty());
    }

    #[test]
    fn conda_reads_both_formats_and_rejects_broken_index() {
        let url = "https://test/repodata.json.bz2";
        let body = compress(
            r#"{"packages":{"a.tar.bz2":{"name":"a"}},"packages.conda":{"b.conda":{"name":"b"}}}"#,
            "bzip2",
        );
        let fetch = snapshot::Canned::default().with(url, 200, body);
        assert_eq!(
            conda_index(&fetch, url).unwrap(),
            BTreeSet::from(["a".into(), "b".into()])
        );
        let fetch = snapshot::Canned::default().with(url, 200, compress("{}", "bzip2"));
        assert!(conda_index(&fetch, url).is_err());
    }

    #[test]
    fn conda_config_includes_target_and_build_dependencies() {
        for invalid in [
            "[workspace]\nchannels = [\"test\", 1]",
            "[workspace]\nchannels = [\"\"]",
            "dependencies = \"broken\"\n[workspace]\nchannels = [\"test\"]",
            "[workspace]\nchannels = [\"test\"]\n[target]\nlinux-aarch64 = 1",
        ] {
            assert!(conda_config(invalid, Platform::LinuxAarch64).is_err());
        }
        let (packages, channels) = conda_config(
            r#"
[workspace]
channels = [{channel = "robostack-jazzy"}, "https://example.test/channel"]
[dependencies]
base = "*"
[build-dependencies]
compiler = "*"
[target.linux-aarch64.dependencies]
arm = "*"
[target.linux-64.dependencies]
x86 = "*"
"#,
            Platform::LinuxAarch64,
        )
        .unwrap();
        assert_eq!(packages, vec!["base", "compiler", "arm"]);
        assert_eq!(channels.len(), 2);
    }

    #[test]
    fn pixi_queries_noarch_and_generated_manifest_without_writing() {
        let mut m = manifest();
        m.environment.provider = ProviderKind::Pixi;
        m.environment.native = None;
        m.environment.pixi = Some(crate::manifest::PixiEnv {
            manifest: "nonexistent-p3-check-pixi.toml".into(),
        });
        let empty = compress(r#"{"packages":{}}"#, "bzip2");
        let mut fetch = snapshot::Canned::default();
        for channel in ["robostack-jazzy", "conda-forge"] {
            for subdir in ["linux-aarch64", "noarch"] {
                fetch = fetch.with(
                    format!("https://conda.anaconda.org/{channel}/{subdir}/repodata.json.bz2"),
                    200,
                    &empty,
                );
            }
        }
        fetch = fetch.with(
            "https://conda.anaconda.org/conda-forge/noarch/repodata.json.bz2",
            200,
            compress(
                r#"{"packages":{"rosdep.tar.bz2":{"name":"rosdep"}}}"#,
                "bzip2",
            ),
        );
        let report = check(&m, Path::new("."), Platform::LinuxAarch64, &fetch);
        assert_eq!(report.unknown(), 0);
        assert!(report
            .items
            .iter()
            .any(|i| i.subject.to_string() == "rosdep" && i.status == Status::Ok));
        assert!(report
            .items
            .iter()
            .any(|i| i.subject.to_string() == "ros-jazzy-ros-base" && i.status == Status::Missing));
    }
}
