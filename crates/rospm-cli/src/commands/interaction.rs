//! Terminal adapter only: policy and operation scope live in rospm-core.
use rospm_core::{
    execution::{Confirmation, Event, Interaction, Operation},
    Error, Result,
};
use std::io::{self, BufRead, IsTerminal, Write};

pub(super) struct Terminal;
impl Interaction for Terminal {
    fn event(&self, _: Event<'_>) {}
    fn confirm(&self, request: &Confirmation) -> Result<bool> {
        let stdin = io::stdin();
        if !stdin.is_terminal() {
            return match &request.unavailable {
                Some(message) => Err(Error::Host(message.clone())),
                None => Ok(false),
            };
        }
        prompt(request, &mut stdin.lock(), &mut io::stderr().lock())
    }
}

fn prompt(request: &Confirmation, input: &mut dyn BufRead, out: &mut dyn Write) -> Result<bool> {
    (|| -> io::Result<()> {
        if let Some(message) = &request.explanation {
            writeln!(out, "{message}")?;
        }
        match &request.operation {
            Operation::CleanBuild { paths, retry } => {
                for path in paths {
                    writeln!(out, "  {}", path.display())?;
                }
                for command in retry {
                    writeln!(out, "  $ {}", command.render())?;
                }
            }
            Operation::SwitchMirror {
                ros_apt, rosdistro, ..
            } => {
                if let Some(url) = ros_apt {
                    writeln!(out, "  ROS apt: {url}")?;
                }
                writeln!(out, "  rosdistro: {rosdistro}")?;
            }
            // The exact immutable uninstall plan has already been rendered.
            Operation::Uninstall { .. } => {}
        }
        write!(out, "{}", request.prompt)?;
        out.flush()
    })()
    .map_err(|e| Error::io("<stderr>", e))?;
    let mut answer = String::new();
    let read = input
        .read_line(&mut answer)
        .map_err(|e| Error::io("<stdin>", e))?;
    Ok(request.accepts_answer((read > 0).then_some(answer.as_str())))
}

pub(super) fn task_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rospm_core::Message;

    fn request(operation: Operation) -> Confirmation {
        Confirmation {
            task: "test".into(),
            step: 1,
            operation,
            prompt: Message::text("confirm?"),
            explanation: Some(Message::text("whole directories")),
            unavailable: None,
        }
    }

    #[test]
    fn eof_is_not_the_mirror_default_and_cleanup_requires_yes() {
        let mirror = request(Operation::SwitchMirror {
            name: "tuna".into(),
            ros_apt: Some("https://apt".into()),
            rosdistro: "https://rosdistro".into(),
        });
        for (input, expected) in [("", false), ("\n", true), ("no\n", false), ("yes\n", true)] {
            let mut out = Vec::new();
            assert_eq!(
                prompt(&mirror, &mut input.as_bytes(), &mut out).unwrap(),
                expected
            );
            let text = String::from_utf8(out).unwrap();
            assert!(text.contains("https://apt") && text.contains("https://rosdistro"));
        }
        let clean = request(Operation::CleanBuild {
            paths: vec!["/project/ws/build".into()],
            retry: vec![],
        });
        for (input, expected) in [("", false), ("\n", false), ("y\n", true)] {
            let mut out = Vec::new();
            assert_eq!(
                prompt(&clean, &mut input.as_bytes(), &mut out).unwrap(),
                expected
            );
            assert_eq!(
                String::from_utf8(out).unwrap(),
                "whole directories\n  /project/ws/build\nconfirm?"
            );
        }
    }

    #[test]
    fn disconnected_input_or_failed_display_cannot_approve() {
        struct Disconnected;
        impl io::Read for Disconnected {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
        }
        let clean = request(Operation::CleanBuild {
            paths: vec![],
            retry: vec![],
        });
        assert!(prompt(
            &clean,
            &mut io::BufReader::new(Disconnected),
            &mut Vec::new()
        )
        .is_err());
        let full = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        let mut input = &b"yes\n"[..];
        assert!(prompt(&clean, &mut input, &mut &full).is_err());
        assert_eq!(input, b"yes\n");
    }
}
