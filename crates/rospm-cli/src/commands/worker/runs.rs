use super::*;
use crate::commands::run_process::ManagedRun;
use crate::commands::shell_session::Session;
use authorization::Authorization;
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
    detached_tasks: Vec<String>,
    pending_authorization: Option<String>,
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

    fn check_sudo(&mut self) -> Result<()> {
        Authorization::for_tui().check_sudo()
    }

    fn pending_authorization(&self) -> Option<&str> {
        self.pending_authorization.as_deref()
    }

    fn authorize_task(&mut self, root: &Path, id: &str) -> Result<()> {
        let worker = connect(&root.join(".rospm/worker/runtime"))?;
        authorization::authorize_with(&worker, id, Authorization::for_tui())
    }

    fn attach_task(&mut self, root: &Path, id: &str) -> Result<()> {
        attach_with(
            &root.join(".rospm/worker/runtime"),
            id,
            Authorization::for_tui(),
        )
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

    fn start_debug(
        &mut self,
        root: &Path,
        manifest: &Path,
        tool: &str,
    ) -> Result<rospm_tui::DebugStart> {
        let manifest = std::env::current_dir()?.join(manifest);
        if let Some(package) = crate::commands::run::missing_debug_package(manifest.clone(), tool)?
        {
            return Ok(rospm_tui::DebugStart::Missing { package });
        }
        let log = output::create()?;
        let process = ManagedRun::start_debug(root, manifest, tool, writer(&log)?)?;
        let status = process.status().clone();
        self.track(status.clone(), log);
        self.foreground.push(process);
        Ok(rospm_tui::DebugStart::Started(status))
    }

    fn debug_tool_statuses(
        &mut self,
        manifest: &Path,
    ) -> Result<[rospm_core::probe::Status; rospm_core::run::DEBUG_TOOLS.len()]> {
        crate::commands::run::debug_tool_statuses(manifest)
    }

    fn install_debug(&mut self, manifest: &Path, tool: &str) -> Result<()> {
        let manifest = std::env::current_dir()?.join(manifest);
        let root = manifest.parent().context("missing manifest directory")?;
        self.task(
            root,
            Some(&manifest),
            Operation::DebugToolInstall {
                tool: tool.into(),
                manifest: manifest.clone(),
            },
        )
        .with_context(|| tr("cli.tui.debug.install_failed"))?;
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
        self.pending_authorization = None;
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
        for process in task_processes(root)? {
            if process.task_connected
                && self.detached_tasks.contains(&process.id)
                && process
                    .task_snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.state == TaskState::WaitingAuthorization)
            {
                self.pending_authorization = Some(process.id.clone());
            }
            running.push(process);
        }
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
        if process.task {
            anyhow::ensure!(
                signal == RunSignal::Terminate,
                "tasks only support cancellation"
            );
            query(
                connect(&root.join(".rospm/worker/runtime"))?,
                Message::Cancel {
                    task_id: process.id.clone(),
                },
            )?;
            Ok(())
        } else if process.background {
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
        self.shell_status();
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
                        interactive: true,
                    }
                )?,
                Message::Accepted { task_id, .. } if task_id == id
            ),
            "worker did not accept the TUI task"
        );
        eprintln!(
            "{}",
            rospm_core::msg!(
                "worker_cli.follow",
                id = &id,
                runtime = rospm_core::plan::shell_quote(&runtime.to_string_lossy())
            )
        );
        if detach {
            self.detached_tasks.push(id);
            Ok(rospm_tui::TaskOutcome::Detached)
        } else {
            follow_task(&mut stream, &id, Authorization::for_tui())?;
            Ok(rospm_tui::TaskOutcome::Completed)
        }
    }
}

