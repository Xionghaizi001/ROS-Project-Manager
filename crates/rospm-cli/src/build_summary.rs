//! CLI presentation of shared build diagnostics.
use rospm_core::diagnostic::summary::DiagnosticStatus;
pub use rospm_core::diagnostic::summary::{collect, Summary};

pub fn print(summary: &Summary) {
    eprintln!("\n{}", rospm_core::msg!("build_summary.title"));
    if let Some(python) = &summary.python {
        eprintln!(
            "  {}",
            rospm_core::msg!(
                "diagnostic.python_selected_interpreter",
                interpreter = python
            )
        );
    }
    match summary.diagnostic_status {
        DiagnosticStatus::Unrecognized => {
            eprintln!("  {}", rospm_core::msg!("build_summary.unrecognized"));
        }
        DiagnosticStatus::Unavailable => {
            eprintln!("  {}", rospm_core::msg!("build_summary.unavailable"));
        }
        DiagnosticStatus::Parsed => {}
    }
    let mut remaining = 8;
    for group in summary.groups.iter().take(8) {
        eprintln!(
            "  [{} / {}]",
            group.workspace.as_deref().unwrap_or("-"),
            group.package.as_deref().unwrap_or("-")
        );
        for d in &group.diagnostics {
            if remaining == 0 {
                break;
            }
            remaining -= 1;
            let mut location = String::new();
            if let Some(l) = &d.location {
                location = l.file.display().to_string();
                if let Some(n) = l.line {
                    location.push_str(&format!(":{n}"));
                }
                if let Some(n) = l.column {
                    location.push_str(&format!(":{n}"));
                }
                location.push_str(": ");
            }
            eprintln!("    {location}{}", d.explanation);
            if let Some(s) = d.suggestions.first() {
                eprintln!("      {s}");
            }
        }
        if !group.consequences.is_empty() {
            eprintln!(
                "    {}",
                rospm_core::msg!(
                    "build_summary.consequences",
                    count = group.consequences.len()
                )
            );
        }
    }
    eprintln!(
        "  {}",
        rospm_core::msg!("build_summary.log", path = summary.log.display())
    );
}

/// Keep stdout exclusively for the final JSON document. Redirecting the fd also
/// covers inherited child output and existing preparation/progress messages.
pub struct JsonOutput(std::os::fd::OwnedFd);
impl JsonOutput {
    pub fn redirect() -> std::io::Result<Self> {
        use std::io::Write;
        use std::os::fd::{AsFd, AsRawFd};
        std::io::stdout().flush()?;
        let saved = std::io::stdout().as_fd().try_clone_to_owned()?;
        // SAFETY: both descriptors are live standard streams.
        if unsafe { libc::dup2(std::io::stderr().as_raw_fd(), 1) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self(saved))
    }
}
impl Drop for JsonOutput {
    fn drop(&mut self) {
        use std::{io::Write, os::fd::AsRawFd};
        let _ = std::io::stdout().flush();
        // SAFETY: the saved descriptor is owned by this guard and remains live.
        unsafe {
            libc::dup2(self.0.as_raw_fd(), 1);
        }
    }
}
