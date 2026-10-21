//! Network reachability and speed probe for the sources `apply` will download
//! from. Read-only: small GET requests with a short timeout, run in parallel.
//!
//! Probing happens *before* the first download so a stalled or blocked
//! official source is reported up front, together with the mirrors that do
//! work, instead of surfacing minutes later as a hung `curl` or a rosdep
//! timeout (decision #17).

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::message::Message;
use crate::mirror::{self, Mirror};
use crate::msg;

/// Default per-request timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(8);

/// What one URL is needed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// `dists/<codename>/InRelease` of a ROS 2 apt repository.
    RosApt,
    /// `rosdep/base.yaml` of a rosdistro tree (also covers `ros.key`, the index).
    Rosdistro,
    /// A host named in a `.repos` file (`vcs import`); reported, never mirrored.
    GitHost,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Endpoint {
    pub role: Role,
    /// Mirror name for [`Role::RosApt`] / [`Role::Rosdistro`], host for [`Role::GitHost`].
    pub source: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum Outcome {
    Ok { millis: u64, bytes: u64 },
    Failed { error: Message },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbeResult {
    #[serde(flatten)]
    pub endpoint: Endpoint,
    #[serde(flatten)]
    pub outcome: Outcome,
}

impl ProbeResult {
    pub fn ok(&self) -> bool {
        matches!(self.outcome, Outcome::Ok { .. })
    }

    pub fn millis(&self) -> Option<u64> {
        match self.outcome {
            Outcome::Ok { millis, .. } => Some(millis),
            Outcome::Failed { .. } => None,
        }
    }

    /// One-line human form of the outcome (`ok 144 ms, 4.6 KB` / `FAILED: timed out`).
    pub fn describe(&self) -> Message {
        match &self.outcome {
            Outcome::Ok { millis, bytes } => {
                let rate = if *millis > 0 && *bytes >= 64 * 1024 {
                    format!("  {:>5} KB/s", bytes * 1000 / 1024 / millis)
                } else {
                    String::new()
                };
                msg!(
                    "net.ok",
                    millis = format!("{millis:>5}"),
                    bytes = format!("{:>8}", human_bytes(*bytes)),
                    rate = rate
                )
            }
            Outcome::Failed { error } => msg!("net.failed", error = error),
        }
    }
}

fn human_bytes(b: u64) -> String {
    if b >= 1024 * 1024 {
        format!("{:.1} MB", b as f64 / (1024.0 * 1024.0))
    } else if b >= 1024 {
        format!("{:.1} KB", b as f64 / 1024.0)
    } else {
        format!("{b} B")
    }
}

/// Endpoints for one source set: its apt repository and the rosdistro tree it
/// resolves to (which may be the official one).
pub fn endpoints_for(mirror: &Mirror, codename: &str) -> Vec<Endpoint> {
    let mut out = vec![Endpoint {
        role: Role::RosApt,
        source: mirror.name.to_string(),
        url: mirror.apt_inrelease_url(codename),
    }];
    let rosdistro_owner = if mirror.mirrors_rosdistro() {
        mirror.name
    } else {
        mirror::OFFICIAL.name
    };
    out.push(Endpoint {
        role: Role::Rosdistro,
        source: rosdistro_owner.to_string(),
        url: mirror.rosdistro_probe_url(),
    });
    out
}

/// Endpoints for every known source set plus the git hosts, deduplicated.
pub fn all_endpoints(codename: &str, git_urls: &[String]) -> Vec<Endpoint> {
    let mut out: Vec<Endpoint> = Vec::new();
    for m in mirror::ALL {
        for e in endpoints_for(m, codename) {
            if !out.iter().any(|x| x.url == e.url) {
                out.push(e);
            }
        }
    }
    let hosts: BTreeSet<String> = git_urls.iter().filter_map(|u| git_host(u)).collect();
    for host in hosts {
        out.push(Endpoint {
            role: Role::GitHost,
            source: host.clone(),
            url: format!("https://{host}/"),
        });
    }
    out
}

/// Host part of a git URL (`https://github.com/x/y.git`, `git@github.com:x/y.git`).
pub fn git_host(url: &str) -> Option<String> {
    if let Some((_, rest)) = url.split_once("://") {
        let authority = rest.split('/').next()?;
        let host = authority.rsplit('@').next()?;
        let host = host.split(':').next()?;
        return (!host.is_empty()).then(|| host.to_string());
    }
    if let Some((user_host, _)) = url.split_once(':') {
        let host = user_host.rsplit('@').next()?;
        if !host.contains('/') && host.contains('.') {
            return Some(host.to_string());
        }
    }
    None
}

/// GET every endpoint in parallel with `timeout` each. Results keep the input order.
pub fn probe(endpoints: &[Endpoint], timeout: Duration) -> Vec<ProbeResult> {
    let handles: Vec<_> = endpoints
        .iter()
        .cloned()
        .map(|e| std::thread::spawn(move || fetch(e, timeout)))
        .collect();
    handles
        .into_iter()
        .map(|h| {
            h.join().unwrap_or_else(|_| ProbeResult {
                endpoint: Endpoint {
                    role: Role::GitHost,
                    source: String::new(),
                    url: String::new(),
                },
                outcome: Outcome::Failed {
                    error: Message::text("probe thread panicked"),
                },
            })
        })
        .collect()
}

fn fetch(endpoint: Endpoint, timeout: Duration) -> ProbeResult {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .max_redirects(5)
        .user_agent(concat!("rospm/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let start = Instant::now();
    let outcome = match agent.get(&endpoint.url).call() {
        Ok(mut resp) => {
            let status = resp.status();
            match resp.body_mut().read_to_vec() {
                Ok(body) if status.is_success() => Outcome::Ok {
                    millis: start.elapsed().as_millis() as u64,
                    bytes: body.len() as u64,
                },
                Ok(_) => Outcome::Failed {
                    error: msg!("net.http_status", status = status.as_u16()),
                },
                Err(e) => Outcome::Failed {
                    error: short_error(&e, timeout),
                },
            }
        }
        Err(e) => Outcome::Failed {
            error: short_error(&e, timeout),
        },
    };
    ProbeResult { endpoint, outcome }
}

fn short_error(e: &ureq::Error, timeout: Duration) -> Message {
    match e {
        ureq::Error::Timeout(_) => msg!("net.timeout", secs = timeout.as_secs()),
        ureq::Error::Io(io) => Message::text(format!("{}", io.kind()).replace('_', " ")),
        other => {
            let s = other.to_string();
            // Keep the first sentence; ureq errors can be long.
            Message::text(s.split(": ").next().unwrap_or(&s).to_string())
        }
    }
}

/// Which source set to use, derived from probe results (pure; no I/O).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The official sources answer; nothing to change.
    OfficialOk,
    /// Some official endpoint failed; these mirrors answered on every
    /// endpoint they need, fastest first.
    UseMirror {
        failed: Vec<String>,
        candidates: Vec<Mirror>,
    },
    /// Neither the official sources nor any mirror answered.
    NothingReachable { failed: Vec<String> },
}

/// The endpoints of `mirror` a run needs. `ros_apt` is false when apt
/// installs from a ROS snapshot, so the ROS apt repository is never used.
pub fn needed(mirror: &Mirror, codename: &str, ros_apt: bool) -> Vec<Endpoint> {
    let mut out = endpoints_for(mirror, codename);
    if !ros_apt {
        out.retain(|e| e.role != Role::RosApt);
    }
    out
}

/// Whether `mirror` answered on every endpoint it needs (apt + the rosdistro
/// tree it resolves to). `None` if a needed endpoint was not probed.
pub fn mirror_ok(
    results: &[ProbeResult],
    mirror: &Mirror,
    codename: &str,
    ros_apt: bool,
) -> Option<bool> {
    let mut ok = true;
    for e in needed(mirror, codename, ros_apt) {
        let r = results.iter().find(|r| r.endpoint.url == e.url)?;
        ok &= r.ok();
    }
    Some(ok)
}

/// Total latency of the endpoints `mirror` needs; `None` if any failed.
fn mirror_latency(
    results: &[ProbeResult],
    mirror: &Mirror,
    codename: &str,
    ros_apt: bool,
) -> Option<u64> {
    let mut total = 0;
    for e in needed(mirror, codename, ros_apt) {
        total += results.iter().find(|r| r.endpoint.url == e.url)?.millis()?;
    }
    Some(total)
}

pub fn verdict(results: &[ProbeResult], codename: &str, ros_apt: bool) -> Verdict {
    if mirror_ok(results, &mirror::OFFICIAL, codename, ros_apt) == Some(true) {
        return Verdict::OfficialOk;
    }
    let failed: Vec<String> = results
        .iter()
        .filter(|r| r.endpoint.role != Role::GitHost && !r.ok())
        .filter(|r| {
            needed(&mirror::OFFICIAL, codename, ros_apt)
                .iter()
                .any(|e| e.url == r.endpoint.url)
        })
        .map(|r| r.endpoint.url.clone())
        .collect();
    let mut candidates: Vec<(u64, Mirror)> = mirror::MIRRORS
        .iter()
        .filter_map(|m| mirror_latency(results, m, codename, ros_apt).map(|ms| (ms, *m)))
        .collect();
    candidates.sort_by_key(|(ms, _)| *ms);
    if candidates.is_empty() {
        Verdict::NothingReachable { failed }
    } else {
        Verdict::UseMirror {
            failed,
            candidates: candidates.into_iter().map(|(_, m)| m).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(url: &str, outcome: Outcome) -> ProbeResult {
        ProbeResult {
            endpoint: Endpoint {
                role: Role::RosApt,
                source: String::new(),
                url: url.to_string(),
            },
            outcome,
        }
    }
    fn ok(url: &str, ms: u64) -> ProbeResult {
        res(
            url,
            Outcome::Ok {
                millis: ms,
                bytes: 1,
            },
        )
    }
    fn fail(url: &str) -> ProbeResult {
        res(
            url,
            Outcome::Failed {
                error: Message::text("timed out"),
            },
        )
    }

    #[test]
    fn git_hosts_are_extracted_from_https_and_ssh_urls() {
        assert_eq!(
            git_host("https://github.com/ros2/demos.git").as_deref(),
            Some("github.com")
        );
        assert_eq!(
            git_host("git@gitlab.example.com:group/repo.git").as_deref(),
            Some("gitlab.example.com")
        );
        assert_eq!(
            git_host("ssh://git@github.com:22/ros2/demos").as_deref(),
            Some("github.com")
        );
        assert_eq!(git_host("/local/path"), None);
    }

    #[test]
    fn all_endpoints_dedups_shared_rosdistro_and_lists_git_hosts_once() {
        let eps = all_endpoints(
            "jammy",
            &[
                "https://github.com/ros2/demos.git".into(),
                "https://github.com/ros2/examples.git".into(),
            ],
        );
        let official_index = eps
            .iter()
            .filter(|e| e.url == mirror::OFFICIAL.rosdistro_probe_url())
            .count();
        assert_eq!(official_index, 1, "official rosdistro probed once");
        let git: Vec<_> = eps.iter().filter(|e| e.role == Role::GitHost).collect();
        assert_eq!(git.len(), 1);
        assert_eq!(git[0].url, "https://github.com/");
        // apt InRelease for every source set
        assert_eq!(
            eps.iter().filter(|e| e.role == Role::RosApt).count(),
            mirror::ALL.len()
        );
    }

    #[test]
    fn verdict_prefers_official_then_fastest_complete_mirror() {
        let c = "jammy";
        let off_apt = mirror::OFFICIAL.apt_inrelease_url(c);
        let off_idx = mirror::OFFICIAL.rosdistro_probe_url();
        let tuna_apt = mirror::TUNA.apt_inrelease_url(c);
        let tuna_idx = mirror::TUNA.rosdistro_probe_url();
        let ustc_apt = mirror::USTC.apt_inrelease_url(c);
        let ustc_idx = mirror::USTC.rosdistro_probe_url();
        let ali_apt = mirror::ALIYUN.apt_inrelease_url(c);

        let all_good = vec![ok(&off_apt, 400), ok(&off_idx, 500), ok(&tuna_apt, 100)];
        assert_eq!(verdict(&all_good, c, true), Verdict::OfficialOk);

        // GitHub down: aliyun (apt only) needs the official rosdistro, so it is
        // not a candidate; ustc is faster than tuna.
        let github_down = vec![
            ok(&off_apt, 400),
            fail(&off_idx),
            ok(&tuna_apt, 100),
            ok(&tuna_idx, 300),
            ok(&ustc_apt, 100),
            ok(&ustc_idx, 150),
            ok(&ali_apt, 50),
        ];
        match verdict(&github_down, c, true) {
            Verdict::UseMirror { failed, candidates } => {
                assert_eq!(failed, vec![off_idx.clone()]);
                assert_eq!(
                    candidates.iter().map(|m| m.name).collect::<Vec<_>>(),
                    vec!["ustc", "tuna"]
                );
            }
            other => panic!("{other:?}"),
        }

        // Locked to a ROS snapshot: a blocked packages.ros.org does not matter.
        let apt_blocked = vec![fail(&off_apt), ok(&off_idx, 500)];
        assert_eq!(verdict(&apt_blocked, c, false), Verdict::OfficialOk);
        assert_ne!(verdict(&apt_blocked, c, true), Verdict::OfficialOk);

        let nothing = vec![fail(&off_apt), fail(&off_idx), fail(&tuna_apt)];
        assert!(matches!(
            verdict(&nothing, c, true),
            Verdict::NothingReachable { .. }
        ));
    }

    #[test]
    fn describe_is_compact() {
        assert_eq!(ok("u", 12).describe().key_name(), Some("net.ok"));
        assert_eq!(fail("u").describe().key_name(), Some("net.failed"));
        assert_eq!(human_bytes(4682), "4.6 KB");
    }
}
