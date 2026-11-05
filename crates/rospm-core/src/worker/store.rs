//! Durable task snapshots and bounded log delivery.

use super::*;
use std::fs::File;

const MAX_LOG_LINE_BYTES: usize = MAX_FRAME_BYTES - 1024;

/// Durable task state owned by one worker. Each task gets its own directory;
/// snapshot writes are atomic and logs are append-only JSONL with monotonic
/// cursors, so reconnecting clients can resume without replaying old entries.
#[derive(Clone)]
pub struct TaskStore {
    pub(super) root: PathBuf,
}

/// Why a worker is recovering durable snapshots. A worker process exit gives
/// us a bounded conclusion; after a host failure the child may have run after
/// the last persisted snapshot, so the result must remain explicitly unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryCause {
    WorkerExit,
    HostFailure,
}

/// Captures one task's child output. Disk persistence is performed before the
/// best-effort UI enqueue, so a slow or disconnected client cannot stall the
/// child reader. The UI queue is bounded by both entry count and bytes.
pub struct LogPump {
    store: TaskStore,
    task_id: String,
    next_cursor: Arc<std::sync::atomic::AtomicU64>,
    record_lock: Arc<Mutex<()>>,
    queue: Arc<Mutex<UiQueue>>,
}

struct UiQueue {
    entries: VecDeque<LogEntry>,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
    dropped: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DispatchStatus {
    pub queued: bool,
    pub dropped_before: u64,
}

impl LogPump {
    pub fn new(
        store: TaskStore,
        task_id: impl Into<String>,
        max_entries: usize,
        max_bytes: usize,
    ) -> Self {
        Self {
            store,
            task_id: task_id.into(),
            next_cursor: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            record_lock: Arc::new(Mutex::new(())),
            queue: Arc::new(Mutex::new(UiQueue {
                entries: VecDeque::new(),
                bytes: 0,
                max_entries,
                max_bytes,
                dropped: 0,
            })),
        }
    }

    pub fn record(
        &self,
        stream: LogStream,
        text: impl Into<String>,
    ) -> Result<DispatchStatus, StoreError> {
        // Keep cursor allocation and the corresponding durable append together.
        // Cloned readers share this lock; TaskStore also serializes processes.
        let _record_lock = self.record_lock.lock().expect("log record lock poisoned");
        let entry = LogEntry {
            cursor: self
                .next_cursor
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            stream,
            text: text.into(),
        };
        self.store.append_log(&self.task_id, &entry)?;
        let size = entry.text.len();
        let mut queue = self.queue.lock().expect("log queue poisoned");
        let dropped_before = queue.dropped;
        if queue.max_entries == 0 || size > queue.max_bytes {
            queue.dropped += 1;
            return Ok(DispatchStatus {
                queued: false,
                dropped_before,
            });
        }
        while queue.entries.len() >= queue.max_entries || queue.bytes + size > queue.max_bytes {
            if let Some(old) = queue.entries.pop_front() {
                queue.bytes = queue.bytes.saturating_sub(old.text.len());
                queue.dropped += 1;
            } else {
                break;
            }
        }
        queue.bytes += size;
        queue.entries.push_back(entry);
        Ok(DispatchStatus {
            queued: true,
            dropped_before,
        })
    }

    pub fn drain(&self, max_entries: usize) -> Vec<LogEntry> {
        let mut queue = self.queue.lock().expect("log queue poisoned");
        let count = max_entries.min(queue.entries.len());
        let mut result = Vec::with_capacity(count);
        for _ in 0..count {
            if let Some(entry) = queue.entries.pop_front() {
                queue.bytes = queue.bytes.saturating_sub(entry.text.len());
                result.push(entry);
            }
        }
        result
    }

    pub fn dropped_count(&self) -> u64 {
        self.queue.lock().expect("log queue poisoned").dropped
    }

    pub fn spawn_reader<R: Read + Send + 'static>(
        &self,
        reader: R,
        stream: LogStream,
    ) -> JoinHandle<Result<(), StoreError>> {
        let pump = self.clone();
        thread::spawn(move || {
            read_log_lines(reader, |line| pump.record(stream.clone(), line).map(|_| ()))
        })
    }
}

