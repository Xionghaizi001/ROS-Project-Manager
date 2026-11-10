//! Protocol and local transport primitives for the on-demand worker.
//!
//! The wire format is newline-delimited JSON.  Commands are deliberately
//! operations, rather than shell strings, so a client cannot turn the worker
//! into an arbitrary (or root) command runner.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

mod coordination;
mod service;
mod socket;
mod store;
mod submission;
mod task;

pub use coordination::{resolve_workspace, ConfirmationWait, WorkspaceLock, WorkspaceLockError};
pub use socket::{PeerIdentity, SocketEndpoint};
pub use store::{DispatchStatus, LogPump, RecoveryCause, StoreError, TaskStore};
pub use submission::SubmissionStatus;
pub use task::{TaskHandle, TaskRunner};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Frame {
    pub version: u16,
    #[serde(flatten)]
    pub message: Message,
}

impl Frame {
    pub fn new(message: Message) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            message,
        }
    }
    pub fn encode(&self) -> Result<Vec<u8>, serde_json::Error> {
        let mut bytes = serde_json::to_vec(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(ProtocolError::FrameTooLarge);
        }
        let frame: Self = serde_json::from_slice(bytes).map_err(ProtocolError::Json)?;
        if frame.version != PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedVersion(frame.version));
        }
        Ok(frame)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Hello {
        client: String,
    },
    Submit {
        request_id: String,
        task_id: String,
        workspace: String,
        operation: Operation,
    },
    Snapshot {
        task_id: String,
    },
    Logs {
        task_id: String,
        cursor: u64,
        limit: u32,
    },
    Confirm {
        task_id: String,
        step: u32,
        response: ConfirmationResponse,
    },
    Cancel {
        task_id: String,
    },
    Accepted {
        request_id: String,
        task_id: String,
    },
    State {
        snapshot: TaskSnapshot,
    },
    LogChunk {
        task_id: String,
        next_cursor: u64,
        complete: bool,
        entries: Vec<LogEntry>,
    },
    Error {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Apply,
    ApplyLocked,
    Build,
    Uninstall,
    Check,
    Verify,
    Deps,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Pending,
    Running,
    WaitingConfirmation,
    CancelRequested,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
    Unknown,
}

