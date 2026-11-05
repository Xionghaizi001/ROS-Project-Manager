//! Production task boundary: workspace lease, confirmation rendezvous and
//! cooperative cancellation all live for the complete plan execution.

use super::*;
use crate::execution::{Confirmation, Event, Interaction, State};
use crate::plan::Plan;
use crate::{Error, Message as UserMessage, Result};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone)]
struct Control {
    cancelled: Arc<AtomicBool>,
    confirmation: ConfirmationWait,
}

/// Owns one running task. The worker/service dispatch loop keeps this handle
/// and calls `respond` or `cancel` for authenticated client messages.
pub struct TaskHandle {
    control: Control,
    thread: JoinHandle<Result<()>>,
}

impl TaskHandle {
    pub fn respond(&self, response: ConfirmationResponse) {
        self.control.confirmation.respond(response);
    }

    pub fn cancel(&self) {
        self.control.cancelled.store(true, Ordering::Release);
        self.control.confirmation.cancel();
    }

    pub fn join(self) -> Result<()> {
        self.thread
            .join()
            .unwrap_or_else(|_| Err(Error::Host(UserMessage::text("worker task panicked"))))
    }
}

/// Starts a plan in a worker-owned thread while holding the workspace lease.
/// The lease intentionally spans confirmation waits and every plan step.
#[derive(Clone)]
pub struct TaskRunner {
    store: TaskStore,
    runtime_dir: PathBuf,
}

impl TaskRunner {
    pub fn new(store: TaskStore, runtime_dir: impl Into<PathBuf>) -> Self {
        Self {
            store,
            runtime_dir: runtime_dir.into(),
        }
    }

    pub fn start(
        &self,
        task_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
        plan: Plan,
    ) -> Result<TaskHandle> {
        let task_id = task_id.into();
        let workspace = workspace.into();
        if !valid_identifier(&task_id) {
            return Err(Error::Host(UserMessage::text(
                "invalid worker task identifier",
            )));
        }
        if matches!(
            self.store.load_snapshot(&task_id),
            Err(StoreError::MissingTask)
        ) {
            self.store
                .create_task(&TaskSnapshot {
                    task_id: task_id.clone(),
                    state: TaskState::Pending,
                    step: 0,
                    total_steps: Some(plan.steps.len() as u32),
                    log_cursor: 0,
                    result: None,
                })
                .map_err(store_error)?;
        }
        let task_for_run = task_id.clone();
        self.start_existing(task_id, workspace, move |interaction| {
            plan.perform_with(&task_for_run, interaction)
        })
    }

    /// Start an arbitrary worker-owned operation. The callback receives the
    /// task-scoped interaction, so shared confirmation helpers can wait for a
    /// socket response while the workspace lease remains held.
    pub fn start_with<F>(
        &self,
        task_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
        run: F,
    ) -> Result<TaskHandle>
    where
        F: FnOnce(&dyn Interaction) -> Result<()> + Send + 'static,
    {
        let task_id = task_id.into();
        let workspace = workspace.into();
        if !valid_identifier(&task_id) {
            return Err(Error::Host(UserMessage::text(
                "invalid worker task identifier",
            )));
        }
        if matches!(
            self.store.load_snapshot(&task_id),
            Err(StoreError::MissingTask)
        ) {
            self.store
                .create_task(&TaskSnapshot {
                    task_id: task_id.clone(),
                    state: TaskState::Pending,
                    step: 0,
                    total_steps: None,
                    log_cursor: 0,
                    result: None,
                })
                .map_err(store_error)?;
        }
        self.start_existing(task_id, workspace, run)
    }

    fn start_existing<F>(&self, task_id: String, workspace: PathBuf, run: F) -> Result<TaskHandle>
    where
        F: FnOnce(&dyn Interaction) -> Result<()> + Send + 'static,
    {
        let control = Control {
            cancelled: Arc::new(AtomicBool::new(false)),
            confirmation: ConfirmationWait::default(),
        };
        let thread_control = control.clone();
        let store = self.store.clone();
        let runtime = self.runtime_dir.clone();
        let thread = thread::spawn(move || {
            let lease = WorkspaceLock::acquire(&runtime, &workspace).map_err(lock_error)?;
            let interaction = WorkerInteraction {
                store: store.clone(),
                task_id: task_id.clone(),
                control: thread_control,
                log: LogPump::new(store.clone(), task_id.clone(), 256, MAX_FRAME_BYTES - 1024),
            };
            interaction.set_state(TaskState::Running, 0);
            let result = run(&interaction);
            let state = match &result {
                Ok(()) => TaskState::Succeeded,
                Err(Error::Cancelled) => TaskState::Cancelled,
                Err(_) => TaskState::Failed,
            };
            interaction.set_state(state, interaction.step());
            drop(lease);
            result
        });
        Ok(TaskHandle { control, thread })
    }
}

struct WorkerInteraction {
    store: TaskStore,
    task_id: String,
    control: Control,
    log: LogPump,
}

impl WorkerInteraction {
    fn step(&self) -> u32 {
        self.store
            .load_snapshot(&self.task_id)
            .map(|s| s.step)
            .unwrap_or(0)
    }

    fn set_state(&self, state: TaskState, step: u32) {
        if let Ok(mut snapshot) = self.store.load_snapshot(&self.task_id) {
            snapshot.state = state;
            snapshot.step = step;
            let _ = self.store.write_snapshot(&snapshot);
        }
    }
}

impl Interaction for WorkerInteraction {
    fn process_groups(&self) -> bool {
        true
    }
    fn event(&self, event: Event<'_>) {
        match event {
            Event::Task { state, .. } | Event::Interaction { state, .. } => {
                let mapped = match state {
                    State::Running => TaskState::Running,
                    State::WaitingConfirmation => TaskState::WaitingConfirmation,
                    State::CancelRequested => TaskState::CancelRequested,
                    State::Cancelled => TaskState::Cancelled,
                    State::Succeeded => TaskState::Succeeded,
                    State::Failed => TaskState::Failed,
                };
                self.set_state(mapped, self.step());
            }
            Event::Step { step, state, .. } => {
                let mapped = match state {
                    State::Running => TaskState::Running,
                    State::Succeeded => TaskState::Running,
                    State::Failed => TaskState::Failed,
                    State::Cancelled => TaskState::Cancelled,
                    State::WaitingConfirmation | State::CancelRequested => TaskState::Running,
                };
                self.set_state(mapped, step as u32);
            }
            Event::Log { stream, bytes, .. } => {
                let stream = match stream {
                    crate::execution::Stream::Stdout => LogStream::Stdout,
                    crate::execution::Stream::Stderr => LogStream::Stderr,
                    crate::execution::Stream::Progress => LogStream::Worker,
                };
                let _ = self.log.record(stream, String::from_utf8_lossy(bytes));
            }
            Event::Diagnostics { .. } => {}
        }
    }

    fn cancel_requested(&self, _: &str) -> bool {
        self.control.cancelled.load(Ordering::Acquire)
    }

    fn confirm(&self, _: &Confirmation) -> Result<bool> {
        match self.control.confirmation.wait() {
            ConfirmationResponse::Approve => Ok(true),
            ConfirmationResponse::Reject => Ok(false),
            ConfirmationResponse::Cancel => Err(Error::Cancelled),
        }
    }
}

fn store_error(error: StoreError) -> Error {
    Error::Host(UserMessage::text(error.to_string()))
}
fn lock_error(error: WorkspaceLockError) -> Error {
    Error::Host(UserMessage::text(error.to_string()))
}