fn task_processes(root: &Path) -> Result<Vec<RunStatus>> {
    let tasks = root.join(".rospm/worker/tasks");
    let metadata = match std::fs::symlink_metadata(&tasks) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| tasks.display().to_string()),
    };
    anyhow::ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0,
        "task store must be a private directory owned by the current user: {}",
        tasks.display()
    );
    let connected = connect(&root.join(".rospm/worker/runtime")).is_ok();
    let mut processes = Vec::new();
    for entry in std::fs::read_dir(&tasks)? {
        let entry = entry?;
        let id = entry.file_name().to_string_lossy().into_owned();
        if !entry.file_type()?.is_dir() || !valid_identifier(&id) {
            continue;
        }
        let path = entry.path().join("snapshot.json");
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(file) => file,
            // A submission or retention cleanup can be between directory and snapshot writes.
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).with_context(|| path.display().to_string()),
        };
        anyhow::ensure!(
            file.metadata()?.is_file(),
            "task snapshot is not a file: {}",
            path.display()
        );
        let snapshot: TaskSnapshot =
            serde_json::from_reader(file).with_context(|| path.display().to_string())?;
        anyhow::ensure!(
            snapshot.task_id == id,
            "task snapshot ID mismatch: {}",
            path.display()
        );
        let exit = snapshot.state.is_terminal().then(|| match snapshot.state {
            TaskState::Succeeded => RunExit::Code(0),
            TaskState::Failed => RunExit::Code(
                snapshot
                    .result
                    .as_ref()
                    .and_then(|r| r.exit_code)
                    .unwrap_or(1),
            ),
            _ => RunExit::Unknown,
        });
        processes.push(RunStatus {
            target: format!("task/{id}"),
            id,
            background: true,
            task: true,
            task_snapshot: Some(snapshot),
            task_connected: connected,
            shell: false,
            last_command: None,
            pid: None,
            suspended: false,
            output_fd: None,
            exit,
        });
    }
    processes.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(processes)
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
            record
                .process
                .last_command
                .clone_from(&process.last_command);
        }
    }

    fn shell_status(&mut self) -> Option<RunStatus> {
        let status = self.shell.as_mut()?.refresh();
        let mut latest = [self.shell.as_ref()?.status().clone()];
        self.update_running(&mut latest);
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

pub(super) fn attach(runtime: &Path, id: &str) -> Result<()> {
    attach_with(runtime, id, Authorization::Terminal)
}

fn attach_with(runtime: &Path, id: &str, authorization: Authorization) -> Result<()> {
    authorization::foreground_terminal()?;
    let mut stream = BufReader::new(connect(runtime)?);
    stream
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(10)))?;
    stream
        .get_ref()
        .set_write_timeout(Some(Duration::from_secs(10)))?;
    exchange(
        &mut stream,
        Message::Hello {
            client: "rospm-attach".into(),
        },
    )?;
    follow_task(&mut stream, id, authorization)
}

