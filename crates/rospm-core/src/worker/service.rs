//! Bounded, nonblocking request/reply transport for the worker owner.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const MAX_CLIENTS: usize = 32;

struct Client {
    stream: UnixStream,
    peer: PeerIdentity,
    input: Vec<u8>,
    output: Vec<u8>,
    written: usize,
    hello: bool,
    closing: bool,
}

impl SocketEndpoint {
    /// Serve durable submissions and reconnect queries. The callback only
    /// enqueues a new Submit or handles a live Confirm/Cancel; it must return
    /// promptly and update control state before returning. Duplicate submissions
    /// never reach it; control requests still need task-local validation.
    pub fn serve_tasks(
        &self,
        stop: &AtomicBool,
        store: &TaskStore,
        mut dispatch: impl FnMut(PeerIdentity, Message) -> Result<(), String>,
    ) -> io::Result<()> {
        self.serve(stop, |peer, message| {
            dispatch_persisted(store, peer, message, &mut dispatch)
                .unwrap_or_else(|e| error("store_error", e))
        })
    }

    /// Serve authenticated clients until `stop` is set. Disconnects only drop
    /// transport state: they never cancel tasks or synthesize confirmations.
    ///
    /// `dispatch` must return promptly (enqueue work, never run a task here).
    /// It owns task validation, durable submission deduplication and replies.
    /// Each client retains at most one input frame and one reply; a client
    /// that does not read cannot block another client's requests.
    pub fn serve(
        &self,
        stop: &AtomicBool,
        mut dispatch: impl FnMut(PeerIdentity, Message) -> Message,
    ) -> io::Result<()> {
        self.listener.set_nonblocking(true)?;
        let result = self.serve_loop(stop, &mut dispatch);
        self.listener.set_nonblocking(false)?;
        result
    }

