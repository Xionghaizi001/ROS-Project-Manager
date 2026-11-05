//! Plan execution, subprocess streaming, key fetching, and file writes.

use super::*;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::sync::atomic::{AtomicBool, Ordering};

impl Plan {
    /// Render the plan to `out`, then execute it unless `dry_run`.
    pub fn execute(&self, dry_run: bool, out: &mut dyn Write) -> Result<()> {
        let stdout = || PathBuf::from("<stdout>");
        write!(out, "{}", self.render()).map_err(|e| Error::io(stdout(), e))?;
        if dry_run {
            writeln!(out, "# {}", msg!("plan.dry_run")).map_err(|e| Error::io(stdout(), e))?;
            return Ok(());
        }
        self.perform(out)
    }

    /// Execute the steps without rendering first. Callers that need a
    /// confirmation between the rendered plan and execution use this.
    pub fn perform(&self, out: &mut dyn Write) -> Result<()> {
        self.perform_inner(out, None)
    }

    /// Execute with structured events and captured child output, without writing
    /// to the frontend terminal. The caller owns task identity and renders the plan.
    /// Child stdin is closed; interactive sudo/shell authorization is a separate
    /// frontend responsibility. Cancellation is cooperative at whole-step
    /// boundaries by default. Worker adapters also isolate subprocess groups and
    /// may signal explicitly interruptible commands; completed changes are not
    /// rolled back. Ordinary command failures take precedence over cancellation.
    pub fn perform_with(&self, task: &str, interaction: &dyn Interaction) -> Result<()> {
        interaction.event(Event::Task {
            task,
            state: State::Running,
            total: self.steps.len(),
            error: None,
        });
        let result = self.perform_inner(&mut std::io::sink(), Some((task, interaction)));
        interaction.event(Event::Task {
            task,
            state: match &result {
                Ok(()) => State::Succeeded,
                Err(Error::Cancelled) => State::Cancelled,
                Err(_) => State::Failed,
            },
            total: self.steps.len(),
            error: result.as_ref().err().map(Error::message),
        });
        result
    }

