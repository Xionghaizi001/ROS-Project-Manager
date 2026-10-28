//! Best-effort presentation of diagnostics from this execution's logs.
use crate::{
    diagnostic::{self, Diagnostic},
    plan::{Plan, Step},
    Error,
};
use serde::Serialize;
use std::{fmt, fs, io::Read, path::PathBuf, time::SystemTime};

#[derive(Debug, Serialize)]
pub struct Summary {
    pub log: PathBuf,
    pub command: String,
    pub status: String,
    pub cwd: Option<PathBuf>,
    pub python: Option<String>,
    pub groups: Vec<Group>,
    pub diagnostic_status: DiagnosticStatus,
}
/// Diagnostic availability never determines the command's success or failure.
#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticStatus {
    Parsed,
    Unrecognized,
    Unavailable,
}
#[derive(Debug, Serialize)]
pub struct Group {
    pub workspace: Option<String>,
    pub package: Option<String>,
    pub diagnostics: Vec<Diagnostic>,
    /// Reported downstream results; association does not establish causality.
    pub consequences: Vec<String>,
}
impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", crate::msg!("build_summary.title"))
    }
}

// Bound parser memory even for very large build logs. Full bytes remain on disk.
fn read(path: &std::path::Path) -> Option<String> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(4 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}
fn field<'a>(header: &'a str, key: &str) -> Option<&'a str> {
    header.lines().find_map(|line| line.strip_prefix(key))
}
fn group(
    text: &str,
    workspace: Option<String>,
    package: Option<String>,
    python: Option<&str>,
) -> Group {
    let mut diagnostics = diagnostic::parse_cpp(text);
    diagnostics.extend(diagnostic::parse_python(text, python));
    let mut unique: Vec<Diagnostic> = Vec::new();
    for d in diagnostics {
        if let Some(existing) = unique.iter_mut().find(|e| {
            e.code == d.code && e.location == d.location && e.explanation == d.explanation
        }) {
            for related in d.related {
                if !existing.related.contains(&related) {
                    existing.related.push(related);
                }
            }
        } else {
            unique.push(d);
        }
    }
    let mut consequences = Vec::new();
    for line in text.lines().filter(|l| {
        l.starts_with("Failed   <<<")
            || l.starts_with("Aborted  <<<")
            || l.starts_with("Summary:")
            || ((l.starts_with("gmake") || l.starts_with("make") || l.starts_with("collect2:"))
                && (l.contains("Error") || l.contains("error:")))
    }) {
        if !consequences.iter().any(|s| s == line) {
            consequences.push(line.to_owned());
        }
    }
    Group {
        workspace,
        package,
        diagnostics: unique,
        consequences,
    }
}