    fn serve_loop(
        &self,
        stop: &AtomicBool,
        dispatch: &mut impl FnMut(PeerIdentity, Message) -> Message,
    ) -> io::Result<()> {
        let mut clients = Vec::new();
        while !stop.load(Ordering::Acquire) {
            // Bound accepts as well as reads so connection floods cannot starve peers.
            for _ in 0..MAX_CLIENTS {
                match self.accept_authenticated() {
                    Ok((stream, peer)) => {
                        if clients.len() < MAX_CLIENTS {
                            stream.set_nonblocking(true)?;
                            clients.push(Client {
                                stream,
                                peer,
                                input: Vec::new(),
                                output: Vec::new(),
                                written: 0,
                                hello: false,
                                closing: false,
                            });
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::PermissionDenied | io::ErrorKind::Interrupted
                        ) =>
                    {
                        continue
                    }
                    Err(e) => return Err(e),
                }
            }
            clients.retain_mut(|client| client.poll(dispatch).unwrap_or(false));
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
}

fn dispatch_persisted(
    store: &TaskStore,
    peer: PeerIdentity,
    message: Message,
    dispatch: &mut impl FnMut(PeerIdentity, Message) -> Result<(), String>,
) -> Result<Message, StoreError> {
    match &message {
        Message::Submit {
            request_id,
            task_id,
            workspace,
            operation,
        } => {
            let status = match store.register_submission(request_id, task_id, workspace, operation)
            {
                Ok(status) => status,
                Err(StoreError::SubmissionConflict | StoreError::AlreadyExists) => {
                    return Ok(error(
                        "submission_conflict",
                        "request or task ID already used",
                    ));
                }
                Err(e) => return Err(e),
            };
            if status == SubmissionStatus::New {
                if let Err(reason) = dispatch(peer, message.clone()) {
                    let mut snapshot = store.load_snapshot(task_id)?;
                    snapshot.state = TaskState::Failed;
                    snapshot.result = Some(TaskResult {
                        success: false,
                        exit_code: None,
                        error: Some(reason.clone()),
                    });
                    store.write_snapshot(&snapshot)?;
                    return Ok(error("dispatch_failed", reason));
                }
            }
            // Pruned or partially committed tasks remain reserved, never relaunched.
            match store.load_snapshot(task_id) {
                Ok(_) => (),
                Err(StoreError::MissingTask) => return Ok(error("task_unavailable", "submission is reserved but its snapshot is unavailable; do not replay automatically")),
                Err(e) => return Err(e),
            }
            Ok(Message::Accepted {
                request_id: request_id.clone(),
                task_id: task_id.clone(),
            })
        }
        Message::Snapshot { task_id } => Ok(Message::State {
            snapshot: store.load_snapshot(task_id)?,
        }),
        Message::Logs {
            task_id,
            cursor,
            limit,
        } => store.log_chunk(task_id, *cursor, *limit),
        Message::Confirm { task_id, step, .. } => {
            let snapshot = store.load_snapshot(task_id)?;
            if snapshot.state != TaskState::WaitingConfirmation || snapshot.step != *step {
                return Ok(error(
                    "confirmation_mismatch",
                    "task is not waiting for this step",
                ));
            }
            if let Err(reason) = dispatch(peer, message.clone()) {
                return Ok(error("dispatch_failed", reason));
            }
            Ok(Message::State {
                snapshot: store.load_snapshot(task_id)?,
            })
        }
        Message::Cancel { task_id } => {
            let snapshot = store.load_snapshot(task_id)?;
            if !snapshot.state.is_terminal() {
                if let Err(reason) = dispatch(peer, message.clone()) {
                    return Ok(error("dispatch_failed", reason));
                }
            }
            Ok(Message::State {
                snapshot: store.load_snapshot(task_id)?,
            })
        }
        _ => Ok(error("invalid_request", "expected a client request")),
    }
}

fn error(code: &str, message: impl ToString) -> Message {
    Message::Error {
        code: code.into(),
        message: message.to_string(),
    }
}

impl Client {
    fn reply(&mut self, message: Message) -> io::Result<()> {
        self.output = Frame::new(message).encode()?;
        if self.output.len() > MAX_FRAME_BYTES {
            self.output =
                Frame::new(error("frame_too_large", "response exceeds frame limit")).encode()?;
        }
        self.written = 0;
        Ok(())
    }

    fn poll(
        &mut self,
        dispatch: &mut impl FnMut(PeerIdentity, Message) -> Message,
    ) -> io::Result<bool> {
        if !self.output.is_empty() {
            match self.stream.write(&self.output[self.written..]) {
                Ok(0) => return Ok(false),
                Ok(n) => self.written += n,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    return Ok(true)
                }
                Err(e) => return Err(e),
            }
            if self.written < self.output.len() {
                return Ok(true);
            }
            self.output.clear();
            self.written = 0;
        }
        if self.closing {
            return Ok(false);
        }
        if !self.input.contains(&b'\n') {
            let mut bytes = [0; 8192];
            let remaining = (MAX_FRAME_BYTES - self.input.len()).min(bytes.len());
            match self.stream.read(&mut bytes[..remaining]) {
                Ok(0) => return Ok(false),
                Ok(n) => self.input.extend_from_slice(&bytes[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    return Ok(true)
                }
                Err(e) => return Err(e),
            }
        }
        if let Some(end) = self.input.iter().position(|b| *b == b'\n') {
            let decoded = Frame::decode(&self.input[..=end]);
            self.input.drain(..=end);
            match decoded {
                Err(e) => {
                    self.closing = true;
                    self.reply(error("protocol_error", e))?;
                }
                Ok(frame) => {
                    let reply = match frame.message {
                        Message::Hello { .. } => {
                            self.hello = true;
                            Message::Hello {
                                client: "rospm-worker".into(),
                            }
                        }
                        _ if !self.hello => error("hello_required", "send hello before requests"),
                        message => match validate_request(&message) {
                            Ok(()) => dispatch(self.peer, message),
                            Err(reason) => error("invalid_request", reason),
                        },
                    };
                    self.reply(reply)?;
                }
            }
        } else if self.input.len() == MAX_FRAME_BYTES {
            self.closing = true;
            self.reply(error("frame_too_large", "request exceeds frame limit"))?;
        }
        Ok(true)
    }
}

fn validate_request(message: &Message) -> Result<(), &'static str> {
    let task_id = match message {
        Message::Submit {
            request_id,
            task_id,
            workspace,
            ..
        } => {
            if !valid_identifier(request_id) || workspace.is_empty() || workspace.contains('\0') {
                return Err("invalid request identifier or workspace");
            }
            task_id
        }
        Message::Snapshot { task_id }
        | Message::Confirm { task_id, .. }
        | Message::Cancel { task_id } => task_id,
        Message::Logs { task_id, limit, .. } => {
            if *limit == 0 {
                return Err("log limit must be positive");
            }
            task_id
        }
        _ => return Err("expected a client request"),
    };
    if !valid_identifier(task_id) {
        return Err("invalid task identifier");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct Server {
        path: PathBuf,
        stop: Arc<AtomicBool>,
        calls: Arc<AtomicUsize>,
        thread: Option<JoinHandle<io::Result<()>>>,
    }

    impl Server {
        fn start() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "rospm-service-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let endpoint = SocketEndpoint::bind(&path).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let calls = Arc::new(AtomicUsize::new(0));
            let (stopping, count) = (stop.clone(), calls.clone());
            let thread = thread::spawn(move || {
                endpoint.serve(&stopping, |_, message| {
                    count.fetch_add(1, Ordering::Relaxed);
                    match message {
                        Message::Logs {
                            task_id, cursor, ..
                        } => Message::LogChunk {
                            task_id,
                            next_cursor: cursor,
                            complete: false,
                            entries: vec![LogEntry {
                                cursor,
                                stream: LogStream::Stdout,
                                text: "x".repeat(MAX_FRAME_BYTES / 2),
                            }],
                        },
                        _ => error("test_reply", "handled"),
                    }
                })
            });
            Self {
                path,
                stop,
                calls,
                thread: Some(thread),
            }
        }

        fn connect(&self) -> BufReader<UnixStream> {
            let stream = UnixStream::connect(self.path.join("worker.sock")).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            BufReader::new(stream)
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            self.thread.take().unwrap().join().unwrap().unwrap();
            fs::remove_dir_all(&self.path).unwrap();
        }
    }

    fn send(client: &mut BufReader<UnixStream>, message: Message) {
        client
            .get_mut()
            .write_all(&Frame::new(message).encode().unwrap())
            .unwrap();
    }

    fn receive(client: &mut BufReader<UnixStream>) -> Message {
        let mut bytes = Vec::new();
        assert!(client.read_until(b'\n', &mut bytes).unwrap() > 0);
        Frame::decode(&bytes).unwrap().message
    }

    fn hello(client: &mut BufReader<UnixStream>) {
        send(
            client,
            Message::Hello {
                client: "test".into(),
            },
        );
        assert!(matches!(receive(client), Message::Hello { .. }));
    }

    #[test]
    fn fragmented_and_coalesced_requests_reconnect_without_implicit_dispatch() {
        let server = Server::start();
        let mut client = server.connect();
        let bytes = Frame::new(Message::Hello {
            client: "test".into(),
        })
        .encode()
        .unwrap();
        client.get_mut().write_all(&bytes[..5]).unwrap();
        client.get_mut().write_all(&bytes[5..]).unwrap();
        assert!(matches!(receive(&mut client), Message::Hello { .. }));
        let requests = [
            Message::Submit {
                request_id: "r1".into(),
                task_id: "t1".into(),
                workspace: "/tmp/ws".into(),
                operation: Operation::Check,
            },
            Message::Snapshot {
                task_id: "t1".into(),
            },
            Message::Confirm {
                task_id: "t1".into(),
                step: 1,
                response: ConfirmationResponse::Reject,
            },
            Message::Cancel {
                task_id: "t1".into(),
            },
        ];
        let bytes: Vec<u8> = requests
            .into_iter()
            .flat_map(|m| Frame::new(m).encode().unwrap())
            .collect();
        client.get_mut().write_all(&bytes).unwrap();
        for _ in 0..4 {
            assert!(
                matches!(receive(&mut client), Message::Error { code, .. } if code == "test_reply")
            );
        }
        drop(client);
        let mut reconnected = server.connect();
        hello(&mut reconnected);
        assert_eq!(server.calls.load(Ordering::Relaxed), 4);
        send(
            &mut reconnected,
            Message::Snapshot {
                task_id: "t1".into(),
            },
        );
        receive(&mut reconnected);
        assert_eq!(server.calls.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn rejects_bad_frames_and_client_responses_without_dispatch() {
        let server = Server::start();
        let mut client = server.connect();
        send(
            &mut client,
            Message::Cancel {
                task_id: "t1".into(),
            },
        );
        assert!(
            matches!(receive(&mut client), Message::Error { code, .. } if code == "hello_required")
        );
        hello(&mut client);
        for message in [
            Message::Cancel {
                task_id: "../escape".into(),
            },
            Message::Accepted {
                task_id: "t1".into(),
                request_id: "r1".into(),
            },
        ] {
            send(&mut client, message);
            assert!(
                matches!(receive(&mut client), Message::Error { code, .. } if code == "invalid_request")
            );
        }
        for bytes in [
            b"{\"version\":99,\"type\":\"hello\",\"client\":\"t\"}\n".to_vec(),
            b"invalid\n".to_vec(),
            vec![b'x'; MAX_FRAME_BYTES],
        ] {
            let mut bad = server.connect();
            bad.get_mut().write_all(&bytes).unwrap();
            assert!(matches!(receive(&mut bad), Message::Error { .. }));
            assert_eq!(bad.read(&mut [0]).unwrap(), 0);
        }
        assert_eq!(server.calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn stalled_clients_do_not_block_requests_or_shutdown() {
        let server = Server::start();
        let mut partial = server.connect();
        partial.get_mut().write_all(b"{\"version\":").unwrap();
        let mut slow = server.connect();
        hello(&mut slow);
        for _ in 0..8 {
            send(
                &mut slow,
                Message::Logs {
                    task_id: "t1".into(),
                    cursor: 0,
                    limit: 1,
                },
            );
        }
        let mut healthy = server.connect();
        hello(&mut healthy);
        send(
            &mut healthy,
            Message::Snapshot {
                task_id: "t1".into(),
            },
        );
        assert!(
            matches!(receive(&mut healthy), Message::Error { code, .. } if code == "test_reply")
        );
        drop(server);
    }
}
