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
use crate::execution::{Event, Execution, Interaction, Progress, State, Stream};
use crate::message::Message;
use crate::msg;

#[path = "plan_execution.rs"]
mod execution;

#[cfg(test)]
pub(crate) use execution::fetch_key;
pub(crate) use execution::run_process;
pub use execution::{fingerprints, is_ascii_armored, is_only_key, shell_quote, write_atomic};

/// One external command. `preamble` lines (typically `source …`) are run in
/// the same `bash -c` before the command so ROS overlays are visible to it.
#[derive(Debug, Clone, serde::Serialize)]
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

    pub(crate) fn to_process(&self, noninteractive: bool) -> process::Command {
        let mut cmd = if !self.preamble.is_empty() {
            // Run through bash so `source` lines take effect.
            let mut inner: Vec<String> = Vec::new();
            if self.sudo {
                inner.push(if noninteractive { "sudo -n" } else { "sudo" }.into());
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
            if noninteractive {
                c.arg("-n");
            }
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
}

#[cfg(test)]
mod tests {
    #[test]
    fn failed_log_write_is_reported_after_draining_child() {
        struct Sink;
        impl crate::execution::Interaction for Sink {
            fn event(&self, _: crate::execution::Event<'_>) {}
        }
        let execution = crate::execution::Execution {
            task: "full",
            step: 1,
            interaction: &Sink,
        };
        let full = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        let mut command = std::process::Command::new("bash");
        command.args([
            "-c",
            "head -c 200000 /dev/zero & head -c 200000 /dev/zero >&2 & wait",
        ]);
        let error = super::run_process(command, Some(&execution), Some(&full)).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(28));
    }

    #[test]
    fn observed_sudo_never_prompts_including_shell_preamble() {
        let command = super::Command::new("true").sudo();
        let process = command.to_process(true);
        assert_eq!(process.get_args().collect::<Vec<_>>(), ["-n", "true"]);
        let process = command.preamble("true").to_process(true);
        assert!(process
            .get_args()
            .last()
            .unwrap()
            .to_str()
            .unwrap()
            .contains("exec sudo -n true"));
    }

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
