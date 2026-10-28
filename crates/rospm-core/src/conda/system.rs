//! Subprocesses and downloads: the only side effects besides project files.

use std::fs;
use std::path::Path;
use std::process::{self, Stdio};
use std::time::Duration;

use crate::error::{Error, Result};
use crate::msg;
use crate::plan::Command;

/// Subprocesses and downloads, replaced by fakes in tests.
pub(crate) trait System {
    /// Run `program args` and return its stdout.
    fn query(&self, program: &str, args: &[&str]) -> Result<String>;
    fn run(&self, command: &Command) -> Result<()>;
    /// Download `url` to `dest`; the caller verifies the contents.
    fn download(&self, url: &str, dest: &Path) -> Result<()>;
}

pub(super) struct Real;

impl System for Real {
    fn query(&self, program: &str, args: &[&str]) -> Result<String> {
        let output = process::Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| Error::Host(msg!("conda.tool_missing", error = e)))?;
        if !output.status.success() {
            return Err(Error::Host(msg!(
                "conda.query_failed",
                status = output.status,
                error = String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn run(&self, command: &Command) -> Result<()> {
        run(command, None)
    }

    /// Streams to `dest` with bounded connect / response waits and retries
    /// transient failures; HTTP errors other than 5xx fail at once.
    fn download(&self, url: &str, dest: &Path) -> Result<()> {
        let agent: ureq::Agent = crate::net::agent_config()
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .timeout_global(Some(Duration::from_secs(30 * 60)))
            .build()
            .into();
        let mut last = String::new();
        for attempt in 1..=3 {
            if attempt > 1 {
                std::thread::sleep(Duration::from_secs(2 * attempt));
            }
            // (message, worth retrying)
            let result = agent
                .get(url)
                .call()
                .map_err(|e| {
                    let retry = !matches!(e, ureq::Error::StatusCode(s) if s < 500);
                    (e.to_string(), retry)
                })
                .and_then(|mut resp| {
                    let mut file = fs::File::create(dest).map_err(|e| (e.to_string(), false))?;
                    std::io::copy(&mut resp.body_mut().as_reader(), &mut file)
                        .map_err(|e| (e.to_string(), true))?;
                    file.sync_all().map_err(|e| (e.to_string(), false))
                });
            match result {
                Ok(()) => return Ok(()),
                Err((error, retry)) => {
                    last = error;
                    if !retry {
                        break;
                    }
                }
            }
        }
        Err(Error::Host(msg!(
            "conda.download_failed",
            url = url,
            error = last,
            path = dest.with_extension("").display()
        )))
    }
}

/// Keep Conda validation/download behavior identical while capturing run output.
pub(super) struct Observed<'a, 'b>(pub &'a crate::execution::Execution<'b>);
impl System for Observed<'_, '_> {
    fn query(&self, program: &str, args: &[&str]) -> Result<String> {
        Real.query(program, args)
    }
    fn download(&self, url: &str, dest: &Path) -> Result<()> {
        Real.download(url, dest)
    }
    fn run(&self, command: &Command) -> Result<()> {
        run(command, Some(self.0))
    }
}

fn run(command: &Command, execution: Option<&crate::execution::Execution<'_>>) -> Result<()> {
    let mut process = command.to_process(execution.is_some());
    process.stdin(Stdio::null());
    let status = crate::plan::run_process(process, execution, None)
        .map_err(|e| Error::io(&command.program, e))?;
    if !status.success() {
        return Err(Error::CommandFailed {
            command: command.render(),
            status: status.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{Event, Execution, Interaction, Stream};
    use std::sync::Mutex;

    #[test]
    fn conda_runs_use_shared_capture_and_preserve_failures() {
        struct Recorder(Mutex<Vec<u8>>);
        impl Interaction for Recorder {
            fn event(&self, event: Event<'_>) {
                if let Event::Log {
                    stream: Stream::Stderr,
                    bytes,
                    ..
                } = event
                {
                    self.0.lock().unwrap().extend_from_slice(bytes);
                }
            }
        }
        let recorder = Recorder(Mutex::new(Vec::new()));
        let execution = Execution {
            task: "conda",
            step: 1,
            interaction: &recorder,
        };
        let error = Observed(&execution)
            .run(
                &Command::new("bash")
                    .args(["-c", "printf conda-error >&2; exit 3"])
                    .cwd("/tmp"),
            )
            .unwrap_err();
        assert!(matches!(error, Error::CommandFailed { .. }));
        assert_eq!(*recorder.0.lock().unwrap(), b"conda-error");
    }
}