pub fn collect(plan: &Plan, error: &Error, started: SystemTime) -> Option<Summary> {
    let Error::CommandFailed { command, status } = error else {
        return None;
    };
    let cmd = plan.steps.iter().find_map(|step| match step {
        Step::Run(c) if c.render() == *command => Some(c),
        _ => None,
    })?;
    let header = cmd.log.as_deref()?;
    let log = plan.log.clone()?;
    let contents = read(&log);
    // Earlier successful workspaces/verification commands must not be diagnosed.
    let marker = format!("==> command: {command}\n");
    let text = contents
        .as_deref()
        .and_then(|s| s.rsplit_once(&marker).map(|(_, output)| output));
    let workspace = field(header, "workspace: ").map(str::to_owned);
    let python = field(header, "python: ");
    let mut groups = Vec::new();
    if let Some(base) =
        field(header, "package logs: ").and_then(|p| p.strip_suffix("<package>/stdout_stderr.log"))
    {
        if let Ok(entries) = fs::read_dir(base) {
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let path = entry.path().join("stdout_stderr.log");
                if !fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .is_ok_and(|t| t >= started)
                {
                    continue;
                }
                if let Some(text) = read(&path) {
                    let g = group(
                        &text,
                        workspace.clone(),
                        Some(entry.file_name().to_string_lossy().into_owned()),
                        python,
                    );
                    if !g.diagnostics.is_empty() || !g.consequences.is_empty() {
                        groups.push(g);
                    }
                }
            }
        }
    }
    let mut fallback = group(text.unwrap_or_default(), workspace, None, python);
    // Keep command-level diagnostics absent from the package logs, without
    // repeating colcon's echoed excerpts or attributing them to another package.
    fallback.diagnostics.retain(|d| {
        // Once package streams are available, do not reconstruct a Python
        // traceback from colcon's potentially interleaved aggregate stream.
        !(d.tool.as_deref() == Some("python") && !groups.is_empty())
            && !groups.iter().any(|g| {
                g.diagnostics.iter().any(|p| {
                    p.code == d.code && p.location == d.location && p.explanation == d.explanation
                })
            })
    });
    fallback
        .consequences
        .retain(|line| !groups.iter().any(|g| g.consequences.contains(line)));
    if !fallback.diagnostics.is_empty() || !fallback.consequences.is_empty() || groups.is_empty() {
        groups.push(fallback);
    }
    groups.sort_by_key(|g| g.diagnostics.is_empty());
    let diagnostic_status = if text.is_none() {
        DiagnosticStatus::Unavailable
    } else if groups.iter().all(|g| g.diagnostics.is_empty()) {
        DiagnosticStatus::Unrecognized
    } else {
        DiagnosticStatus::Parsed
    };
    Some(Summary {
        diagnostic_status,
        log,
        command: command.clone(),
        status: status.clone(),
        cwd: cmd.cwd.clone(),
        python: python.map(str::to_owned),
        groups,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::Command;

    #[test]
    fn only_failed_command_is_parsed_and_stale_package_logs_are_ignored() {
        let root = std::env::temp_dir().join(format!("rospm-summary-{}", std::process::id()));
        fs::create_dir_all(root.join("latest_build/stale")).unwrap();
        let stale = root.join("latest_build/stale/stdout_stderr.log");
        fs::write(&stale, "stale.cpp:1: error: expected ';'\n").unwrap();
        let file = fs::File::options().write(true).open(&stale).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH).unwrap();
        let log = root.join("build.log");
        let cmd = Command::new("false").log(format!(
            "workspace: main\npython: system\npackage logs: {}/latest_build/<package>/stdout_stderr.log\n", root.display()
        ));
        fs::write(&log, format!("==> command: successful\nearlier.cpp:1: error: expected ';'\n==> command: {}\nnow.cpp:2:3: error: expected ';'\n", cmd.render())).unwrap();
        let error = Error::CommandFailed {
            command: cmd.render(),
            status: "exit status: 2".into(),
        };
        let plan = Plan {
            steps: vec![Step::Run(cmd)],
            log: Some(log),
            ..Plan::default()
        };
        let report = collect(&plan, &error, SystemTime::now()).unwrap();
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].package, None);
        assert_eq!(report.groups[0].diagnostics.len(), 1);
        assert_eq!(
            report.groups[0].diagnostics[0]
                .location
                .as_ref()
                .unwrap()
                .file,
            PathBuf::from("now.cpp")
        );
        // Missing/truncated/unreadable logs never replace the command failure.
        fs::write(plan.log.as_ref().unwrap(), "unknown output").unwrap();
        for missing in [false, true] {
            if missing {
                fs::remove_file(plan.log.as_ref().unwrap()).unwrap();
            }
            let report = collect(&plan, &error, SystemTime::now()).unwrap();
            assert_eq!(report.diagnostic_status, DiagnosticStatus::Unavailable);
            assert_eq!(report.status, "exit status: 2");
            assert_eq!(report.command, "false");
            assert!(report.groups.iter().all(|g| g.diagnostics.is_empty()));
        }
        fs::remove_dir_all(root).unwrap();
    }
}
