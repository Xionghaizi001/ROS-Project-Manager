use super::*;
use crate::commands::run_process::ManagedRun;
use crate::commands::shell_session::Session;
use rospm_core::run::{output, RunExit, RunRequest, RunSignal, RunStatus};
use rospm_core::worker::{LogStream, TaskSnapshot, TaskState};
use std::{
    fs::{File, OpenOptions},
    io::{self, BufReader, Write},
    os::fd::AsRawFd,
    os::unix::fs::OpenOptionsExt,
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Default)]
pub struct Runs {
    foreground: Vec<ManagedRun>,
    worker: Option<Child>,
    shell: Option<Session>,
    logs: Vec<RecordedRun>,
    task_runs: Vec<RunStatus>,
    task_logs: Vec<File>,
}

struct RecordedRun {
    process: RunStatus,
    log: File,
    transcript: bool,
}

impl RecordedRun {
    fn exit(&self) -> Option<RunExit> {
        if self.transcript {
            output::exit(&self.log).ok().flatten()
        } else {
            None
        }
    }
}

impl rospm_tui::RunBackend for Runs {
    fn has_task_worker(&self) -> bool {
        true
    }

    fn start(&mut self, root: &Path, request: RunRequest, background: bool) -> Result<()> {
        let log = output::create()?;
        if !background {
            let process = ManagedRun::start(root, request, false, writer(&log)?)?;
            self.track(process.status().clone(), log);
            self.foreground.push(process);
            return Ok(());
        }
        let runtime = WorkspaceLock::run_runtime(root)?;
        let stream = self.ensure_worker(root, &runtime)?;
        match query(
            stream,
            Message::StartRun {
                request,
                output_fd: Some(log.as_raw_fd()),
            },
        )? {
            Message::RunStarted { process } => self.track(process, log),
            Message::Runs { processes } => {
                // Older workers retain their original per-run log on disk.
                if let Some(process) = processes.into_iter().last() {
                    if let Some(file) = legacy_output(root, &process) {
                        self.track(process, file);
                    }
                }
            }
            _ => bail!("unexpected start response from run worker"),
        }
        Ok(())
    }

    fn foreground(&mut self) -> Vec<RunStatus> {
        self.foreground.retain_mut(ManagedRun::refresh);
        let mut running = statuses(&self.foreground);
        running.extend(self.shell_status());
        self.update_running(&mut running);
        running
    }

    fn background(&mut self, root: &Path) -> Result<Vec<RunStatus>> {
        if let Some(worker) = &mut self.worker {
            if worker.try_wait()?.is_some() {
                self.worker = None;
            }
        }
        let runtime = WorkspaceLock::run_runtime(root)?;
        let mut running = match connect(&runtime) {
            Ok(stream) => {
                let peer = peer_identity(&stream)?;
                let Message::Runs { processes } = query(stream, Message::ListRuns)? else {
                    bail!("unexpected list response from run worker");
                };
                for process in &processes {
                    if !self
                        .logs
                        .iter()
                        .any(|record| record.process.id == process.id)
                    {
                        let file = match process.output_fd {
                            Some(fd) => output::open(peer.pid, fd, false).ok(),
                            None => legacy_output(root, process),
                        };
                        if let Some(file) = file {
                            self.track(process.clone(), file);
                        }
                    }
                }
                processes
            }
            Err(error) if missing(&error) => Vec::new(),
            Err(error) => return Err(error),
        };
        self.update_running(&mut running);
        for process in &mut self.task_runs {
            let path = root
                .join(".rospm/worker/tasks")
                .join(&process.id)
                .join("snapshot.json");
            if let Ok(bytes) = std::fs::read(path) {
                if let Ok(snapshot) = serde_json::from_slice::<TaskSnapshot>(&bytes) {
                    if snapshot.state.is_terminal() {
                        process.exit = Some(if snapshot.result.is_some_and(|r| r.success) {
                            RunExit::Code(0)
                        } else {
                            RunExit::Code(1)
                        });
                    }
                }
            }
        }
        running.extend(self.task_runs.iter().cloned());
        for record in &mut self.logs {
            if record.process.background
                && record.process.exit.is_none()
                && !running.iter().any(|item| item.id == record.process.id)
            {
                record.process.exit = record.exit().or(Some(RunExit::Unknown));
            }
        }
        Ok(running)
    }

