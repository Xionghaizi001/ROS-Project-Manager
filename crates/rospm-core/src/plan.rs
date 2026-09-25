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
}

impl Default for Plan {
    fn default() -> Self {
        Plan {
            title: Message::text(""),
            steps: Vec::new(),
        }
    }
}

impl Plan {
    pub fn new(title: impl Into<Message>) -> Self {
        Plan {
            title: title.into(),
            steps: Vec::new(),
        }
    }

    pub fn run(&mut self, command: Command) -> &mut Self {
        self.steps.push(Step::Run(command));
        self
    }

    pub fn write_file(
        &mut self,
        path: impl Into<PathBuf>,
        contents: impl Into<String>,
    ) -> &mut Self {
        self.steps.push(Step::WriteFile {
            path: path.into(),
            contents: contents.into(),
            sudo: false,
        });
        self
    }

    /// Like [`Plan::write_file`], but the file is owned by root and written
    /// through `sudo tee`. Contents are shown in full by `--dry-run`.
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
        for (i, step) in self.steps.iter().enumerate() {
            match step {
                Step::Run(c) => {
                    writeln!(out, "==> [{}] {}", i + 1, c.render())
                        .map_err(|e| Error::io(stdout(), e))?;
                    let status = c
                        .to_process()
                        .status()
                        .map_err(|e| Error::io(&c.program, e))?;
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
                        std::fs::write(path, contents).map_err(|e| Error::io(path, e))?;
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
    if !found.iter().any(|f| f.eq_ignore_ascii_case(fingerprint)) {
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

    let install = Command::new("install")
        .args(["-D", "-m", "0644", "-o", "root", "-g", "root"])
        .arg(tmp.to_string_lossy())
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

/// `sudo mkdir -p <parent> && printf … | sudo tee <path>`; the contents go
/// through stdin so they never appear on a command line.
fn write_file_sudo(path: &PathBuf, contents: &str) -> Result<()> {
    let failed = |program: &str, status: process::ExitStatus| Error::CommandFailed {
        command: format!("sudo {program} {}", path.display()),
        status: status.to_string(),
    };
    if let Some(parent) = path.parent() {
        let status = process::Command::new("sudo")
            .args(["mkdir", "-p"])
            .arg(parent)
            .status()
            .map_err(|e| Error::io("sudo", e))?;
        if !status.success() {
            return Err(failed("mkdir -p", status));
        }
    }
    let mut child = process::Command::new("sudo")
        .arg("tee")
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| Error::io("sudo", e))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(contents.as_bytes())
            .map_err(|e| Error::io(path, e))?;
    }
    let status = child.wait().map_err(|e| Error::io("sudo", e))?;
    if !status.success() {
        return Err(failed("tee", status));
    }
    Ok(())
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
    use super::{fetch_key, fingerprints};

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
        assert!(err.contains("expects 0000"), "{err}");
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
}
