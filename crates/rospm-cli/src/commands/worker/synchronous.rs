//! One detached worker per invocation preserves argv, cwd and environment without
//! persisting credentials or changing process-global state in a shared worker.
// shortcut: one worker per invocation; reuse workers when cwd/environment become request-scoped.
use super::*;
use crate::Cli;
use rospm_core::execution::{Confirmation, Stream};
use std::{
    io::{self, IsTerminal},
    os::fd::{AsRawFd, OwnedFd},
    os::unix::process::CommandExt,
    process::{Command, ExitCode, Stdio},
    thread,
};

pub(crate) fn submit(cli: &Cli) -> Result<ExitCode> {
    let global = Global {
        verbose: cli.verbose > 0,
        manifest: cli.manifest.clone(),
        json: cli.json,
        interaction: None,
        task: None,
        interactive: io::stdin().is_terminal(),
    };
    let loaded = load(&global)?;
    {
        let _project = super::super::mutation_lease(&loaded.root)?;
        let _workspaces = loaded.workspace_leases()?;
    }
    let id = super::super::interaction::task_id();
    // A stable project-local runtime lets separate CLI invocations reconnect to
    // the same worker. The task ID is still unique, so reconnecting cannot
    // replay a side-effecting submission.
    let runtime = loaded.root.join(".rospm/worker/runtime");
    let stream = match connect(&runtime) {
        Ok(stream) => stream,
        Err(error) if is_missing_worker(&error) => {
            let socket = runtime.join("worker.sock");
            if let Ok(metadata) = std::fs::symlink_metadata(&socket) {
                anyhow::ensure!(
                    metadata.file_type().is_socket(),
                    "worker socket path is not a socket"
                );
                // A refused connection means the old listener is gone. Remove
                // only that stale socket; an active worker was already returned
                // by `connect` above.
                std::fs::remove_file(socket)?;
            }
            let mut command = Command::new(std::env::current_exe()?);
            command
                .arg("--internal-worker-runtime")
                .arg(&runtime)
                .arg("--internal-worker-task")
                .arg(&id);
            if global.interactive {
                command.arg("--internal-worker-interactive");
            }
            command.args(std::env::args_os().skip(1));
            command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            // No terminal or parent-owned output pipe survives in the worker.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut spawned = command.spawn()?;
            let deadline = Instant::now() + Duration::from_secs(10);
            let stream = loop {
                match connect(&runtime) {
                    Ok(stream) => break stream,
                    Err(error) => {
                        if let Some(status) = spawned.try_wait()? {
                            bail!("worker startup failed ({status}): {error}");
                        }
                        if Instant::now() >= deadline {
                            // Nothing has been submitted yet.
                            spawned.kill()?;
                            spawned.wait()?;
                            return Err(error);
                        }
                        thread::sleep(Duration::from_millis(20));
                    }
                }
            };
            stream
        }
        Err(error) => return Err(error),
    };
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut stream = BufReader::new(stream);
    anyhow::ensure!(
        matches!(
            exchange(
                &mut stream,
                Message::Hello {
                    client: "rospm-cli".into()
                }
            )?,
            Message::Hello { .. }
        ),
        "{}",
        tr("worker_cli.expected_hello")
    );
    // Print before submitting: even a lost acknowledgement must be queryable.
    eprintln!(
        "{}",
        rospm_core::msg!(
            "worker_cli.follow",
            id = &id,
            runtime = rospm_core::plan::shell_quote(&runtime.to_string_lossy())
        )
    );
    let reply = exchange(
        &mut stream,
        Message::Submit {
            request_id: id.clone(),
            task_id: id.clone(),
            workspace: loaded.root.to_str().context(tr("worker_cli.utf8"))?.into(),
            operation: cli
                .command
                .mutation(Some(&std::env::current_dir()?.join(&loaded.path)))
                .expect("mutation"),
            interactive: global.interactive,
        },
    )?;
    anyhow::ensure!(
        matches!(reply, Message::Accepted { task_id, .. } if task_id == id),
        "unexpected submit response"
    );
    let mut cursor = 0;
    let mut review = None;
    loop {
        let Message::State { snapshot } = exchange(
            &mut stream,
            Message::Snapshot {
                task_id: id.clone(),
            },
        )?
        else {
            bail!("unexpected snapshot response");
        };
        loop {
            let Message::LogChunk {
                next_cursor,
                entries,
                complete,
                ..
            } = exchange(
                &mut stream,
                Message::Logs {
                    task_id: id.clone(),
                    cursor,
                    limit: 100,
                },
            )?
            else {
                bail!("unexpected log response");
            };
            for entry in entries {
                match entry.stream {
                    LogStream::Stdout => {
                        io::stdout().write_all(entry.text.as_bytes())?;
                        io::stdout().flush()?;
                    }
                    LogStream::Stderr => {
                        io::stderr().write_all(entry.text.as_bytes())?;
                        io::stderr().flush()?;
                    }
                    LogStream::Worker => {
                        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&entry.text) {
                            if value["task"] == id && value["operation"].is_object() {
                                review = Some(value);
                            }
                        }
                    }
                }
            }
            cursor = next_cursor;
            if complete || cursor >= snapshot.log_cursor {
                break;
            }
        }
        if snapshot.state.is_terminal() {
            let code = if snapshot.result.as_ref().is_some_and(|r| r.success) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
            drop(stream);
            wait_for_worker_exit(&runtime);
            return Ok(code);
        }
        if snapshot.state == TaskState::WaitingConfirmation {
            if let Some(request) = review.take() {
                let mut answer = String::new();
                let read = io::stdin().read_line(&mut answer)?;
                let approved = read > 0 && matches!(answer.trim(), "y" | "Y" | "yes")
                    || read > 0
                        && answer.trim().is_empty()
                        && request["operation"]["kind"] == "switch_mirror";
                exchange(
                    &mut stream,
                    Message::Confirm {
                        task_id: id.clone(),
                        step: snapshot.step,
                        response: if approved {
                            ConfirmationResponse::Approve
                        } else {
                            ConfirmationResponse::Reject
                        },
                    },
                )?;
            }
        }
        if snapshot.state == TaskState::WaitingAuthorization {
            authorization::authorize(stream.get_ref(), &id)?;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn is_missing_worker(error: &anyhow::Error) -> bool {
    error.downcast_ref::<io::Error>().is_some_and(|error| {
        matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
        )
    })
}

fn wait_for_worker_exit(runtime: &Path) {
    let socket = runtime.join("worker.sock");
    let deadline = Instant::now() + Duration::from_secs(1);
    while socket.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
}

pub(crate) fn serve(cli: Cli) -> Result<ExitCode> {
    let runtime = cli
        .internal_worker_runtime
        .clone()
        .context("missing internal runtime")?;
    let id = cli
        .internal_worker_task
        .clone()
        .context("missing internal task")?;
    anyhow::ensure!(valid_identifier(&id), "invalid task identifier");
    let global = Global {
        verbose: cli.verbose > 0,
        manifest: cli.manifest.clone(),
        json: cli.json,
        interaction: None,
        task: None,
        interactive: cli.internal_worker_interactive,
    };
    let loaded = load(&global)?;
    let operation = cli
        .command
        .mutation(Some(&std::env::current_dir()?.join(&loaded.path)))
        .context("not a mutation")?;
    let store = TaskStore::open(loaded.root.join(".rospm/worker/tasks"))?;
    let endpoint = SocketEndpoint::bind(&runtime)?;
    let runner = TaskRunner::new(store.clone(), loaded.root.join(".rospm/worker/leases"));
    let handle: RefCell<Option<TaskHandle>> = RefCell::new(None);
    let mut invocation = Some(cli);
    let started = Instant::now();
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
    let result = endpoint.serve_tasks_while(
        &AtomicBool::new(false),
        &store,
        |_, message| {
            match message {
                Message::Submit {
                    task_id,
                    workspace,
                    operation: requested,
                    ..
                } => {
                    if task_id != id
                        || Path::new(&workspace) != loaded.root
                        || requested != operation
                    {
                        return Err("invocation does not match worker".into());
                    }
                    let cli = invocation.take().ok_or("invocation already submitted")?;
                    let task = id.clone();
                    *handle.borrow_mut() = Some(
                        runner
                            .start_with(&id, &workspace, move |interaction| {
                                capture(interaction, &task, || {
                                    let adapter = OutputInteraction {
                                        inner: interaction,
                                        task: &task,
                                        interactive: cli.internal_worker_interactive,
                                        failure: std::sync::Mutex::new(None),
                                    };
                                    let interactive = cli.internal_worker_interactive;
                                    let terminal = authorization::TerminalInteraction(&adapter);
                                    let interaction: &dyn Interaction =
                                        if interactive { &terminal } else { &adapter };
                                    let code = crate::execute_cli(
                                        cli,
                                        Some(interaction),
                                        Some(&task),
                                        interactive,
                                    );
                                    if let Some(error) = adapter.failure.lock().unwrap().take() {
                                        return Err(rospm_core::Error::io(
                                            "<worker-output>",
                                            error,
                                        ));
                                    }
                                    code
                                })
                            })
                            .map_err(|e| e.to_string())?,
                    );
                }
                Message::Confirm {
                    task_id,
                    step,
                    response,
                } if task_id == id => {
                    handle
                        .borrow()
                        .as_ref()
                        .ok_or("task is not active")?
                        .respond_at(step, response)
                        .map_err(|e| e.to_string())?;
                }
                Message::Cancel { task_id } if task_id == id => {
                    handle
                        .borrow()
                        .as_ref()
                        .ok_or("task is not active")?
                        .cancel()
                        .map_err(|e| e.to_string())?;
                }
                _ => return Err("unsupported request".into()),
            }
            Ok(())
        },
        |connected| match handle.borrow().as_ref() {
            Some(handle) => connected || !handle.is_finished(),
            None => started.elapsed() < Duration::from_secs(15),
        },
    );
    if let Some(handle) = handle.into_inner() {
        let _ = handle.join();
    }
    drop(endpoint);
    // The persistent task store remains available to `task status/logs`.
    let _ = std::fs::remove_dir(&runtime);
    result?;
    Ok(ExitCode::SUCCESS)
}

struct OutputInteraction<'a> {
    inner: &'a dyn Interaction,
    task: &'a str,
    interactive: bool,
    failure: std::sync::Mutex<Option<io::Error>>,
}
impl Interaction for OutputInteraction<'_> {
    fn process_groups(&self) -> bool {
        true
    }
    fn cancel_requested(&self, _: &str) -> bool {
        self.failure.lock().unwrap().is_some() || self.inner.cancel_requested(self.task)
    }
    fn event(&self, event: Event<'_>) {
        match event {
            Event::Log {
                stream: Stream::Stdout,
                bytes,
                ..
            } => {
                self.write(1, bytes);
            }
            Event::Log {
                stream: Stream::Stderr,
                bytes,
                ..
            } => {
                self.write(2, bytes);
            }
            // Progress is already written to the caller's captured stdout.
            Event::Log { .. } | Event::LogEnd { .. } => {}
            other => self.inner.event(other),
        }
    }
    fn confirm(&self, request: &Confirmation) -> rospm_core::Result<bool> {
        if !self.interactive {
            return match &request.unavailable {
                Some(message) => Err(rospm_core::Error::Host(message.clone())),
                None => Ok(false),
            };
        }
        let mut prompt = Vec::new();
        if let rospm_core::execution::Operation::Uninstall { commands } = &request.operation {
            for command in commands {
                writeln!(prompt, "  $ {}", command.render())
                    .map_err(|e| rospm_core::Error::io("<confirmation>", e))?;
            }
        }
        super::super::interaction::render(request, &mut prompt)?;
        self.inner.event(Event::Log {
            task: self.task,
            step: request.step,
            stream: Stream::Stderr,
            bytes: &prompt,
        });
        self.inner.confirm(request)
    }
}

