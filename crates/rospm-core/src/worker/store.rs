//! Durable task snapshots and bounded log delivery.

use super::*;

/// Durable task state owned by one worker. Each task gets its own directory;
/// snapshot writes are atomic and logs are append-only JSONL with monotonic
/// cursors, so reconnecting clients can resume without replaying old entries.
#[derive(Clone)]
pub struct TaskStore {
    root: PathBuf,
}

/// Captures one task's child output. Disk persistence is performed before the
/// best-effort UI enqueue, so a slow or disconnected client cannot stall the
/// child reader. The UI queue is bounded by both entry count and bytes.
pub struct LogPump {
    store: TaskStore,
    task_id: String,
    next_cursor: Arc<std::sync::atomic::AtomicU64>,
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
            for line in BufReader::new(reader).lines() {
                pump.record(stream.clone(), line?)?;
            }
            Ok(())
        })
    }
}

impl Clone for LogPump {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            task_id: self.task_id.clone(),
            next_cursor: Arc::clone(&self.next_cursor),
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
}

impl TaskStore {
    /// Creates a private store at `root`. The caller should place this below
    /// the same user-private runtime directory as the socket.
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

    pub fn write_snapshot(&self, snapshot: &TaskSnapshot) -> Result<(), StoreError> {
        let (path, _) = self.files(&snapshot.task_id)?;
        if !path.parent().map(|p| p.is_dir()).unwrap_or(false) {
            return Err(StoreError::MissingTask);
        }
        let tmp = path.with_extension("json.tmp");
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
        let mut file = fs::OpenOptions::new().append(true).open(path)?;
        serde_json::to_writer(&mut file, entry)?;
        file.write_all(b"\n")?;
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
        for line in BufReader::new(fs::File::open(path)?).lines() {
            let entry: LogEntry = serde_json::from_str(&line?)?;
            if entry.cursor >= cursor {
                entries.push(entry);
                if entries.len() >= limit as usize {
                    break;
                }
            }
        }
        Ok(entries)
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
                if matches!(
                    snapshot.state,
                    TaskState::Succeeded
                        | TaskState::Failed
                        | TaskState::Cancelled
                        | TaskState::Interrupted
                        | TaskState::Unknown
                ) {
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
