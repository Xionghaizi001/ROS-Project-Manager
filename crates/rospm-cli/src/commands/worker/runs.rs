use super::*;
use crate::commands::run_process::ManagedRun;
use crate::commands::shell_session::Session;
use rospm_core::run::{output, RunExit, RunRequest, RunSignal, RunStatus};
use std::{
    fs::{File, OpenOptions},
    io,
    os::fd::AsRawFd,
    os::unix::fs::OpenOptionsExt,
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
};

#[derive(Default)]
pub struct Runs {
    foreground: Vec<ManagedRun>,
    worker: Option<Child>,
    shell: Option<Session>,
    logs: Vec<RecordedRun>,
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
}

fn missing(error: &anyhow::Error) -> bool {
    error.downcast_ref::<io::Error>().is_some_and(|error| {
        matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
        )
    })
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