    fn perform_inner(
        &self,
        out: &mut dyn Write,
        observer: Option<(&str, &dyn Interaction)>,
    ) -> Result<()> {
        let checkpoint = |step| match observer {
            Some((task, interaction)) => crate::execution::checkpoint(task, step, interaction),
            None => Ok(()),
        };
        checkpoint(0)?;
        let stdout = || PathBuf::from("<stdout>");
        let started = std::time::SystemTime::now();
        let mut log: Option<std::fs::File> = None;
        for (i, step) in self.steps.iter().enumerate() {
            let process_cancelled = AtomicBool::new(false);
            let execution = observer.map(|(task, interaction)| Execution {
                task,
                step: i + 1,
                interaction,
                process_cancelled: Some(&process_cancelled),
                interruptible: matches!(step, Step::Run(c) if c.interruptible && !c.sudo),
            });
            if let Some(e) = &execution {
                e.interaction.event(Event::Step {
                    task: e.task,
                    step: e.step,
                    state: State::Running,
                });
            }
            let mut progress = Progress {
                out,
                execution: execution.as_ref(),
            };
            let out: &mut dyn Write = &mut progress;
            let result = (|| -> Result<()> {
                match step {
                    Step::Conda(step) => {
                        writeln!(out, "==> [{}] {}", i + 1, step.render())
                            .map_err(|e| Error::io(stdout(), e))?;
                        step.perform_observed(out, execution.as_ref())?;
                    }
                    Step::Run(c) => {
                        writeln!(out, "==> [{}] {}", i + 1, c.render())
                            .map_err(|e| Error::io(stdout(), e))?;
                        let status = match (&self.log, &c.log) {
                            (Some(path), Some(header)) => {
                                if log.is_none() {
                                    writeln!(
                                        out,
                                        "==> # {}",
                                        msg!("plan.log", path = path.display())
                                    )
                                    .map_err(|e| Error::io(stdout(), e))?;
                                    log = Some(open_log(path).map_err(|e| Error::io(path, e))?);
                                }
                                out.flush().map_err(|e| Error::io(stdout(), e))?;
                                let file = log.as_ref().expect("opened above");
                                run_logged(c, header, file, execution.as_ref())
                                    .map_err(|e| Error::io(path, e))?
                            }
                            _ => run_process(
                                c.to_process(execution.is_some()),
                                execution.as_ref(),
                                None,
                            )
                            .map_err(|e| Error::io(&c.program, e))?,
                        };
                        if process_cancelled.load(Ordering::Acquire)
                            && (status.success()
                                || matches!(status.signal(), Some(libc::SIGTERM | libc::SIGKILL)))
                        {
                            return Err(Error::Cancelled);
                        }
                        if !status.success() {
                            return Err(Error::CommandFailed {
                                command: c.render(),
                                status: status.to_string(),
                            });
                        }
                    }
                    Step::WriteFile {
                        path,
                        contents,
                        sudo,
                    } => {
                        writeln!(
                            out,
                            "==> [{}] {}",
                            i + 1,
                            msg!("plan.writing", path = path.display())
                        )
                        .map_err(|e| Error::io(stdout(), e))?;
                        if *sudo {
                            write_file_sudo(path, contents, execution.as_ref())?;
                        } else {
                            if let Some(parent) = path.parent() {
                                std::fs::create_dir_all(parent)
                                    .map_err(|e| Error::io(parent, e))?;
                            }
                            write_atomic(path, contents.as_bytes())
                                .map_err(|e| Error::io(path, e))?;
                        }
                    }
                    Step::FetchKey {
                        url,
                        dest,
                        fingerprint,
                    } => {
                        writeln!(
                            out,
                            "==> [{}] {}",
                            i + 1,
                            msg!(
                                "plan.fetching_key",
                                url = url,
                                dest = dest.display(),
                                fingerprint = fingerprint
                            )
                        )
                        .map_err(|e| Error::io(stdout(), e))?;
                        fetch_key_observed(url, dest, fingerprint, execution.as_ref())?;
                    }
                    Step::Note(text) => {
                        writeln!(out, "==> # {text}").map_err(|e| Error::io(stdout(), e))?;
                    }
                }
                Ok(())
            })();
            if let Some(e) = &execution {
                if let Err(error) = &result {
                    if let Some(summary) = crate::diagnostic::summary::collect(self, error, started)
                    {
                        e.interaction.event(Event::Diagnostics {
                            task: e.task,
                            step: e.step,
                            summary: &summary,
                        });
                    }
                }
                e.interaction.event(Event::Step {
                    task: e.task,
                    step: e.step,
                    state: match &result {
                        Ok(()) => State::Succeeded,
                        Err(Error::Cancelled) => State::Cancelled,
                        Err(_) => State::Failed,
                    },
                });
            }
            result?;
            checkpoint(i + 1)?;
        }
        Ok(())
    }
}

fn open_log(path: &Path) -> std::io::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::File::create(path)
}

/// Run `c` with its stdout / stderr forwarded live and copied to `log`.
/// One reader thread per pipe, fixed-size buffers: neither pipe can fill up
/// and block the child, and memory stays bounded however long the output.
/// Bytes are kept verbatim (ANSI colors included); parsers strip them.
fn run_logged(
    c: &Command,
    header: &str,
    mut log: &std::fs::File,
    execution: Option<&Execution<'_>>,
) -> std::io::Result<process::ExitStatus> {
    let cwd = c.cwd.as_deref().map_or_else(
        || std::env::current_dir().unwrap_or_default(),
        Path::to_path_buf,
    );
    writeln!(
        log,
        "==> command: {}\ncwd: {}\n{}",
        c.render(),
        cwd.display(),
        header
    )?;
    let status = run_process(c.to_process(execution.is_some()), execution, Some(log))?;
    writeln!(log, "==> {status}\n")?;
    Ok(status)
}

