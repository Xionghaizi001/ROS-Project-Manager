//! Execution plans.
//!
//! Every command in the "mutates the system / produces artifacts" class
//! (decision #2) first builds a [`Plan`], renders it, and only then executes it.
//! `--dry-run` stops after rendering.

use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;
use std::process;

use crate::error::{Error, Result};

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
    },
    /// Informational line, printed but not executed. Used for degradations
    /// ("snapshot unreachable, falling back to …") so they are never silent.
    Note(String),
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub title: String,
    pub steps: Vec<Step>,
}

impl Plan {
    pub fn new(title: impl Into<String>) -> Self {
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
        });
        self
    }

    pub fn note(&mut self, text: impl Into<String>) -> &mut Self {
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
        let mut out = format!("# plan: {}\n", self.title);
        for (i, step) in self.steps.iter().enumerate() {
            let n = i + 1;
            match step {
                Step::Run(c) => {
                    let _ = writeln!(out, "[{n}] $ {}", c.render());
                }
                Step::WriteFile { path, contents } => {
                    let _ = writeln!(
                        out,
                        "[{n}] write {} ({} bytes)",
                        path.display(),
                        contents.len()
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
            writeln!(out, "# dry-run: nothing executed").map_err(|e| Error::io(stdout(), e))?;
            return Ok(());
        }
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
                Step::WriteFile { path, contents } => {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
                    }
                    std::fs::write(path, contents).map_err(|e| Error::io(path, e))?;
                }
                Step::Note(text) => {
                    writeln!(out, "==> # {text}").map_err(|e| Error::io(stdout(), e))?;
                }
            }
        }
        Ok(())
    }
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
