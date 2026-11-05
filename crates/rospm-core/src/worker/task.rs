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
    pending: Arc<Mutex<Option<u32>>>,
}

/// Owns one running task. The worker/service dispatch loop keeps this handle
/// and calls `respond` or `cancel` for authenticated client messages.
pub struct TaskHandle {
    control: Control,
    thread: JoinHandle<Result<()>>,
}

impl TaskHandle {
    pub fn respond(&self, response: ConfirmationResponse) {
        let step = *self.control.pending.lock().expect("confirmation poisoned");
        if let Some(step) = step {
            let _ = self.respond_at(step, response);
        }
    }

    /// Accept exactly one response for the currently published confirmation.
    pub fn respond_at(&self, step: u32, response: ConfirmationResponse) -> Result<()> {
        let mut pending = self.control.pending.lock().expect("confirmation poisoned");
        if *pending != Some(step) || self.control.cancelled.load(Ordering::Acquire) {
            return Err(Error::Host(UserMessage::text("confirmation mismatch")));
        }
        *pending = None;
        self.control.confirmation.respond(response);
        Ok(())
    }

    pub fn is_finished(&self) -> bool {
        self.thread.is_finished()
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
        match self.store.load_snapshot(&task_id) {
            Ok(snapshot) if snapshot.state != TaskState::Pending => {
                return Err(Error::Host(UserMessage::text("task is not pending")));
            }
            Ok(_) | Err(StoreError::MissingTask) => {}
            Err(error) => return Err(store_error(error)),
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
        let snapshot = self.store.load_snapshot(&task_id).map_err(store_error)?;
        if snapshot.state != TaskState::Pending {
            return Err(Error::Host(UserMessage::text("task is not pending")));
        }
        let control = Control {
            cancelled: Arc::new(AtomicBool::new(false)),
            confirmation: ConfirmationWait::default(),
            pending: Arc::new(Mutex::new(None)),
        };
        let thread_control = control.clone();
        let store = self.store.clone();
        let runtime = self.runtime_dir.clone();
        let thread = thread::spawn(move || {
            let interaction = WorkerInteraction {
                store: store.clone(),
                task_id: task_id.clone(),
                control: thread_control,
                failure: Mutex::new(None),
                log: LogPump::new(store.clone(), task_id.clone(), 256, MAX_FRAME_BYTES - 1024),
            };
            let mut lease = None;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                loop {
                    if interaction.cancel_requested(&task_id) {
                        return Err(Error::Cancelled);
                    }
                    lease = WorkspaceLock::try_acquire(&runtime, &workspace).map_err(lock_error)?;
                    if lease.is_some() {
                        break;
                    }
                    thread::sleep(std::time::Duration::from_millis(20));
                }
                interaction.set_state(TaskState::Running, 0);
                interaction.check_storage()?;
                let result = run(&interaction);
                interaction.check_storage()?;
                if result.is_ok() && interaction.cancel_requested(&task_id) {
                    return Err(Error::Cancelled);
                }
                result
            }))
            .unwrap_or_else(|_| Err(Error::Host(UserMessage::text("worker task panicked"))));
            let state = match &result {
                Ok(()) => TaskState::Succeeded,
                Err(Error::Cancelled) => TaskState::Cancelled,
                Err(_) => TaskState::Failed,
            };
            let mut snapshot = store.load_snapshot(&task_id).map_err(store_error)?;
            snapshot.state = state;
            snapshot.result = Some(TaskResult {
                success: result.is_ok(),
                exit_code: match &result {
                    Ok(()) => Some(0),
                    _ => None,
                },
                error: result.as_ref().err().map(ToString::to_string),
            });
            store.write_snapshot(&snapshot).map_err(store_error)?;
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
    failure: Mutex<Option<String>>,
}

impl WorkerInteraction {
    fn record_failure(&self, result: std::result::Result<(), StoreError>) {
        if let Err(error) = result {
            *self.failure.lock().unwrap() = Some(error.to_string());
            self.control.cancelled.store(true, Ordering::Release);
            self.control.confirmation.cancel();
        }
    }

    fn check_storage(&self) -> Result<()> {
        match self.failure.lock().unwrap().as_ref() {
            Some(error) => Err(Error::Host(UserMessage::text(error.clone()))),
            None => Ok(()),
        }
    }

    fn set_state(&self, state: TaskState, step: u32) {
        self.record_failure((|| {
            let mut snapshot = self.store.load_snapshot(&self.task_id)?;
            snapshot.state = state;
            snapshot.step = step;
            self.store.write_snapshot(&snapshot)
        })());
    }
}

impl Interaction for WorkerInteraction {
    fn process_groups(&self) -> bool {
        true
    }
    fn event(&self, event: Event<'_>) {
        match event {
            Event::Task { total, .. } => {
                self.record_failure((|| {
                    let mut snapshot = self.store.load_snapshot(&self.task_id)?;
                    snapshot.total_steps = Some(total as u32);
                    self.store.write_snapshot(&snapshot)
                })());
            }
            Event::Interaction { step, state, .. } => {
                // Confirmation publishes its state only after the review is durable.
                if state != State::WaitingConfirmation {
                    self.set_state(TaskState::Running, step as u32);
                }
            }
            Event::Step { step, .. } => self.set_state(TaskState::Running, step as u32),
            Event::Log { stream, bytes, .. } => {
                let stream = match stream {
                    crate::execution::Stream::Stdout => LogStream::Stdout,
                    crate::execution::Stream::Stderr => LogStream::Stderr,
                    crate::execution::Stream::Progress => LogStream::Worker,
                };
                self.record_failure(
                    self.log
                        .record(stream, String::from_utf8_lossy(bytes))
                        .map(|_| ()),
                );
            }
            Event::Diagnostics { .. } => {}
        }
    }

    fn cancel_requested(&self, _: &str) -> bool {
        self.control.cancelled.load(Ordering::Acquire)
    }

    fn confirm(&self, request: &Confirmation) -> Result<bool> {
        let review = serde_json::to_string(request)
            .map_err(|e| Error::Host(UserMessage::text(e.to_string())))?;
        self.log
            .record(LogStream::Worker, review)
            .map_err(store_error)?;
        {
            let mut pending = self.control.pending.lock().unwrap();
            if self.control.cancelled.load(Ordering::Acquire) {
                return Err(Error::Cancelled);
            }
            *pending = Some(request.step as u32);
            self.set_state(TaskState::WaitingConfirmation, request.step as u32);
        }
        self.check_storage()?;
        let response = self.control.confirmation.wait();
        *self.control.pending.lock().unwrap() = None;
        if self.control.cancelled.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        match response {
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