fn follow_task(
    stream: &mut BufReader<UnixStream>,
    id: &str,
    authorization: Authorization,
) -> Result<()> {
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
            if let Some(review) = review
                .take()
                .filter(|review| review["step"] == snapshot.step)
            {
                show_review(&review)?;
                let mut answer = String::new();
                let read = io::stdin().read_line(&mut answer)?;
                let approved = read > 0
                    && (matches!(answer.trim(), "y" | "Y" | "yes")
                        || (answer.trim().is_empty()
                            && review["operation"]["kind"] == "switch_mirror"));
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
        if snapshot.state == TaskState::WaitingAuthorization {
            authorization::authorize_with(stream.get_ref(), id, authorization)?;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn show_review(review: &serde_json::Value) -> Result<()> {
    let mut err = io::stderr().lock();
    if let Some(explanation) = review["explanation"].as_str() {
        writeln!(err, "{explanation}")?;
    }
    let operation = &review["operation"];
    for key in ["commands", "retry"] {
        if let Some(commands) = operation[key].as_array() {
            for command in commands {
                let command: rospm_core::plan::Command = serde_json::from_value(command.clone())?;
                writeln!(err, "  $ {}", command.render())?;
            }
        }
    }
    for path in operation["paths"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
    {
        writeln!(err, "  {path}")?;
    }
    for key in ["ros_apt", "rosdistro"] {
        if let Some(value) = operation[key].as_str() {
            writeln!(err, "  {value}")?;
        }
    }
    write!(
        err,
        "{}",
        review["prompt"]
            .as_str()
            .context("missing confirmation prompt")?
    )?;
    err.flush()?;
    Ok(())
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
                    Ok(output_fd
                        .is_some()
                        .then_some(Message::RunStarted { process: started }))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_discovery_restores_states_and_results_without_creating_or_replaying_tasks() {
        let root = std::env::temp_dir().join(task_id());
        assert!(task_processes(&root).unwrap().is_empty());
        assert!(!root.exists());
        let store = TaskStore::open(root.join(".rospm/worker/tasks")).unwrap();
        for (index, state) in [
            TaskState::Pending,
            TaskState::Running,
            TaskState::WaitingConfirmation,
            TaskState::WaitingAuthorization,
            TaskState::CancelRequested,
            TaskState::Succeeded,
            TaskState::Failed,
            TaskState::Cancelled,
            TaskState::Interrupted,
            TaskState::Unknown,
        ]
        .into_iter()
        .enumerate()
        {
            store
                .create_task(&TaskSnapshot {
                    task_id: format!("task-{index}"),
                    result: state.is_terminal().then(|| TaskResult {
                        success: state == TaskState::Succeeded,
                        exit_code: Some(7),
                        error: Some("saved result".into()),
                    }),
                    state,
                    step: 2,
                    total_steps: Some(3),
                    log_cursor: 0,
                })
                .unwrap();
        }
        let processes = task_processes(&root).unwrap();
        assert_eq!(processes.len(), 10);
        for process in &processes {
            let snapshot = store.load_snapshot(&process.id).unwrap();
            assert_eq!(process.task_snapshot.as_ref(), Some(&snapshot));
            assert_eq!(process.exit.is_some(), snapshot.state.is_terminal());
            assert!(process.task && !process.task_connected && process.pid.is_none());
            if snapshot.state == TaskState::Failed {
                assert_eq!(process.exit, Some(RunExit::Code(7)));
            }
            if matches!(snapshot.state, TaskState::Unknown | TaskState::Interrupted) {
                assert_eq!(process.exit, Some(RunExit::Unknown));
            }
        }
        assert_eq!(task_processes(&root).unwrap(), processes);
        assert!(!root.join(".rospm/worker/runtime").exists());
        assert!(task_processes(&root.join("other-project"))
            .unwrap()
            .is_empty());
        let mut snapshot = store.load_snapshot("task-0").unwrap();
        snapshot.state = TaskState::Cancelled;
        store.write_snapshot(&snapshot).unwrap();
        assert!(task_processes(&root)
            .unwrap()
            .iter()
            .find(|p| p.id == "task-0")
            .unwrap()
            .exit
            .is_some());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn task_discovery_rejects_mismatched_ids_and_symlinked_snapshots() {
        let root = std::env::temp_dir().join(task_id());
        let store = TaskStore::open(root.join(".rospm/worker/tasks")).unwrap();
        let snapshot = TaskSnapshot {
            task_id: "task".into(),
            state: TaskState::Running,
            step: 0,
            total_steps: None,
            log_cursor: 0,
            result: None,
        };
        store.create_task(&snapshot).unwrap();
        let path = root.join(".rospm/worker/tasks/task/snapshot.json");
        let mut bad = snapshot;
        bad.task_id = "another-task".into();
        std::fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
        assert!(task_processes(&root)
            .unwrap_err()
            .to_string()
            .contains("ID mismatch"));
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(root.join("outside"), &path).unwrap();
        assert!(task_processes(&root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