/// Only this single-task worker redirects process fds. Drain both pipes before
/// publishing the final snapshot, including JSON and preparation diagnostics.
fn capture(
    interaction: &dyn Interaction,
    task: &str,
    run: impl FnOnce() -> rospm_core::Result<()>,
) -> rospm_core::Result<()> {
    struct Redirect {
        fd: i32,
        saved: OwnedFd,
    }
    impl Drop for Redirect {
        fn drop(&mut self) {
            unsafe {
                libc::dup2(self.saved.as_raw_fd(), self.fd);
            }
        }
    }
    let io_error = |e| rospm_core::Error::io("<worker-output>", e);
    thread::scope(|scope| {
        let mut guards = Vec::new();
        let mut readers = Vec::new();
        for (fd, stream) in [(1, Stream::Stdout), (2, Stream::Stderr)] {
            let (mut read, write) = UnixStream::pair().map_err(io_error)?;
            let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
            let saved = borrowed.try_clone_to_owned().map_err(io_error)?;
            if unsafe { libc::dup2(write.as_raw_fd(), fd) } < 0 {
                return Err(io_error(io::Error::last_os_error()));
            }
            guards.push(Redirect { fd, saved });
            drop(write);
            readers.push(scope.spawn(move || -> io::Result<()> {
                let mut bytes = [0; 8192];
                loop {
                    let count = read.read(&mut bytes)?;
                    if count == 0 {
                        break;
                    }
                    interaction.event(Event::Log {
                        task,
                        step: 0,
                        stream,
                        bytes: &bytes[..count],
                    });
                }
                Ok(())
            }));
        }
        let result = run();
        let flush = io::stdout().flush().and_then(|()| io::stderr().flush());
        drop(guards);
        for reader in readers {
            reader
                .join()
                .map_err(|_| {
                    rospm_core::Error::Host(rospm_core::Message::text("output reader panicked"))
                })?
                .map_err(io_error)?;
        }
        flush.map_err(io_error)?;
        result
    })
}

impl OutputInteraction<'_> {
    fn write(&self, fd: i32, bytes: &[u8]) {
        // Avoid stdout's Rust mutex: the plan thread holds it while pipe readers emit events.
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
        let result = borrowed
            .try_clone_to_owned()
            .and_then(|owned| std::fs::File::from(owned).write_all(bytes));
        if let Err(error) = result {
            *self.failure.lock().unwrap() = Some(error);
        }
    }
}
