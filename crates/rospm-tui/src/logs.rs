//! Bounded, read-only log browsing. Each source remains separate.
use rospm_core::{
    run::{output, RunStatus},
    worker::{LogEntry, LogStream},
};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
};

const PAGE_BYTES: usize = 64 * 1024;
#[derive(Clone)]
pub(crate) struct Source {
    pub path: PathBuf,
    pub label: String,
    json: bool,
    pub process: bool,
}

fn sources(root: &Path) -> Vec<Source> {
    let names = [
        "build.log",
        "apply.log",
        "uninstall.log",
        "deps.log",
        "conda.log",
        "run.log",
        "debug.log",
    ];
    let mut result: Vec<_> = names
        .iter()
        .map(|name| Source {
            path: root.join(".rospm/log").join(name),
            label: name.to_string(),
            json: false,
            process: false,
        })
        .collect();
    if let Ok(entries) = fs::read_dir(root.join(".rospm/log/run")) {
        let mut paths: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
        paths.sort_unstable_by(|a, b| b.cmp(a));
        result.extend(paths.into_iter().filter_map(|path| {
            let name = path.file_name()?.to_str()?;
            let id = name.strip_suffix(".log")?;
            if !rospm_core::worker::valid_identifier(id) {
                return None;
            }
            Some(Source {
                label: format!("run/{id}"),
                path,
                json: false,
                process: false,
            })
        }));
    }
    // Task snapshots currently have no operation field: never infer a workflow
    // from the task id or merge unrelated task output into a build log.
    if let Ok(entries) = fs::read_dir(root.join(".rospm/worker/tasks")) {
        let mut tasks: Vec<_> = entries.flatten().filter(|e| e.path().is_dir()).collect();
        tasks.sort_by_key(|e| e.file_name());
        result.extend(tasks.into_iter().map(|e| Source {
            label: format!("task/{}", e.file_name().to_string_lossy()),
            path: e.path().join("logs.jsonl"),
            json: true,
            process: false,
        }));
    }
    result
}

pub(crate) struct Browser {
    pub sources: Vec<Source>,
    pub selected: usize,
    pub lines: Vec<String>,
    pub error: Option<String>,
    pub scroll: usize,
    pub horizontal: u16,
    offset: u64,
    next: u64,
    previous: Vec<u64>,
    pub more: bool,
}
impl Browser {
    pub fn new(root: Option<&Path>, processes: &[RunStatus], preferred: Option<&str>) -> Self {
        let mut sources = root.map(sources).unwrap_or_default();
        let mut selected = 0;
        for process in processes {
            if let Some(source) = process_source(process) {
                if preferred == Some(process.id.as_str()) {
                    selected = sources.len();
                }
                sources.push(source);
            }
        }
        let mut browser = Self {
            sources,
            selected,
            lines: Vec::new(),
            error: None,
            scroll: 0,
            horizontal: 0,
            offset: 0,
            next: 0,
            previous: Vec::new(),
            more: false,
        };
        browser.reload();
        browser
    }

    pub fn select(&mut self, forward: bool) {
        if self.sources.is_empty() {
            return;
        }
        self.selected = if forward {
            (self.selected + 1) % self.sources.len()
        } else {
            (self.selected + self.sources.len() - 1) % self.sources.len()
        };
        self.offset = 0;
        self.previous.clear();
        self.scroll = 0;
        self.horizontal = 0;
        self.reload();
    }
    pub fn page(&mut self, forward: bool) {
        if forward && self.more {
            self.previous.push(self.offset);
            self.offset = self.next;
        } else if !forward {
            self.offset = self.previous.pop().unwrap_or(0);
        }
        self.scroll = 0;
        self.reload();
    }
    pub fn reload(&mut self) {
        self.lines.clear();
        self.error = None;
        self.more = false;
        let Some(source) = self.sources.get(self.selected) else {
            return;
        };
        match read_page(source, self.offset) {
            Ok((lines, next, more)) => {
                self.lines = lines;
                self.next = next;
                self.more = more;
            }
            Err(error) => self.error = Some(error.to_string()),
        }
        self.scroll = self.scroll.min(self.lines.len().saturating_sub(1));
    }
}

fn process_source(process: &RunStatus) -> Option<Source> {
    let fd = process.output_fd?;
    Some(Source {
        path: PathBuf::from(format!("/proc/self/fd/{fd}")),
        label: process.target.clone(),
        json: process.task,
        process: !process.task,
    })
}

