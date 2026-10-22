//! Execution plans.
//!
//! Every command in the "mutates the system / produces artifacts" class
//! (decision #2) first builds a [`Plan`], renders it, and only then executes it.
//! `--dry-run` stops after rendering.

use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{Error, Result};
use crate::message::Message;
use crate::msg;

/// One external command. `preamble` lines (typically `source …`) are run in
/// the same `bash -c` before the command so ROS overlays are visible to it.
#[derive(Debug, Clone)]
pub struct Command {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub preamble: Vec<String>,
    pub sudo: bool,
    /// Header lines (`key: value`) for the plan's build log. When both this
    /// and [`Plan::log`] are set, output is forwarded and also saved there.
    pub log: Option<String>,
}

impl Command {
    pub fn new(program: impl Into<String>) -> Self {
        Command {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            preamble: Vec::new(),
            sudo: false,
            log: None,
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn preamble(mut self, line: impl Into<String>) -> Self {
        self.preamble.push(line.into());
        self
    }

    pub fn sudo(mut self) -> Self {
        self.sudo = true;
        self
    }

    pub fn log(mut self, header: impl Into<String>) -> Self {
        self.log = Some(header.into());
        self
    }

    /// Shell-like one-liner for humans and logs.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let mut parts: Vec<String> = Vec::new();
        for (k, v) in &self.env {
            parts.push(format!("{k}={}", shell_quote(v)));
        }
        if self.sudo {
            parts.push("sudo".into());
        }
        parts.push(shell_quote(&self.program));
        parts.extend(self.args.iter().map(|a| shell_quote(a)));
        let command = parts.join(" ");

        let body = if self.preamble.is_empty() {
            command
        } else {
            format!("{} && {command}", self.preamble.join(" && "))
        };
        match &self.cwd {
            Some(dir) => {
                let _ = write!(
                    out,
                    "(cd {} && {body})",
                    shell_quote(&dir.to_string_lossy())
                );
            }
            None => out.push_str(&body),
        }
        out
    }

    fn to_process(&self) -> process::Command {
        let mut cmd = if !self.preamble.is_empty() {
            // Run through bash so `source` lines take effect.
            let mut inner: Vec<String> = Vec::new();
            if self.sudo {
                inner.push("sudo".into());
            }
            inner.push(shell_quote(&self.program));
            inner.extend(self.args.iter().map(|a| shell_quote(a)));
            let script = format!(
                "set -e\n{}\nexec {}",
                self.preamble.join("\n"),
                inner.join(" ")
            );
            let mut c = process::Command::new("bash");
            c.arg("-c").arg(script);
            c
        } else if self.sudo {
            let mut c = process::Command::new("sudo");
            c.arg(&self.program).args(&self.args);
            c
        } else {
            let mut c = process::Command::new(&self.program);
            c.args(&self.args);
            c
        };
        if let Some(dir) = &self.cwd {
            cmd.current_dir(dir);
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd
    }
}

#[derive(Debug, Clone)]
pub enum Step {
    Run(Command),
    /// Check an existing Conda environment or prepare a project-owned one.
    Conda(Box<crate::conda::CondaStep>),
    WriteFile {
        path: PathBuf,
        contents: String,
        /// Write through `sudo tee` (system files such as apt sources).
        sudo: bool,
    },
    /// Download an OpenPGP keyring to a temporary file, verify its full
    /// fingerprint with `gpg`, then install it as root. The download runs
    /// with a timeout and retries; a key with the wrong fingerprint is never
    /// written to `dest`.
    FetchKey {
        url: String,
        dest: PathBuf,
        /// Expected fingerprint, 40 hex digits, no spaces.
        fingerprint: String,
    },
    /// Informational line, printed but not executed. Used for degradations
    /// ("snapshot unreachable, falling back to …") so they are never silent.
    Note(Message),
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub title: Message,
    pub steps: Vec<Step>,
    /// Build log, rewritten by each run that executes a logged command.
    pub log: Option<PathBuf>,
}

impl Default for Plan {
    fn default() -> Self {
        Plan {
            title: Message::text(""),
            steps: Vec::new(),
            log: None,
        }
    }
}

impl Plan {
    pub fn new(title: impl Into<Message>) -> Self {
        Plan {
            title: title.into(),
            steps: Vec::new(),
            log: None,
        }
    }

    pub fn run(&mut self, command: Command) -> &mut Self {
        self.steps.push(Step::Run(command));
        self
    }

    /// A root-owned file, written through `sudo tee`. Contents are shown in full by `--dry-run`.
    pub fn write_file_sudo(
        &mut self,
        path: impl Into<PathBuf>,
        contents: impl Into<String>,
    ) -> &mut Self {
        self.steps.push(Step::WriteFile {
            path: path.into(),
            contents: contents.into(),
            sudo: true,
        });
        self
    }

    /// See [`Step::FetchKey`].
    pub fn fetch_key(
        &mut self,
        url: impl Into<String>,
        dest: impl Into<PathBuf>,
        fingerprint: impl Into<String>,
    ) -> &mut Self {
        self.steps.push(Step::FetchKey {
            url: url.into(),
            dest: dest.into(),
            fingerprint: fingerprint.into(),
        });
        self
    }

    pub fn note(&mut self, text: impl Into<Message>) -> &mut Self {
        self.steps.push(Step::Note(text.into()));
        self
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Number of steps that actually do something (excludes notes).
    pub fn action_count(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| !matches!(s, Step::Note(_)))
            .count()
    }

    pub fn render(&self) -> String {
        let mut out = format!("# {}\n", msg!("plan.title", title = &self.title));
        for (i, step) in self.steps.iter().enumerate() {
            let n = i + 1;
            match step {
                Step::Conda(step) => {
                    let _ = writeln!(out, "[{n}] {}", step.render());
                }
                Step::Run(c) => {
                    let _ = writeln!(out, "[{n}] $ {}", c.render());
                }
                Step::WriteFile {
                    path,
                    contents,
                    sudo,
                } => {
                    let what = if *sudo {
                        msg!(
                            "plan.write_file_sudo",
                            path = path.display(),
                            bytes = contents.len()
                        )
                    } else {
                        msg!(
                            "plan.write_file",
                            path = path.display(),
                            bytes = contents.len()
                        )
                    };
                    let _ = writeln!(out, "[{n}] {what}");
                    // Small system files (apt sources) are worth reviewing verbatim.
                    if contents.lines().count() <= 16 {
                        for line in contents.lines() {
                            let _ = writeln!(out, "      | {line}");
                        }
                    }
                }
                Step::FetchKey {
                    url,
                    dest,
                    fingerprint,
                } => {
                    let _ = writeln!(
                        out,
                        "[{n}] {}",
                        msg!(
                            "plan.fetch_key",
                            url = url,
                            dest = dest.display(),
                            fingerprint = fingerprint
                        )
                    );
                }
                Step::Note(text) => {
                    let _ = writeln!(out, "[{n}] # {text}");
                }
            }
        }
        out
    }

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
        let stdout = || PathBuf::from("<stdout>");
        let mut log: Option<std::fs::File> = None;
        for (i, step) in self.steps.iter().enumerate() {
            match step {
                Step::Conda(step) => {
                    writeln!(out, "==> [{}] {}", i + 1, step.render())
                        .map_err(|e| Error::io(stdout(), e))?;
                    step.perform(out)?;
                }
                Step::Run(c) => {
                    writeln!(out, "==> [{}] {}", i + 1, c.render())
                        .map_err(|e| Error::io(stdout(), e))?;
                    let status = match (&self.log, &c.log) {
                        (Some(path), Some(header)) => {
                            if log.is_none() {
                                writeln!(out, "==> # {}", msg!("plan.log", path = path.display()))
                                    .map_err(|e| Error::io(stdout(), e))?;
                                log = Some(open_log(path).map_err(|e| Error::io(path, e))?);
                            }
                            out.flush().map_err(|e| Error::io(stdout(), e))?;
                            let file = log.as_ref().expect("opened above");
                            run_logged(c, header, file).map_err(|e| Error::io(path, e))?
                        }
                        _ => c
                            .to_process()
                            .status()
                            .map_err(|e| Error::io(&c.program, e))?,
                    };
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
                        write_file_sudo(path, contents)?;
                    } else {
                        if let Some(parent) = path.parent() {
                            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
                        }
                        write_atomic(path, contents.as_bytes()).map_err(|e| Error::io(path, e))?;
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
                    fetch_key(url, dest, fingerprint)?;
                }
                Step::Note(text) => {
                    writeln!(out, "==> # {text}").map_err(|e| Error::io(stdout(), e))?;
                }
            }
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
) -> std::io::Result<process::ExitStatus> {
    use std::io::Read;
    use std::os::fd::AsFd;

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
    let mut child = c
        .to_process()
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let child_out = child.stdout.take().expect("piped");
    let child_err = child.stderr.take().expect("piped");
    // A duplicate of fd 1: the caller may hold the `Stdout` lock.
    let our_out = std::fs::File::from(std::io::stdout().as_fd().try_clone_to_owned()?);
    let tee = |mut from: Box<dyn Read + Send>, mut to: Box<dyn Write + Send>| {
        let mut buf = [0u8; 8192];
        let mut log = log;
        while let Ok(n) = from.read(&mut buf) {
            if n == 0 {
                break;
            }
            // Keep draining on write errors (closed terminal, full disk) so
            // the child never blocks on a full pipe.
            let _ = to.write_all(&buf[..n]);
            let _ = log.write_all(&buf[..n]);
        }
    };
    std::thread::scope(|s| {
        s.spawn(|| tee(Box::new(child_out), Box::new(our_out)));
        tee(Box::new(child_err), Box::new(std::io::stderr()));
    });
    let status = child.wait()?;
    writeln!(log, "==> {status}\n")?;
    Ok(status)
}

/// Download `url` as the current user (bounded by timeouts and retries, so a
/// stalled connection fails instead of hanging `apply` forever), check the
/// key's fingerprint, and only then `sudo install` it to `dest`.
fn fetch_key(url: &str, dest: &Path, fingerprint: &str) -> Result<()> {
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
    let status = curl
        .to_process()
        .status()
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
    let status = install
        .to_process()
        .status()
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
fn write_file_sudo(path: &Path, contents: &str) -> Result<()> {
    let failed = |program: &str, status: process::ExitStatus| Error::CommandFailed {
        command: format!("sudo {program} {}", path.display()),
        status: status.to_string(),
    };
    let sudo = |args: &[&std::ffi::OsStr]| {
        process::Command::new("sudo")
            .args(args)
            .status()
            .map_err(|e| Error::io("sudo", e))
    };
    if let Some(parent) = path.parent() {
        let status = sudo(&["mkdir".as_ref(), "-p".as_ref(), parent.as_ref()])?;
        if !status.success() {
            return Err(failed("mkdir -p", status));
        }
    }
    let tmp = tmp_sibling(path);
    let mut child = process::Command::new("sudo")
        .arg("tee")
        .arg(&tmp)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| Error::io("sudo", e))?;
    // Close stdin (end of input) before waiting, and wait even if writing failed.
    let written = child
        .stdin
        .take()
        .map_or(Ok(()), |mut stdin| stdin.write_all(contents.as_bytes()));
    let status = child.wait().map_err(|e| Error::io("sudo", e))?;
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

#[cfg(test)]
mod tests {
    use super::{fetch_key, fingerprints, is_ascii_armored, is_only_key, write_atomic};

    const ROS_KEY_URL: &str = "https://raw.githubusercontent.com/ros/rosdistro/master/ros.key";
    const ROS_KEY_FINGERPRINT: &str = "C1CF6E31E6BADE8868B172B4F42ED6FBAB17C654";

    /// Needs network, `gpg` and password-less sudo: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn fetch_key_installs_the_ros_key_when_the_fingerprint_matches() {
        let dest = std::env::temp_dir().join(format!("rospm-test-{}.gpg", std::process::id()));
        fetch_key(ROS_KEY_URL, &dest, ROS_KEY_FINGERPRINT).unwrap();
        let meta = std::fs::metadata(&dest).unwrap();
        assert!(meta.len() > 100);
        let _ = std::process::Command::new("sudo")
            .args(["rm", "-f"])
            .arg(&dest)
            .status();
    }

    /// Needs network and `gpg`; must fail before touching `dest`.
    #[test]
    #[ignore]
    fn fetch_key_refuses_a_wrong_fingerprint() {
        let dest = std::env::temp_dir().join(format!("rospm-test-bad-{}.gpg", std::process::id()));
        let err = fetch_key(
            ROS_KEY_URL,
            &dest,
            "0000000000000000000000000000000000000000",
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("C1CF6E31E6BADE8868B172B4F42ED6FBAB17C654"),
            "{err}"
        );
        assert!(err.contains("plan.key_fingerprint_mismatch"), "{err}");
        assert!(
            err.contains("expected=0000000000000000000000000000000000000000"),
            "{err}"
        );
        assert!(!dest.exists());
    }

    /// Needs network; a 404 must be an error, not an HTML file installed as a key.
    #[test]
    #[ignore]
    fn fetch_key_fails_on_http_error() {
        let dest = std::env::temp_dir().join(format!("rospm-test-404-{}.gpg", std::process::id()));
        let err = fetch_key(
            "https://raw.githubusercontent.com/ros/rosdistro/master/does-not-exist.key",
            &dest,
            ROS_KEY_FINGERPRINT,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("curl"), "{err}");
        assert!(!dest.exists());
    }

    /// Needs network, `gpg` and password-less sudo: the keyserver serves the
    /// snapshot key armoured; what lands in the keyring must be binary.
    #[test]
    #[ignore]
    fn fetch_key_dearmors_the_keyserver_snapshot_key() {
        let dest = std::env::temp_dir().join(format!("rospm-test-snap-{}.gpg", std::process::id()));
        let fp = crate::snapshot::ROS_SNAPSHOT_KEY_FINGERPRINT;
        fetch_key(&crate::snapshot::keyserver_key_url(fp), &dest, fp).unwrap();
        let bytes = std::fs::read(&dest).unwrap();
        let _ = std::process::Command::new("sudo")
            .args(["rm", "-f"])
            .arg(&dest)
            .status();
        assert!(bytes.len() > 100);
        assert!(!is_ascii_armored(&bytes));
    }

    #[test]
    fn is_ascii_armored_detects_armour_only() {
        assert!(is_ascii_armored(
            b"-----BEGIN PGP PUBLIC KEY BLOCK-----\nVersion: x\n"
        ));
        assert!(is_ascii_armored(
            b"\n  -----BEGIN PGP PUBLIC KEY BLOCK-----\n"
        ));
        assert!(!is_ascii_armored(&[0x99, 0x02, 0x0d, 0x04]));
        assert!(!is_ascii_armored(b""));
    }

    #[test]
    fn fingerprints_takes_primary_keys_only() {
        let colons = "\
pub:-:4096:1:F42ED6FBAB17C654:1559176854:1906547974::-:::scSC::::::23::0:
fpr:::::::::C1CF6E31E6BADE8868B172B4F42ED6FBAB17C654:
uid:-::::1746016774::040AC5A3724F8A5146B31C136FD89A85D616FAF9::Open Robotics <info@osrfoundation.org>::::::::::0:
sub:-:4096:1:0123456789ABCDEF:1559176854::::::e::::::23:
fpr:::::::::0000000000000000000000000000000000000000:
";
        assert_eq!(
            fingerprints(colons),
            vec!["C1CF6E31E6BADE8868B172B4F42ED6FBAB17C654".to_string()]
        );
        assert!(fingerprints("").is_empty());
        assert!(fingerprints("<html>not a key</html>\n").is_empty());
    }

    #[test]
    fn is_only_key_rejects_a_smuggled_extra_key() {
        let real = "C1CF6E31E6BADE8868B172B4F42ED6FBAB17C654".to_string();
        let evil = "0000000000000000000000000000000000000000".to_string();
        assert!(is_only_key(
            std::slice::from_ref(&real),
            &real.to_ascii_lowercase()
        ));
        assert!(!is_only_key(&[real.clone(), evil.clone()], &real));
        assert!(!is_only_key(&[evil, real.clone()], &real));
        assert!(!is_only_key(&[], &real));
    }

    #[test]
    fn write_atomic_replaces_whole_files_and_leaves_no_temporary() {
        let dir = std::env::temp_dir().join(format!("rospm-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("rospm.lock");
        write_atomic(&file, b"old").unwrap();
        write_atomic(&file, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        // A failed rename (target is a directory) keeps the target and drops the temporary.
        let blocked = dir.join("blocked");
        std::fs::create_dir_all(blocked.join("inside")).unwrap();
        assert!(write_atomic(&blocked, b"x").is_err());
        assert!(blocked.join("inside").is_dir());
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
