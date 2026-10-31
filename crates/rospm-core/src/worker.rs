//! Protocol and local transport primitives for the on-demand worker.
//!
//! The wire format is newline-delimited JSON.  Commands are deliberately
//! operations, rather than shell strings, so a client cannot turn the worker
//! into an arbitrary (or root) command runner.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

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
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

/// A private, per-user endpoint. The directory and socket are both checked
/// before use; an existing non-socket path or symlink is rejected.
pub struct SocketEndpoint {
    pub directory: PathBuf,
    pub path: PathBuf,
    pub listener: UnixListener,
}

impl SocketEndpoint {
    pub fn bind(directory: impl AsRef<Path>) -> io::Result<Self> {
        let directory = directory.as_ref().to_path_buf();
        fs::create_dir_all(&directory)?;
        if fs::symlink_metadata(&directory)?.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "worker directory cannot be a symlink",
            ));
        }
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        let meta = fs::metadata(&directory)?;
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "worker directory must be private and owned by current user",
            ));
        }
        let path = directory.join("worker.sock");
        if path.exists() || fs::symlink_metadata(&path).is_ok() {
            let m = fs::symlink_metadata(&path)?;
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                if m.file_type().is_socket() {
                    "worker socket already exists; do not replace a running worker"
                } else {
                    "worker socket path is not a socket"
                },
            ));
        }
        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            directory,
            path,
            listener,
        })
    }

    pub fn accept_authenticated(&self) -> io::Result<(UnixStream, PeerIdentity)> {
        let (stream, _) = self.listener.accept()?;
        let peer = peer_identity(&stream)?;
        if peer.uid != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                ProtocolError::UnauthorizedPeer,
            ));
        }
        Ok((stream, peer))
    }
}

impl Drop for SocketEndpoint {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerIdentity {
    pub pid: i32,
    pub uid: u32,
    pub gid: u32,
}

fn peer_identity(stream: &UnixStream) -> io::Result<PeerIdentity> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PeerIdentity {
        pid: cred.pid,
        uid: cred.uid,
        gid: cred.gid,
    })
}

/// Atomically remembers submitted request IDs. Reconnecting with an existing
/// ID is therefore an acknowledgement lookup, never a second execution.
#[derive(Default)]
pub struct SubmissionRegistry(std::sync::Mutex<std::collections::HashMap<String, String>>);
impl SubmissionRegistry {
    pub fn record(&self, request_id: &str, task_id: &str) -> Result<bool, ProtocolError> {
        if !valid_identifier(request_id) || !valid_identifier(task_id) {
            return Err(ProtocolError::InvalidIdentifier);
        }
        let mut map = self.0.lock().expect("submission registry poisoned");
        Ok(match map.get(request_id) {
            Some(existing) => existing == task_id,
            None => {
                map.insert(request_id.into(), task_id.into());
                true
            }
        })
    }
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
    fn duplicate_request_never_reexecutes() {
        let registry = SubmissionRegistry::default();
        assert!(registry.record("r1", "t1").unwrap());
        assert!(registry.record("r1", "t1").unwrap());
        assert!(!registry.record("r1", "t2").unwrap());
        assert!(registry.record("r2", "t2").unwrap());
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