fn read_page(source: &Source, offset: u64) -> std::io::Result<(Vec<String>, u64, bool)> {
    let mut file = File::open(&source.path)?;
    let (mut bytes, offset, more) = if source.process && output::is_transcript(file.as_raw_fd()) {
        output::page(&file, offset, PAGE_BYTES)?
    } else {
        let end = file.metadata()?.len();
        let start = if source.process {
            end.saturating_sub(output::LIMIT as u64)
        } else {
            0
        };
        let length = end - start;
        if offset > length {
            return Err(std::io::Error::other(
                "log was truncated; reopen the source",
            ));
        }
        file.seek(SeekFrom::Start(start + offset))?;
        let mut bytes = Vec::new();
        file.take(PAGE_BYTES as u64).read_to_end(&mut bytes)?;
        let more = offset + (bytes.len() as u64) < length;
        (bytes, offset, more)
    };
    // Keep record boundaries for normal JSONL records. Very large records are
    // shown as explicit parse errors, never silently skipped.
    if more {
        if let Some(end) = bytes.iter().rposition(|b| *b == b'\n') {
            bytes.truncate(end + 1);
        }
    }
    let next = offset + bytes.len() as u64;
    let contents = String::from_utf8_lossy(&bytes);
    let mut lines = Vec::new();
    for line in contents.lines() {
        if source.json {
            match serde_json::from_str::<LogEntry>(line) {
                Ok(entry) => {
                    let stream = match entry.stream {
                        LogStream::Stdout => "stdout",
                        LogStream::Stderr => "stderr",
                        LogStream::Worker => "worker",
                    };
                    for line in entry.text.split('\n') {
                        lines.push(format!("[{} {stream}] {}", entry.cursor, clean(line)));
                    }
                }
                Err(error) => lines.push(format!("! JSONL: {error}: {}", clean(line))),
            }
        } else if source.process {
            lines.push(clean(&rospm_core::diagnostic::strip_ansi(line)));
        } else {
            lines.push(clean(line));
        }
    }
    Ok((lines, next, more))
}
fn clean(line: &str) -> String {
    line.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Diagnose only the compatibility build log; do not combine unrelated tasks.
pub(crate) fn diagnostics(root: &Path) -> Vec<String> {
    let source = Source {
        path: root.join(".rospm/log/build.log"),
        label: "build.log".into(),
        json: false,
        process: false,
    };
    let Ok((lines, _, _)) = read_page(&source, 0) else {
        return Vec::new();
    };
    let output = lines.join("\n");
    let mut diagnostics = rospm_core::diagnostic::parse_cpp(&output);
    diagnostics.extend(rospm_core::diagnostic::parse_python(&output, None));
    diagnostics
        .into_iter()
        .map(|d| format!("{:?}: {}", d.code, d.explanation))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_browser_opens_the_focused_process_without_hiding_workflow_logs() {
        let transcript = File::open("/dev/null").unwrap();
        let process = RunStatus {
            id: "run-test".into(),
            target: "pkg/node".into(),
            background: false,
            task: false,
            shell: false,
            pid: None,
            suspended: false,
            output_fd: Some(transcript.as_raw_fd()),
            exit: None,
        };
        let mut browser = Browser::new(
            Some(Path::new(".")),
            std::slice::from_ref(&process),
            Some(&process.id),
        );
        assert_eq!(browser.sources[browser.selected].label, "pkg/node");
        browser.select(true);
        assert_eq!(browser.sources[browser.selected].label, "build.log");
        assert!(browser
            .sources
            .iter()
            .any(|source| source.label == "deps.log"));
    }

    #[test]
    fn pages_preserve_order_and_json_streams() {
        let dir = std::env::temp_dir().join(format!("rospm-log-view-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let source = Source {
            path: dir.join("log"),
            label: "test".into(),
            json: false,
            process: false,
        };
        let text = "hello\n".repeat(PAGE_BYTES);
        fs::write(&source.path, &text).unwrap();
        let (first, next, more) = read_page(&source, 0).unwrap();
        assert!(more);
        assert!(next <= PAGE_BYTES as u64);
        assert_eq!(first.join("\n") + "\n", text[..next as usize]);
        let (second, _, _) = read_page(&source, next).unwrap();
        assert_eq!(second[0], "hello");
        fs::write(
            &source.path,
            "{\"cursor\":7,\"stream\":\"stderr\",\"text\":\"failure\\ncontext\"}\ninvalid\n",
        )
        .unwrap();
        let source = Source {
            json: true,
            ..source
        };
        let (lines, _, _) = read_page(&source, 0).unwrap();
        assert_eq!(lines[0], "[7 stderr] failure");
        assert_eq!(lines[1], "[7 stderr] context");
        assert!(lines[2].starts_with("! JSONL:"));
        fs::remove_dir_all(dir).unwrap();
    }
}