    fn history(&mut self) -> Vec<RunStatus> {
        self.logs
            .iter_mut()
            .rev()
            .filter_map(|record| {
                if record.process.exit.is_none() {
                    record.process.exit = record.exit();
                }
                record.process.exit.map(|_| record.process.clone())
            })
            .collect()
    }

    fn signal(&mut self, root: &Path, process: &RunStatus, signal: RunSignal) -> Result<()> {
        if process.background {
            query(
                connect(&WorkspaceLock::run_runtime(root)?)?,
                Message::SignalRun {
                    id: process.id.clone(),
                    signal,
                },
            )?;
            Ok(())
        } else {
            if let Some(shell) = self.shell.as_ref().filter(|shell| shell.id() == process.id) {
                return shell.signal(signal);
            }
            signal_run(&self.foreground, &process.id, signal)
        }
    }

    fn close_foreground(&mut self) {
        self.shell = None;
        self.foreground.clear();
    }

    fn shell(&mut self, root: &Path, manifest: &Path) -> Result<()> {
        self.shell_status();
        if self.shell.is_none() {
            let log = output::create()?;
            let shell = Session::start(root, manifest, writer(&log)?)?;
            self.track(shell.status().clone(), log);
            self.shell = Some(shell);
        }
        let result = self.shell.as_mut().unwrap().attach();
        if !matches!(result, Ok(true)) {
            self.shell = None;
        }
        result.map(|_| ())
    }

    fn task(
        &mut self,
        root: &Path,
        manifest: Option<&Path>,
        operation: Operation,
    ) -> Result<rospm_tui::TaskOutcome> {
        let detach = operation == Operation::DepsInstall;
        let runtime = root.join(".rospm/worker/runtime");
        let stream = self.ensure_task_worker(root, &runtime, manifest)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        let id = task_id();
        let workspace = root.canonicalize()?.to_string_lossy().into_owned();
        let mut stream = BufReader::new(stream);
        anyhow::ensure!(
            matches!(
                exchange(
                    &mut stream,
                    Message::Hello {
                        client: "rospm-tui".into()
                    }
                )?,
                Message::Hello { .. }
            ),
            "worker did not accept the TUI connection"
        );
        anyhow::ensure!(
            matches!(
                exchange(
                    &mut stream,
                    Message::Submit {
                        request_id: id.clone(),
                        task_id: id.clone(),
                        workspace,
                        operation,
                    }
                )?,
                Message::Accepted { task_id, .. } if task_id == id
            ),
            "worker did not accept the TUI task"
        );
        if detach {
            let log = File::open(
                root.join(".rospm/worker/tasks")
                    .join(&id)
                    .join("logs.jsonl"),
            )?;
            self.task_logs.push(log);
            self.task_runs.push(RunStatus {
                id,
                target: "deps install".into(),
                background: true,
                task: true,
                shell: false,
                pid: None,
                suspended: false,
                output_fd: self.task_logs.last().map(|file| file.as_raw_fd()),
                exit: None,
            });
            Ok(rospm_tui::TaskOutcome::Detached)
        } else {
            follow_task(&mut stream, &id)?;
            Ok(rospm_tui::TaskOutcome::Completed)
        }
    }
}

impl Runs {
    fn track(&mut self, mut process: RunStatus, file: File) {
        let fd = file.as_raw_fd();
        process.output_fd = Some(fd);
        self.logs.push(RecordedRun {
            process,
            log: file,
            transcript: output::is_transcript(fd),
        });
    }

    fn update_running(&mut self, running: &mut [RunStatus]) {
        for process in running {
            let Some(record) = self
                .logs
                .iter_mut()
                .find(|record| record.process.id == process.id)
            else {
                process.output_fd = None;
                continue;
            };
            process.output_fd = record.process.output_fd;
            record.process.pid = process.pid;
        }
    }

    fn shell_status(&mut self) -> Option<RunStatus> {
        let status = self.shell.as_mut()?.refresh();
        if status.is_none() {
            self.shell = None;
        }
        status
    }