impl Clone for LogPump {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            task_id: self.task_id.clone(),
            next_cursor: Arc::clone(&self.next_cursor),
            record_lock: Arc::clone(&self.record_lock),
            queue: Arc::clone(&self.queue),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("task store I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("task store JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid task identifier")]
    InvalidTask,
    #[error("task already exists")]
    AlreadyExists,
    #[error("task does not exist")]
    MissingTask,
    #[error("request or task ID is already bound to another submission")]
    SubmissionConflict,
    #[error("invalid submission")]
    InvalidSubmission,
    #[error("log line exceeds the {0}-byte limit")]
    LogLineTooLarge(usize),
}

fn read_log_lines<R: Read>(
    mut reader: R,
    mut on_line: impl FnMut(String) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let mut chunk = [0u8; 8192];
    let mut line = Vec::new();
    loop {
        let count = reader.read(&mut chunk)?;
        if count == 0 {
            if !line.is_empty() {
                on_line(String::from_utf8_lossy(&line).into_owned())?;
            }
            return Ok(());
        }
        for byte in &chunk[..count] {
            if *byte == b'\n' {
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                on_line(String::from_utf8_lossy(&line).into_owned())?;
                line.clear();
            } else {
                if line.len() >= MAX_LOG_LINE_BYTES {
                    return Err(StoreError::LogLineTooLarge(MAX_LOG_LINE_BYTES));
                }
                line.push(*byte);
            }
        }
    }
}

struct TaskFileLock(File);

impl Drop for TaskFileLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl TaskStore {
    fn lock_task(&self, task_id: &str) -> Result<TaskFileLock, StoreError> {
        let dir = self.task_dir(task_id)?;
        if !dir.is_dir() {
            return Err(StoreError::MissingTask);
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(dir.join(".io.lock"))?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(TaskFileLock(file))
    }

    /// Creates a private store at `root`. The caller should place this below
    /// a user-private persistent state directory (not ephemeral socket storage).
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        if fs::symlink_metadata(&root)?.file_type().is_symlink() {
            return Err(StoreError::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "task store cannot be a symlink",
            )));
        }
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let meta = fs::metadata(&root)?;
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err(StoreError::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "task store must be private and owned by current user",
            )));
        }
        Ok(Self { root })
    }

    fn task_dir(&self, task_id: &str) -> Result<PathBuf, StoreError> {
        if !valid_identifier(task_id) {
            return Err(StoreError::InvalidTask);
        }
        Ok(self.root.join(task_id))
    }

    fn files(&self, task_id: &str) -> Result<(PathBuf, PathBuf), StoreError> {
        let dir = self.task_dir(task_id)?;
        Ok((dir.join("snapshot.json"), dir.join("logs.jsonl")))
    }

    pub fn create_task(&self, snapshot: &TaskSnapshot) -> Result<(), StoreError> {
        let dir = self.task_dir(&snapshot.task_id)?;
        if dir.exists() || fs::symlink_metadata(&dir).is_ok() {
            return Err(StoreError::AlreadyExists);
        }
        fs::create_dir(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        let (_, logs) = self.files(&snapshot.task_id)?;
        fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&logs)?;
        self.write_snapshot(snapshot)
    }

    pub fn load_snapshot(&self, task_id: &str) -> Result<TaskSnapshot, StoreError> {
        let (snapshot, _) = self.files(task_id)?;
        if !snapshot.is_file() {
            return Err(StoreError::MissingTask);
        }
        Ok(serde_json::from_slice(&fs::read(snapshot)?)?)
    }

    /// Resolve active tasks left by a previous worker instance. This only
    /// changes durable state; it never starts a command or replays a plan.
    /// UI and SSH disconnects must not call this method: their tasks continue
    /// under the worker and remain reconnectable through the same snapshot.
    pub fn recover_active_tasks(
        &self,
        cause: RecoveryCause,
    ) -> Result<Vec<TaskSnapshot>, StoreError> {
        let mut recovered = Vec::new();
        for item in fs::read_dir(&self.root)? {
            let item = item?;
            if !item.file_type()?.is_dir() {
                continue;
            }
            let id = item.file_name().to_string_lossy().into_owned();
            let mut snapshot = match self.load_snapshot(&id) {
                Ok(snapshot) => snapshot,
                Err(StoreError::MissingTask) => continue,
                Err(error) => return Err(error),
            };
            if !matches!(
                snapshot.state,
                TaskState::Pending
                    | TaskState::Running
                    | TaskState::WaitingConfirmation
                    | TaskState::CancelRequested
            ) {
                continue;
            }
            snapshot.state = match cause {
                RecoveryCause::WorkerExit => TaskState::Interrupted,
                RecoveryCause::HostFailure => TaskState::Unknown,
            };
            snapshot.result = Some(TaskResult {
                success: false,
                exit_code: None,
                error: Some(match cause {
                    RecoveryCause::WorkerExit => {
                        "worker exited while the task was active; inspect logs before retrying"
                            .into()
                    }
                    RecoveryCause::HostFailure => {
                        "host state is unknown after failure; verify the system before retrying"
                            .into()
                    }
                }),
            });
            self.write_snapshot(&snapshot)?;
            recovered.push(snapshot);
        }
        Ok(recovered)
    }

    pub fn write_snapshot(&self, snapshot: &TaskSnapshot) -> Result<(), StoreError> {
        let (path, _) = self.files(&snapshot.task_id)?;
        if !path.parent().map(|p| p.is_dir()).unwrap_or(false) {
            return Err(StoreError::MissingTask);
        }
        let _lock = self.lock_task(&snapshot.task_id)?;
        let tmp = path.with_extension("json.tmp");
        // A previous worker may have died after syncing the temp file. The lock
        // proves no writer is active, so that stale file is safe to replace.
        let _ = fs::remove_file(&tmp);
        let bytes = serde_json::to_vec_pretty(snapshot)?;
        {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
        }
        fs::rename(tmp, path)?;
        Ok(())
    }

    pub fn append_log(&self, task_id: &str, entry: &LogEntry) -> Result<(), StoreError> {
        let (_, path) = self.files(task_id)?;
        if !path.is_file() {
            return Err(StoreError::MissingTask);
        }
        let _lock = self.lock_task(task_id)?;
        let mut record = serde_json::to_vec(entry)?;
        if record.len() > MAX_LOG_LINE_BYTES {
            return Err(StoreError::LogLineTooLarge(MAX_LOG_LINE_BYTES));
        }
        record.push(b'\n');
        let mut file = fs::OpenOptions::new().append(true).open(path)?;
        file.write_all(&record)?;
        file.sync_data()?;
        Ok(())
    }

    pub fn read_logs(
        &self,
        task_id: &str,
        cursor: u64,
        limit: u32,
    ) -> Result<Vec<LogEntry>, StoreError> {
        let (_, path) = self.files(task_id)?;
        if !path.is_file() {
            return Err(StoreError::MissingTask);
        }
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut entries = Vec::new();
        read_log_lines(fs::File::open(path)?, |line| {
            if entries.len() < limit as usize {
                let entry: LogEntry = serde_json::from_str(&line)?;
                if entry.cursor >= cursor {
                    entries.push(entry);
                }
            }
            Ok(())
        })?;
        Ok(entries)
    }

    /// Reconnect pagination is bounded by the encoded wire size as well as count.
    pub fn log_chunk(&self, task_id: &str, cursor: u64, limit: u32) -> Result<Message, StoreError> {
        if limit == 0 {
            return Err(StoreError::InvalidSubmission);
        }
        let terminal = self.load_snapshot(task_id)?.state.is_terminal();
        let (_, path) = self.files(task_id)?;
        let mut reader = BufReader::new(fs::File::open(path)?);
        let mut entries = Vec::new();
        let mut next_cursor = cursor;
        let mut previous = None;
        let mut complete = terminal;
        // Leave space for the task ID, envelope, cursor and JSON separators.
        let mut remaining = MAX_FRAME_BYTES - 1024;
        loop {
            let mut line = Vec::new();
            let length = reader
                .by_ref()
                .take((MAX_FRAME_BYTES + 1) as u64)
                .read_until(b'\n', &mut line)?;
            if length == 0 {
                break;
            }
            if length > MAX_FRAME_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "stored log entry exceeds frame limit",
                )
                .into());
            }
            if !line.ends_with(b"\n") {
                if terminal {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "incomplete final log entry",
                    )
                    .into());
                }
                complete = false;
                break;
            }
            let entry: LogEntry = serde_json::from_slice(&line)?;
            if previous.is_some_and(|previous| entry.cursor <= previous) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "log cursors are not increasing",
                )
                .into());
            }
            previous = Some(entry.cursor);
            if entry.cursor < cursor {
                continue;
            }
            let size = serde_json::to_vec(&entry)?.len() + 1;
            if entries.len() >= limit.min(256) as usize || size > remaining {
                if entries.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "stored log entry exceeds response budget",
                    )
                    .into());
                }
                complete = false;
                break;
            }
            next_cursor = entry
                .cursor
                .checked_add(1)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "log cursor overflow"))?;
            remaining -= size;
            entries.push(entry);
        }
        Ok(Message::LogChunk {
            task_id: task_id.into(),
            next_cursor,
            complete,
            entries,
        })
    }

    /// Keep at most `max_completed` completed task directories. Active tasks
    /// are never removed; callers should invoke this after worker startup and
    /// task completion. A task's snapshot remains readable until pruned.
    pub fn prune_completed(&self, max_completed: usize) -> Result<usize, StoreError> {
        let mut completed = Vec::new();
        for item in fs::read_dir(&self.root)? {
            let item = item?;
            if !item.file_type()?.is_dir() {
                continue;
            }
            let id = item.file_name().to_string_lossy().into_owned();
            if let Ok(snapshot) = self.load_snapshot(&id) {
                if snapshot.state.is_terminal() {
                    let modified = item
                        .metadata()?
                        .modified()
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    completed.push((modified, item.path()));
                }
            }
        }
        completed.sort_by_key(|(time, _)| *time);
        let remove_count = completed.len().saturating_sub(max_completed);
        for (_, path) in completed.into_iter().take(remove_count) {
            fs::remove_dir_all(path)?;
        }
        Ok(remove_count)
    }
}