impl TaskState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Interrupted | Self::Unknown
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationResponse {
    Approve,
    Reject,
    Cancel,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskSnapshot {
    pub task_id: String,
    pub state: TaskState,
    pub step: u32,
    pub total_steps: Option<u32>,
    /// Exclusive end of durable log records: last persisted cursor + 1, or zero.
    pub log_cursor: u64,
    pub result: Option<TaskResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskResult {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogEntry {
    pub cursor: u64,
    pub stream: LogStream,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    Stdout,
    Stderr,
    Worker,
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("protocol JSON error: {0}")]
    Json(serde_json::Error),
    #[error("unsupported protocol version {0}")]
    UnsupportedVersion(u16),
    #[error("protocol frame is too large")]
    FrameTooLarge,
    #[error("invalid task or request identifier")]
    InvalidIdentifier,
    #[error("socket peer is not the current user")]
    UnauthorizedPeer,
}

pub fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && !matches!(value, "." | "..")
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip_and_version_check() {
        let frame = Frame::new(Message::Logs {
            task_id: "task-1".into(),
            cursor: 3,
            limit: 10,
        });
        let decoded = Frame::decode(&frame.encode().unwrap()).unwrap();
        assert_eq!(decoded, frame);
        let bad = br#"{"version":99,"type":"hello","client":"t"}"#;
        assert!(matches!(
            Frame::decode(bad),
            Err(ProtocolError::UnsupportedVersion(99))
        ));
    }

    #[test]
    fn task_store_persists_snapshot_and_cursor_logs() {
        let root = std::env::temp_dir().join(format!("rospm-task-store-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = TaskStore::open(&root).unwrap();
        let snapshot = TaskSnapshot {
            task_id: "task-1".into(),
            state: TaskState::Running,
            step: 1,
            total_steps: Some(2),
            log_cursor: 0,
            result: None,
        };
        store.create_task(&snapshot).unwrap();
        store
            .append_log(
                "task-1",
                &LogEntry {
                    cursor: 0,
                    stream: LogStream::Stdout,
                    text: "one".into(),
                },
            )
            .unwrap();
        store
            .append_log(
                "task-1",
                &LogEntry {
                    cursor: 1,
                    stream: LogStream::Stderr,
                    text: "two".into(),
                },
            )
            .unwrap();
        assert_eq!(store.read_logs("task-1", 1, 10).unwrap().len(), 1);
        assert_eq!(store.load_snapshot("task-1").unwrap().log_cursor, 2);
        store
            .write_snapshot(&TaskSnapshot {
                state: TaskState::Succeeded,
                result: Some(TaskResult {
                    success: true,
                    exit_code: Some(0),
                    error: None,
                }),
                ..snapshot
            })
            .unwrap();
        assert_eq!(
            store.load_snapshot("task-1").unwrap().state,
            TaskState::Succeeded
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recovery_marks_worker_exit_interrupted_without_replaying() {
        let root = std::env::temp_dir().join(format!("rospm-recovery-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = TaskStore::open(&root).unwrap();
        store
            .create_task(&TaskSnapshot {
                task_id: "active".into(),
                state: TaskState::Running,
                step: 2,
                total_steps: Some(4),
                log_cursor: 0,
                result: None,
            })
            .unwrap();
        store
            .create_task(&TaskSnapshot {
                task_id: "done".into(),
                state: TaskState::Succeeded,
                step: 1,
                total_steps: Some(1),
                log_cursor: 0,
                result: Some(TaskResult {
                    success: true,
                    exit_code: Some(0),
                    error: None,
                }),
            })
            .unwrap();
        let recovered = store
            .recover_active_tasks(RecoveryCause::WorkerExit)
            .unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].state, TaskState::Interrupted);
        assert!(store.load_snapshot("done").unwrap().result.unwrap().success);
        assert_eq!(
            store.load_snapshot("active").unwrap().state,
            TaskState::Interrupted
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn host_failure_is_unknown_and_never_auto_replayed() {
        let root = std::env::temp_dir().join(format!("rospm-host-recovery-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = TaskStore::open(&root).unwrap();
        store
            .create_task(&TaskSnapshot {
                task_id: "active".into(),
                state: TaskState::WaitingConfirmation,
                step: 1,
                total_steps: Some(2),
                log_cursor: 0,
                result: None,
            })
            .unwrap();
        let recovered = store
            .recover_active_tasks(RecoveryCause::HostFailure)
            .unwrap();
        assert_eq!(recovered[0].state, TaskState::Unknown);
        assert!(!recovered[0].result.as_ref().unwrap().success);
        assert!(recovered[0]
            .result
            .as_ref()
            .unwrap()
            .error
            .as_deref()
            .unwrap()
            .contains("verify"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn log_pump_persists_when_ui_queue_is_full() {
        let root = std::env::temp_dir().join(format!("rospm-log-pump-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = TaskStore::open(&root).unwrap();
        store
            .create_task(&TaskSnapshot {
                task_id: "task-1".into(),
                state: TaskState::Running,
                step: 0,
                total_steps: None,
                log_cursor: 0,
                result: None,
            })
            .unwrap();
        let pump = LogPump::new(store.clone(), "task-1", 1, 16);
        assert!(pump.record(LogStream::Stdout, "first").unwrap().queued);
        assert!(pump.record(LogStream::Stderr, "second").unwrap().queued);
        assert_eq!(pump.dropped_count(), 1);
        assert_eq!(pump.drain(10).len(), 1);
        assert_eq!(store.read_logs("task-1", 0, 10).unwrap().len(), 2);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reader_returns_disk_errors_instead_of_claiming_complete() {
        let root = std::env::temp_dir().join(format!("rospm-log-reader-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = TaskStore::open(&root).unwrap();
        store
            .create_task(&TaskSnapshot {
                task_id: "task-1".into(),
                state: TaskState::Running,
                step: 0,
                total_steps: None,
                log_cursor: 0,
                result: None,
            })
            .unwrap();
        let pump = LogPump::new(store, "task-1", 4, 1024);
        let handle = pump.spawn_reader(std::io::Cursor::new(b"a\nb\n".to_vec()), LogStream::Stdout);
        handle.join().unwrap().unwrap();
        assert_eq!(pump.drain(4).len(), 2);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reader_bounds_unterminated_lines_and_keeps_non_utf8_output() {
        let root = std::env::temp_dir().join(format!("rospm-log-bound-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = TaskStore::open(&root).unwrap();
        store
            .create_task(&TaskSnapshot {
                task_id: "task-1".into(),
                state: TaskState::Running,
                step: 0,
                total_steps: None,
                log_cursor: 0,
                result: None,
            })
            .unwrap();
        let pump = LogPump::new(store.clone(), "task-1", 4, 1024);
        let too_long = pump.spawn_reader(
            std::io::Cursor::new(vec![b'x'; 8 * 1024 * 1024]),
            LogStream::Stdout,
        );
        assert!(matches!(
            too_long.join().unwrap(),
            Err(StoreError::LogLineTooLarge(_))
        ));
        let utf8 = pump.spawn_reader(std::io::Cursor::new(vec![0xff, b'\n']), LogStream::Stderr);
        utf8.join().unwrap().unwrap();
        let entries = store.read_logs("task-1", 0, 4).unwrap();
        assert_eq!(entries.last().unwrap().text, "�");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_log_records_remain_complete_json_lines() {
        let root =
            std::env::temp_dir().join(format!("rospm-concurrent-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = TaskStore::open(&root).unwrap();
        store
            .create_task(&TaskSnapshot {
                task_id: "task-1".into(),
                state: TaskState::Running,
                step: 0,
                total_steps: None,
                log_cursor: 0,
                result: None,
            })
            .unwrap();
        let pump = Arc::new(LogPump::new(store.clone(), "task-1", 256, 1 << 20));
        let mut handles = Vec::new();
        for stream in [LogStream::Stdout, LogStream::Stderr] {
            let pump = Arc::clone(&pump);
            handles.push(thread::spawn(move || {
                for index in 0..100 {
                    pump.record(stream.clone(), format!("{stream:?}-{index}"))
                        .unwrap();
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        let entries = store.read_logs("task-1", 0, 300).unwrap();
        assert_eq!(entries.len(), 200);
        assert_eq!(store.load_snapshot("task-1").unwrap().log_cursor, 200);
        assert!(entries
            .windows(2)
            .all(|pair| pair[0].cursor < pair[1].cursor));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recovery_replaces_stale_snapshot_temp_file() {
        let root =
            std::env::temp_dir().join(format!("rospm-stale-snapshot-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = TaskStore::open(&root).unwrap();
        store
            .create_task(&TaskSnapshot {
                task_id: "active".into(),
                state: TaskState::Running,
                step: 1,
                total_steps: Some(2),
                log_cursor: 0,
                result: None,
            })
            .unwrap();
        fs::write(root.join("active/snapshot.json.tmp"), b"partial").unwrap();
        let recovered = store
            .recover_active_tasks(RecoveryCause::WorkerExit)
            .unwrap();
        assert_eq!(recovered[0].state, TaskState::Interrupted);
        assert!(!root.join("active/snapshot.json.tmp").exists());
        assert_eq!(
            store.load_snapshot("active").unwrap().state,
            TaskState::Interrupted
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn endpoint_is_private_and_authenticates_same_user() {
        let dir = std::env::temp_dir().join(format!("rospm-worker-{}", std::process::id()));
        let endpoint = SocketEndpoint::bind(&dir).unwrap();
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&endpoint.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let client = UnixStream::connect(&endpoint.path).unwrap();
        let (_server, peer) = endpoint.accept_authenticated().unwrap();
        assert_eq!(peer.uid, unsafe { libc::geteuid() });
        drop(client);
    }
}