    fn ensure_worker(&mut self, root: &Path, runtime: &Path) -> Result<UnixStream> {
        match connect(runtime) {
            Ok(stream) => return Ok(stream),
            Err(error) if missing(&error) => {}
            Err(error) => return Err(error),
        }
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("worker")
            .arg("--run-project")
            .arg(root)
            .arg("--runtime")
            .arg(runtime)
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut worker = command.spawn()?;
        let deadline = Instant::now() + Duration::from_secs(2);
        let result = loop {
            match connect(runtime) {
                Ok(stream) => break Ok(stream),
                Err(error) if missing(&error) => {
                    if worker.try_wait()?.is_some() || Instant::now() >= deadline {
                        break Err(error).context("run worker did not start");
                    }
                }
                Err(error) => break Err(error),
            }
            thread::sleep(Duration::from_millis(20));
        };
        self.worker = Some(worker);
        result
    }

    fn ensure_task_worker(
        &mut self,
        root: &Path,
        runtime: &Path,
        manifest: Option<&Path>,
    ) -> Result<UnixStream> {
        match connect(runtime) {
            Ok(stream) => return Ok(stream),
            Err(error) if missing(&error) => {}
            Err(error) => return Err(error),
        }
        let mut command = Command::new(std::env::current_exe()?);
        command.arg("worker").arg("--runtime").arg(runtime);
        if let Some(manifest) = manifest {
            command.arg("--manifest").arg(manifest.canonicalize()?);
        }
        command
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut worker = command.spawn()?;
        let deadline = Instant::now() + Duration::from_secs(2);
        let result = loop {
            match connect(runtime) {
                Ok(stream) => break Ok(stream),
                Err(error) if missing(&error) => {
                    if worker.try_wait()?.is_some() || Instant::now() >= deadline {
                        break Err(error).context("task worker did not start");
                    }
                }
                Err(error) => break Err(error),
            }
            thread::sleep(Duration::from_millis(20));
        };
        let _ = worker.try_wait();
        result
    }
}

fn missing(error: &anyhow::Error) -> bool {
    error.downcast_ref::<io::Error>().is_some_and(|error| {
        matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
        )
    })
}

