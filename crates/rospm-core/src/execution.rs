//! Frontend-neutral execution events. Callbacks must return promptly; persistence
//! and slow-client queues belong to the worker, not the plan executor.
use crate::{diagnostic::summary::Summary, Message};
use serde::Serialize;
use std::{io::Write, path::PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stream {
    Stdout,
    Stderr,
    Progress,
}

/// Step indices are one-based positions in Plan::steps (including notes).
/// Log chunks preserve bytes; they need not end on a line or UTF-8 boundary.
#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event<'a> {
    Task {
        task: &'a str,
        state: State,
        total: usize,
        error: Option<Message>,
    },
    Step {
        task: &'a str,
        step: usize,
        state: State,
    },
    Log {
        task: &'a str,
        step: usize,
        stream: Stream,
        bytes: &'a [u8],
    },
    Diagnostics {
        task: &'a str,
        step: usize,
        summary: &'a Summary,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Operation {
    SwitchMirror {
        name: String,
        ros_apt: Option<String>,
        rosdistro: String,
    },
    Uninstall {
        commands: Vec<crate::plan::Command>,
    },
    CleanBuild {
        paths: Vec<PathBuf>,
        retry: Vec<crate::plan::Command>,
    },
}

/// The synchronous response applies only to this borrowed request. A transport
/// must match task, step and the complete operation before returning approval.
#[derive(Debug, Serialize)]
pub struct Confirmation {
    pub task: String,
    /// One-based plan position; zero means preparation before a plan exists.
    pub step: usize,
    pub operation: Operation,
    pub prompt: Message,
    pub explanation: Option<Message>,
    /// None means a frontend without input should simply decline.
    pub unavailable: Option<Message>,
}
impl Confirmation {
    /// Preserve CLI defaults without treating EOF as an empty affirmative line.
    pub fn accepts_answer(&self, answer: Option<&str>) -> bool {
        answer.is_some_and(|answer| match answer.trim() {
            "y" | "Y" | "yes" => true,
            "" => matches!(self.operation, Operation::SwitchMirror { .. }),
            _ => false,
        })
    }
}

pub trait Interaction: Sync {
    /// May be called concurrently by the stdout and stderr readers.
    fn event(&self, event: Event<'_>);
    /// Safe default for a missing/disconnected frontend. Business callers decide
    /// whether a declined request aborts or keeps the existing configuration.
    fn confirm(&self, _request: &Confirmation) -> crate::Result<bool> {
        Ok(false)
    }
}

pub(crate) struct Execution<'a> {
    pub task: &'a str,
    pub step: usize,
    pub interaction: &'a dyn Interaction,
}
impl Execution<'_> {
    pub fn log(&self, stream: Stream, bytes: &[u8]) {
        self.interaction.event(Event::Log {
            task: self.task,
            step: self.step,
            stream,
            bytes,
        });
    }
}

pub(crate) struct Progress<'a, 'b> {
    pub out: &'a mut dyn Write,
    pub execution: Option<&'a Execution<'b>>,
}
impl Write for Progress<'_, '_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let n = self.out.write(bytes)?;
        if let Some(execution) = self.execution {
            execution.log(Stream::Progress, &bytes[..n]);
        }
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}