/// Shared subprocess path for plans and Conda. Both pipes are always drained.
pub(crate) fn run_process(
    mut command: process::Command,
    execution: Option<&Execution<'_>>,
    log: Option<&std::fs::File>,
) -> std::io::Result<process::ExitStatus> {
    if execution.is_none() && log.is_none() {
        return command.status();
    }
    let isolated = execution.is_some_and(|e| e.interaction.process_groups());
    if isolated {
        command.process_group(0);
    }
    if execution.is_some() {
        command.stdin(Stdio::null());
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let (status, out, err) = std::thread::scope(|scope| {
        let reader = scope.spawn(|| drain_output(stdout, Stream::Stdout, execution, log));
        let errors = scope.spawn(|| drain_output(stderr, Stream::Stderr, execution, log));
        let status = if isolated {
            wait_group(&mut child, execution.expect("isolated worker"), || {
                reader.is_finished() && errors.is_finished()
            })
        } else {
            child.wait()
        };
        (
            status,
            reader.join().expect("output reader panicked"),
            errors.join().expect("output reader panicked"),
        )
    });
    let status = status?;
    out?;
    err?;
    Ok(status)
}

// Keep the leader unreaped until pipes close: its PID/PGID cannot be reused
// while cancellation targets descendants that still hold the pipe descriptors.
fn wait_group(
    child: &mut process::Child,
    execution: &Execution<'_>,
    drained: impl Fn() -> bool,
) -> std::io::Result<process::ExitStatus> {
    use std::time::{Duration, Instant};
    let pgid = child.id() as i32;
    let mut sent = None;
    let mut killed = false;
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                child.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if rc != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if unsafe { info.si_pid() } != 0 && drained() {
            return child.wait();
        }
        if execution.interruptible && execution.interaction.cancel_requested(execution.task) {
            let signal = if sent.is_none() {
                Some(libc::SIGTERM)
            } else if !killed
                && sent.is_some_and(|at: Instant| at.elapsed() >= Duration::from_secs(2))
            {
                Some(libc::SIGKILL)
            } else {
                None
            };
            if let Some(signal) = signal {
                if unsafe { libc::kill(-pgid, signal) } != 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ESRCH) {
                        return Err(error);
                    }
                } else {
                    if let Some(cancelled) = execution.process_cancelled {
                        cancelled.store(true, Ordering::Release);
                    }
                    execution.interaction.event(Event::Interaction {
                        task: execution.task,
                        step: execution.step,
                        state: State::CancelRequested,
                    });
                }
                if signal == libc::SIGTERM {
                    sent = Some(Instant::now());
                } else {
                    killed = true;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn drain_output(
    mut from: impl std::io::Read,
    stream: Stream,
    execution: Option<&Execution<'_>>,
    mut log: Option<&std::fs::File>,
) -> std::io::Result<()> {
    use std::os::fd::AsFd;
    // Duplicate descriptors to avoid deadlocking the CLI's held StdoutLock.
    let terminal = if execution.is_none() {
        Some(
            match stream {
                Stream::Stdout => std::io::stdout().as_fd().try_clone_to_owned(),
                _ => std::io::stderr().as_fd().try_clone_to_owned(),
            }
            .map(std::fs::File::from),
        )
    } else {
        None
    };
    let mut error = None;
    let mut terminal = match terminal {
        Some(Ok(file)) => Some(file),
        Some(Err(e)) => {
            error = Some(e);
            None
        }
        None => None,
    };
    let mut buf = [0; 8192];
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                error.get_or_insert(e);
                break;
            }
        };
        if let Some(e) = execution {
            e.log(stream, &buf[..n]);
        }
        if let Some(file) = &mut terminal {
            // A disconnected terminal must not prevent the log from completing.
            let _ = file.write_all(&buf[..n]);
        }
        if let Some(file) = &mut log {
            if let Err(e) = file.write_all(&buf[..n]) {
                error.get_or_insert(e);
            }
        }
    }
    match error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Download `url` as the current user (bounded by timeouts and retries, so a
/// stalled connection fails instead of hanging `apply` forever), check the
/// key's fingerprint, and only then `sudo install` it to `dest`.
#[cfg(test)]
pub(crate) fn fetch_key(url: &str, dest: &Path, fingerprint: &str) -> Result<()> {
    fetch_key_observed(url, dest, fingerprint, None)
}

fn fetch_key_observed(
    url: &str,
    dest: &Path,
    fingerprint: &str,
    execution: Option<&Execution<'_>>,
) -> Result<()> {
    // Unique per process *and* per call: several plans (or tests) in one
    // process must not share the temporary file.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = std::env::temp_dir().join(format!(
        "rospm-key-{}-{}.gpg",
        process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _cleanup = RemoveOnDrop(tmp.clone());

    // `--retry` only repeats transient failures (timeouts, 5xx); a 404 fails
    // at once. `--max-time` bounds each attempt, so a stalled connection
    // costs at most a few minutes instead of hanging `apply` forever.
    let curl = Command::new("curl").args([
        "-fsSL",
        "--connect-timeout",
        "15",
        "--max-time",
        "60",
        "--retry",
        "2",
        url,
        "-o",
        &tmp.to_string_lossy(),
    ]);
    let status = run_process(curl.to_process(execution.is_some()), execution, None)
        .map_err(|e| Error::io("curl", e))?;
    if !status.success() {
        return Err(Error::CommandFailed {
            command: curl.render(),
            status: status.to_string(),
        });
    }

    let gpg = process::Command::new("gpg")
        .args([
            "--batch",
            "--no-default-keyring",
            "--with-colons",
            "--with-fingerprint",
            "--show-keys",
        ])
        .arg(&tmp)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::io("gpg", e))?;
    if !gpg.status.success() {
        return Err(Error::Host(msg!(
            "plan.key_unreadable",
            url = url,
            error = String::from_utf8_lossy(&gpg.stderr).trim()
        )));
    }
    let found = fingerprints(&String::from_utf8_lossy(&gpg.stdout));
    if !is_only_key(&found, fingerprint) {
        return Err(Error::Host(msg!(
            "plan.key_fingerprint_mismatch",
            url = url,
            found = if found.is_empty() {
                "<none>".to_string()
            } else {
                found.join(", ")
            },
            expected = fingerprint,
            dest = dest.display()
        )));
    }

    // apt's `signed-by=` only accepts ASCII-armoured keys in files named
    // `*.asc`; our keyrings are `*.gpg`, so armoured downloads (the
    // keyserver's snapshot key; `ros.key` is already binary) are converted.
    // Found in the 2026-09-26 real `apply --locked` run: NO_PUBKEY.
    let bytes = std::fs::read(&tmp).map_err(|e| Error::io(tmp.clone(), e))?;
    let binary = tmp.with_extension("bin");
    let _cleanup_binary = RemoveOnDrop(binary.clone());
    let to_install = if is_ascii_armored(&bytes) {
        let out = process::Command::new("gpg")
            .args(["--batch", "--yes", "--dearmor", "--output"])
            .arg(&binary)
            .arg(&tmp)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| Error::io("gpg", e))?;
        if !out.status.success() {
            return Err(Error::Host(msg!(
                "plan.key_unreadable",
                url = url,
                error = String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        &binary
    } else {
        &tmp
    };

    let install = Command::new("install")
        .args(["-D", "-m", "0644", "-o", "root", "-g", "root"])
        .arg(to_install.to_string_lossy())
        .arg(dest.to_string_lossy())
        .sudo();
    let status = run_process(install.to_process(execution.is_some()), execution, None)
        .map_err(|e| Error::io("sudo", e))?;
    if !status.success() {
        return Err(Error::CommandFailed {
            command: install.render(),
            status: status.to_string(),
        });
    }
    Ok(())
}

/// Whether `bytes` is an ASCII-armoured OpenPGP block rather than a binary
/// keyring (apt only reads binary keyrings from `*.gpg` files).
pub fn is_ascii_armored(bytes: &[u8]) -> bool {
    let start = bytes
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    bytes[start..].starts_with(b"-----BEGIN PGP")
}

/// True when the key file holds exactly one primary key and it is `expected`.
/// Any extra key would be trusted by apt too, so a mirror could smuggle its
/// own signing key next to the real one.
pub fn is_only_key(found: &[String], expected: &str) -> bool {
    matches!(found, [only] if only.eq_ignore_ascii_case(expected))
}

/// Primary-key fingerprints (`fpr:` records that follow a `pub:` record) from
/// `gpg --with-colons --with-fingerprint --show-keys` output.
pub fn fingerprints(colons: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut after_pub = false;
    for line in colons.lines() {
        let mut fields = line.split(':');
        match fields.next() {
            Some("pub") => after_pub = true,
            Some("sub") | Some("ssb") | Some("sec") => after_pub = false,
            Some("fpr") if after_pub => {
                // fpr:::::::::<FINGERPRINT>:
                if let Some(fp) = line.split(':').nth(9) {
                    if !fp.is_empty() {
                        out.push(fp.to_ascii_uppercase());
                    }
                }
                after_pub = false;
            }
            _ => {}
        }
    }
    out
}

struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// `<path>.tmp`: the temporary sibling an atomic write renames over `path`.
fn tmp_sibling(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Write `<path>.tmp`, flush it to disk, then rename it over `path`: an
/// interruption or a full disk leaves the old file intact, never half of it.
pub fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let tmp = tmp_sibling(path);
    let written = std::fs::File::create(&tmp)
        .and_then(|mut f| f.write_all(contents).and_then(|()| f.sync_all()))
        .and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// `sudo mkdir -p <parent>`, `sudo tee <path>.tmp` (contents through stdin, so
/// they never appear on a command line), then `sudo mv -f` it over `path`.
fn write_file_sudo(path: &Path, contents: &str, execution: Option<&Execution<'_>>) -> Result<()> {
    let failed = |program: &str, status: process::ExitStatus| Error::CommandFailed {
        command: format!("sudo {program} {}", path.display()),
        status: status.to_string(),
    };
    let sudo = |args: &[&std::ffi::OsStr]| {
        let mut command = process::Command::new("sudo");
        if execution.is_some() {
            command.arg("-n");
        }
        command.args(args);
        run_process(command, execution, None).map_err(|e| Error::io("sudo", e))
    };
    if let Some(parent) = path.parent() {
        let status = sudo(&["mkdir".as_ref(), "-p".as_ref(), parent.as_ref()])?;
        if !status.success() {
            return Err(failed("mkdir -p", status));
        }
    }
    let tmp = tmp_sibling(path);
    let mut command = process::Command::new("sudo");
    if execution.is_some() {
        command.arg("-n");
    }
    command
        .arg("tee")
        .arg(&tmp)
        .stdin(Stdio::piped())
        .stdout(Stdio::null());
    let mut child = command
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::io("sudo", e))?;
    let stderr = child.stderr.take().expect("piped");
    let (written, drained) = std::thread::scope(|scope| {
        let drain = scope.spawn(|| drain_output(stderr, Stream::Stderr, execution, None));
        let written = child
            .stdin
            .take()
            .map_or(Ok(()), |mut stdin| stdin.write_all(contents.as_bytes()));
        (written, drain.join().expect("output reader panicked"))
    });
    let status = child.wait().map_err(|e| Error::io("sudo", e))?;
    let written = written.and(drained);
    let result = match written {
        Err(e) => Err(Error::io(&tmp, e)),
        Ok(()) if !status.success() => Err(failed("tee", status)),
        Ok(()) => match sudo(&["mv".as_ref(), "-f".as_ref(), tmp.as_ref(), path.as_ref()])? {
            s if s.success() => return Ok(()),
            s => Err(failed("mv -f", s)),
        },
    };
    let _ = sudo(&["rm".as_ref(), "-f".as_ref(), tmp.as_ref()]);
    result
}

/// Minimal POSIX single-quote escaping.
pub fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./:=+@%,".contains(&b));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}