fn task_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let tick = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "tui-{}-{tick}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn follow_task(stream: &mut BufReader<UnixStream>, id: &str) -> Result<()> {
    let mut cursor = 0;
    let mut review = None;
    loop {
        let Message::State { snapshot } =
            exchange(stream, Message::Snapshot { task_id: id.into() })?
        else {
            anyhow::bail!("unexpected task snapshot response")
        };
        loop {
            let Message::LogChunk {
                next_cursor,
                complete,
                entries,
                ..
            } = exchange(
                stream,
                Message::Logs {
                    task_id: id.into(),
                    cursor,
                    limit: 100,
                },
            )?
            else {
                anyhow::bail!("unexpected task log response")
            };
            for entry in entries {
                match entry.stream {
                    LogStream::Stdout => io::stdout().write_all(entry.text.as_bytes())?,
                    LogStream::Stderr => io::stderr().write_all(entry.text.as_bytes())?,
                    LogStream::Worker => {
                        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&entry.text) {
                            if value["task"] == id && value["operation"].is_object() {
                                review = Some(value);
                            }
                        }
                    }
                }
            }
            io::stdout().flush()?;
            io::stderr().flush()?;
            cursor = next_cursor;
            if complete || cursor >= snapshot.log_cursor {
                break;
            }
        }
        if snapshot.state.is_terminal() {
            return match snapshot.result.as_ref().filter(|result| result.success) {
                Some(_) => Ok(()),
                None => anyhow::bail!(
                    "{}",
                    snapshot
                        .result
                        .and_then(|result| result.error)
                        .unwrap_or_else(|| "worker task failed".into())
                ),
            };
        }
        if snapshot.state == TaskState::WaitingConfirmation {
            if review.take().is_some() {
                let mut answer = String::new();
                let read = io::stdin().read_line(&mut answer)?;
                let approved = read > 0 && matches!(answer.trim(), "y" | "Y" | "yes");
                exchange(
                    stream,
                    Message::Confirm {
                        task_id: id.into(),
                        step: snapshot.step,
                        response: if approved {
                            rospm_core::worker::ConfirmationResponse::Approve
                        } else {
                            rospm_core::worker::ConfirmationResponse::Reject
                        },
                    },
                )?;
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn writer(file: &File) -> std::io::Result<File> {
    output::open(std::process::id() as i32, file.as_raw_fd(), true)
}

fn legacy_output(root: &Path, process: &RunStatus) -> Option<File> {
    if !valid_identifier(&process.id) {
        return None;
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(
            root.join(".rospm/log/run")
                .join(format!("{}.log", process.id)),
        )
        .ok()?;
    file.metadata().ok()?.is_file().then_some(file)
}

fn query(stream: UnixStream, message: Message) -> Result<Message> {
    stream.set_read_timeout(Some(Duration::from_millis(300)))?;
    stream.set_write_timeout(Some(Duration::from_millis(300)))?;
    let mut stream = BufReader::new(stream);
    exchange(
        &mut stream,
        Message::Hello {
            client: "rospm-tui".into(),
        },
    )?;
    exchange(&mut stream, message)
}

fn statuses(processes: &[ManagedRun]) -> Vec<RunStatus> {
    processes
        .iter()
        .map(|process| process.status().clone())
        .collect()
}

fn signal_run(processes: &[ManagedRun], id: &str, signal: RunSignal) -> Result<()> {
    let process = processes
        .iter()
        .find(|process| process.status().id == id)
        .context(tr("cli.tui.run_gone"))?;
    anyhow::ensure!(
        process.status().pid.is_some(),
        "{}",
        tr("cli.tui.run_starting")
    );
    process.signal(signal)
}

pub fn serve(root: PathBuf, runtime: &Path, idle_timeout: Duration) -> Result<()> {
    let root = root.canonicalize()?;
    anyhow::ensure!(
        runtime == WorkspaceLock::run_runtime(&root)?,
        "invalid run worker runtime"
    );
    TaskStore::open(runtime)?;
    let _lease =
        WorkspaceLock::try_acquire(runtime, &root)?.context(tr("worker_cli.already_running"))?;
    let socket = runtime.join("worker.sock");
    if let Ok(metadata) = std::fs::symlink_metadata(&socket) {
        anyhow::ensure!(
            metadata.file_type().is_socket(),
            "{}",
            tr("worker_cli.not_socket")
        );
        std::fs::remove_file(socket)?;
    }
    let endpoint = SocketEndpoint::bind(runtime)?;
    let processes = RefCell::new(Vec::new());
    let mut last_busy = Instant::now();
    let mut last_refresh = Instant::now();
    unsafe {
        libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
    endpoint.serve_while(
        &STOP,
        |peer, message| {
            let mut processes = processes.borrow_mut();
            processes.retain_mut(ManagedRun::refresh);
            let result = match message {
                Message::ListRuns => Ok(None),
                Message::StartRun { request, output_fd } => (|| {
                    let manifest_root = request
                        .manifest
                        .parent()
                        .context("missing run manifest directory")?;
                    anyhow::ensure!(
                        manifest_root.canonicalize()? == root,
                        "run manifest is outside the worker project"
                    );
                    let log = match output_fd {
                        Some(fd) => output::open(peer.pid, fd, true)?,
                        None => output::create()?,
                    };
                    let process = ManagedRun::start(&root, request, true, log)?;
                    let started = process.status().clone();
                    processes.push(process);
                    Ok(Some(Message::RunStarted { process: started })
                        .filter(|_| output_fd.is_some()))
                })(),
                Message::SignalRun { id, signal } => {
                    signal_run(&processes, &id, signal).map(|_| None)
                }
                _ => Err(anyhow::anyhow!("unsupported run worker request")),
            };
            match result {
                Ok(Some(reply)) => reply,
                Ok(None) => Message::Runs {
                    processes: statuses(&processes),
                },
                Err(error) => Message::Error {
                    code: "run_failed".into(),
                    message: super::super::describe(&error),
                },
            }
        },
        |connected| {
            let mut processes = processes.borrow_mut();
            if last_refresh.elapsed() >= Duration::from_millis(250) {
                processes.retain_mut(ManagedRun::refresh);
                last_refresh = Instant::now();
            }
            if connected || !processes.is_empty() {
                last_busy = Instant::now();
            }
            last_busy.elapsed() < idle_timeout
        },
    )?;
    Ok(())
}
