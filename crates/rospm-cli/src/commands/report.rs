//! Shared environment and network report formatting.

use serde::Serialize;

use rospm_core::msg;
use rospm_core::net::{ProbeResult, Role};
use rospm_core::probe::{EnvReport, Status};

use crate::i18n::{self, tr};

#[derive(Default, Serialize)]
pub(super) struct EnvSummary {
    /// False when probing was skipped (target differs from host).
    probed: bool,
    pub(super) ready: bool,
    problems: usize,
    unknown: usize,
    items: Vec<rospm_core::probe::EnvItem>,
}

// A default summary means probing was skipped; a report means it ran,
// even when there are no items to check.
impl From<EnvReport> for EnvSummary {
    fn from(report: EnvReport) -> Self {
        Self {
            probed: true,
            ready: report.ready(),
            problems: report.problems(),
            unknown: report.unknown(),
            items: report.items,
        }
    }
}

/// Expected-vs-actual table. Read-only; the hints point at the command that
/// would change things, nothing is executed here.
pub(super) fn print_env_report(r: &EnvReport, platform: &str) {
    print_check_report(r, platform, false);
}

pub(super) fn print_check_report(r: &EnvReport, platform: &str, repositories: bool) {
    if repositories {
        let header = msg!("repository.header", platform = platform);
        print_report(r, header, "repository.ready", "repository.not_ready");
    } else {
        let header = msg!("cli.check.env_header", platform = platform);
        print_report(r, header, "cli.check.ready", "cli.check.not_ready");
    }
}

/// Header, one aligned row per item, then the ready / not-ready summary.
pub(super) fn print_report(
    r: &EnvReport,
    header: impl std::fmt::Display,
    ready: &'static str,
    not_ready: &'static str,
) {
    println!("{header}");
    let subjects: Vec<String> = r.items.iter().map(|i| i.subject.to_string()).collect();
    let width = subjects
        .iter()
        .map(|s| i18n::display_width(s))
        .max()
        .unwrap_or(0)
        .min(48);
    for (item, subject) in r.items.iter().zip(&subjects) {
        let mark = match item.status {
            Status::Ok => tr("cli.status.ok"),
            Status::Missing => tr("cli.status.missing"),
            Status::Mismatch => tr("cli.status.mismatch"),
            Status::Unknown => tr("cli.status.unknown"),
            Status::Info => tr("cli.status.info"),
        };
        println!(
            "  {} {:<8} {}  {}  |  {}",
            i18n::pad(&mark, 8),
            item.area,
            i18n::pad(subject, width),
            item.expected,
            item.actual
        );
    }
    let problems = r.problems();
    let unknown = r.unknown();
    if r.ready() {
        println!("{}", tr(ready));
    } else {
        let mut parts = Vec::new();
        if problems == 1 {
            parts.push(tr("cli.check.problems_one"));
        } else if problems > 1 {
            parts.push(msg!("cli.check.problems_many", count = problems).to_string());
        }
        if unknown > 0 {
            parts.push(msg!("cli.check.undetermined", count = unknown).to_string());
        }
        let summary = rospm_core::message::Message::key(not_ready).arg("summary", parts.join(", "));
        println!("{summary}");
    }
}

pub(super) fn print_probe_table(results: &[ProbeResult]) {
    for r in results {
        let role = tr(match r.endpoint.role {
            Role::RosApt => "cli.apply.role.ros_apt",
            Role::Rosdistro => "cli.apply.role.rosdistro",
            Role::GitHost => "cli.apply.role.git_host",
        });
        println!(
            "  {} {} {:<9} {}",
            i18n::pad(&r.describe().to_string(), 34),
            i18n::pad(&role, 9),
            r.endpoint.source,
            r.endpoint.url
        );
    }
}
