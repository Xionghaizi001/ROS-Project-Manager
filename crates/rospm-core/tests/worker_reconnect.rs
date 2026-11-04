use rospm_core::worker::*;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread::{self, JoinHandle};
use std::time::Duration;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "rospm-reconnect-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn store(&self) -> TaskStore {
        TaskStore::open(self.0.join("tasks")).unwrap()
    }
    fn server(&self, calls: Arc<AtomicUsize>, fail: bool) -> Server {
        let endpoint = SocketEndpoint::bind(self.0.join("runtime")).unwrap();
        let path = endpoint.path.clone();
        let store = self.store();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let handle = thread::spawn(move || {
            endpoint
                .serve_tasks(&stopping, &store, |_, message| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    if fail {
                        return Err("queue closed".into());
                    }
                    match message {
                        Message::Submit { task_id, .. } => {
                            let mut snapshot = store.load_snapshot(&task_id).unwrap();
                            snapshot.state = TaskState::WaitingConfirmation;
                            snapshot.step = 2;
                            store.write_snapshot(&snapshot).unwrap();
                        }
                        Message::Confirm { task_id, .. } | Message::Cancel { task_id } => {
                            let mut snapshot = store.load_snapshot(&task_id).unwrap();
                            snapshot.state = TaskState::Cancelled;
                            store.write_snapshot(&snapshot).unwrap();
                        }
                        _ => panic!("queries must not reach task runner"),
                    }
                    Ok(())
                })
                .unwrap();
        });
        Server {
            path,
            stop,
            handle: Some(handle),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
struct Server {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}
impl Server {
    fn connect(&self) -> BufReader<UnixStream> {
        let stream = UnixStream::connect(&self.path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut client = BufReader::new(stream);
        assert!(matches!(
            exchange(
                &mut client,
                Message::Hello {
                    client: "test".into()
                }
            ),
            Message::Hello { .. }
        ));
        client
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.handle.take().unwrap().join().unwrap();
    }
}
fn send(client: &mut BufReader<UnixStream>, message: Message) {
    client
        .get_mut()
        .write_all(&Frame::new(message).encode().unwrap())
        .unwrap();
}
fn exchange(client: &mut BufReader<UnixStream>, message: Message) -> Message {
    send(client, message);
    let mut bytes = Vec::new();
    assert!(client.read_until(b'\n', &mut bytes).unwrap() > 0);
    assert!(bytes.len() <= MAX_FRAME_BYTES);
    Frame::decode(&bytes).unwrap().message
}
fn submit() -> Message {
    Message::Submit {
        request_id: "r1".into(),
        task_id: "t1".into(),
        workspace: "/workspace".into(),
        operation: Operation::Check,
    }
}
fn register(store: &TaskStore) -> Result<SubmissionStatus, StoreError> {
    store.register_submission("r1", "t1", "/workspace", &Operation::Check)
}

#[test]
fn lost_ack_reconnect_and_restart_never_dispatch_twice() {
    let fixture = Fixture::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = fixture.server(calls.clone(), false);
    let mut lost = server.connect();
    send(&mut lost, submit());
    drop(lost); // Deliberately never read Accepted.
    let mut client = server.connect();
    assert!(matches!(
        exchange(&mut client, submit()),
        Message::Accepted { .. }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        matches!(exchange(&mut client, Message::Snapshot { task_id: "t1".into() }), Message::State { snapshot } if snapshot.state == TaskState::WaitingConfirmation)
    );
    assert!(
        matches!(exchange(&mut client, Message::Confirm { task_id: "t1".into(), step: 1, response: ConfirmationResponse::Approve }), Message::Error { code, .. } if code == "confirmation_mismatch")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(client);
    drop(server);
    let store = fixture.store();
    store
        .recover_active_tasks(RecoveryCause::WorkerExit)
        .unwrap();
    let restarted = fixture.server(calls.clone(), false);
    let mut client = restarted.connect();
    assert!(matches!(
        exchange(&mut client, submit()),
        Message::Accepted { .. }
    ));
    assert!(
        matches!(exchange(&mut client, Message::Snapshot { task_id: "t1".into() }), Message::State { snapshot } if snapshot.state == TaskState::Interrupted)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(store.prune_completed(0).unwrap(), 1);
    assert!(
        matches!(exchange(&mut client, submit()), Message::Error { code, .. } if code == "task_unavailable")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn concurrent_reservations_bind_full_payload() {
    let fixture = Fixture::new();
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let store = fixture.store();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                register(&store).unwrap()
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == SubmissionStatus::New)
            .count(),
        1
    );
    let store = fixture.store();
    for (request, task, workspace, operation) in [
        ("r1", "t2", "/workspace", Operation::Check),
        ("r1", "t1", "/other", Operation::Check),
        ("r1", "t1", "/workspace", Operation::Build),
        ("r2", "t1", "/workspace", Operation::Check),
    ] {
        assert!(matches!(
            store.register_submission(request, task, workspace, &operation),
            Err(StoreError::SubmissionConflict)
        ));
    }
    assert_eq!(
        register(&fixture.store()).unwrap(),
        SubmissionStatus::Existing
    );
    for id in [".", "..", "../escape", ""] {
        assert!(store
            .register_submission(id, "new", "/workspace", &Operation::Check)
            .is_err());
        assert!(store
            .register_submission("new", id, "/workspace", &Operation::Check)
            .is_err());
    }
}

#[test]
fn reconnect_reads_bounded_logs_and_answers_waiting_step() {
    let fixture = Fixture::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = fixture.server(calls.clone(), false);
    let mut client = server.connect();
    exchange(&mut client, submit());
    drop(client);
    let store = fixture.store();
    for cursor in 0..3 {
        store
            .append_log(
                "t1",
                &LogEntry {
                    cursor,
                    stream: LogStream::Stdout,
                    text: "\"".repeat(200_000),
                },
            )
            .unwrap();
    }
    let mut client = server.connect();
    let page = exchange(
        &mut client,
        Message::Logs {
            task_id: "t1".into(),
            cursor: 0,
            limit: u32::MAX,
        },
    );
    assert!(
        matches!(page, Message::LogChunk { next_cursor: 2, complete: false, entries, .. } if entries.len() == 2)
    );
    assert!(
        matches!(exchange(&mut client, Message::Confirm { task_id: "t1".into(), step: 2, response: ConfirmationResponse::Cancel }), Message::State { snapshot } if snapshot.state == TaskState::Cancelled)
    );
    assert!(
        matches!(exchange(&mut client, Message::Logs { task_id: "t1".into(), cursor: 2, limit: 1 }), Message::LogChunk { next_cursor: 3, complete: true, entries, .. } if entries.len() == 1 && entries[0].cursor == 2)
    );
    assert!(
        matches!(exchange(&mut client, Message::Logs { task_id: "t1".into(), cursor: 3, limit: 1 }), Message::LogChunk { next_cursor: 3, complete: true, entries, .. } if entries.is_empty())
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn failed_dispatch_and_missing_snapshot_are_never_replayed() {
    let fixture = Fixture::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = fixture.server(calls.clone(), true);
    let mut client = server.connect();
    assert!(
        matches!(exchange(&mut client, submit()), Message::Error { code, .. } if code == "dispatch_failed")
    );
    assert!(matches!(
        exchange(&mut client, submit()),
        Message::Accepted { .. }
    ));
    assert!(
        matches!(exchange(&mut client, Message::Snapshot { task_id: "t1".into() }), Message::State { snapshot } if snapshot.state == TaskState::Failed)
    );
    fs::remove_dir_all(fixture.0.join("tasks/t1")).unwrap();
    assert!(
        matches!(exchange(&mut client, submit()), Message::Error { code, .. } if code == "task_unavailable")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    fs::write(fixture.0.join("tasks/@submissions.json"), b"partial").unwrap();
    assert!(
        matches!(exchange(&mut client, submit()), Message::Error { code, .. } if code == "store_error")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn process_exit_preserves_reservation_without_replaying() {
    const CHILD_ROOT: &str = "ROSPM_SUBMISSION_CRASH_TEST_ROOT";
    if let Some(path) = std::env::var_os(CHILD_ROOT) {
        let store = TaskStore::open(PathBuf::from(path).join("tasks")).unwrap();
        assert_eq!(register(&store).unwrap(), SubmissionStatus::New);
        // Exit without destructors immediately after the reservation commit.
        std::process::exit(71);
    }
    let fixture = Fixture::new();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "process_exit_preserves_reservation_without_replaying",
        ])
        .env(CHILD_ROOT, &fixture.0)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(71));
    let store = fixture.store();
    assert_eq!(register(&store).unwrap(), SubmissionStatus::Existing);
    let recovered = store
        .recover_active_tasks(RecoveryCause::WorkerExit)
        .unwrap();
    assert_eq!(recovered[0].state, TaskState::Interrupted);
    // An interrupted ledger write before rename never erases committed reservations.
    fs::write(fixture.0.join("tasks/@submissions.tmp"), b"partial").unwrap();
    assert_eq!(
        store
            .register_submission("r2", "t2", "/workspace", &Operation::Check)
            .unwrap(),
        SubmissionStatus::New
    );
    assert_eq!(register(&store).unwrap(), SubmissionStatus::Existing);
}

#[test]
fn reconnect_never_reports_truncated_or_invalid_logs_complete() {
    let fixture = Fixture::new();
    let store = fixture.store();
    register(&store).unwrap();
    let path = fixture.0.join("tasks/t1/logs.jsonl");
    fs::write(&path, b"{\"cursor\":0").unwrap();
    assert!(
        matches!(store.log_chunk("t1", 0, 10).unwrap(), Message::LogChunk { complete: false, next_cursor: 0, entries, .. } if entries.is_empty())
    );
    let mut snapshot = store.load_snapshot("t1").unwrap();
    snapshot.state = TaskState::Succeeded;
    store.write_snapshot(&snapshot).unwrap();
    assert!(store.log_chunk("t1", 0, 10).is_err());
    fs::write(&path, vec![b'x'; MAX_FRAME_BYTES + 1]).unwrap();
    assert!(store.log_chunk("t1", 0, 10).is_err());
    fs::write(&path, b"").unwrap();
    for cursor in [1, 0] {
        store
            .append_log(
                "t1",
                &LogEntry {
                    cursor,
                    stream: LogStream::Stdout,
                    text: "out of order".into(),
                },
            )
            .unwrap();
    }
    assert!(store.log_chunk("t1", 0, 10).is_err());
}
