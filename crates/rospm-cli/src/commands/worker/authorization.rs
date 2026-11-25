//! A detached worker hands a generated command to a foreground client once.
//! Sudo opens that client's controlling terminal itself for authentication.
use super::*;
use rospm_core::{
    execution::{Confirmation, State},
    plan, Error,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
    fs::File,
    hash::{DefaultHasher, Hash, Hasher},
    io::{self, IsTerminal},
    os::{
        fd::AsRawFd,
        linux::net::SocketAddrExt,
        unix::{
            net::{SocketAddr, UnixListener},
            process::ExitStatusExt,
        },
    },
    process::{Command, ExitStatus, Stdio},
    sync::Mutex,
    thread,
};

#[derive(Serialize, Deserialize)]
struct Request {
    command: plan::Command,
    input: Option<String>,
}

#[derive(Serialize, Deserialize)]
enum Reply {
    Output { stderr: bool, bytes: Vec<u8> },
    Exit(i32),
    Failed(String),
}

pub(super) struct TerminalInteraction<'a>(pub &'a dyn Interaction);

impl Interaction for TerminalInteraction<'_> {
    fn event(&self, event: Event<'_>) {
        self.0.event(event);
    }

    fn process_groups(&self) -> bool {
        self.0.process_groups()
    }

    fn cancel_requested(&self, task: &str) -> bool {
        self.0.cancel_requested(task)
    }

    fn confirm(&self, request: &Confirmation) -> rospm_core::Result<bool> {
        self.0.confirm(request)
    }

    fn terminal_command(
        &self,
        task: &str,
        step: usize,
        command: &plan::Command,
        input: Option<&str>,
    ) -> Option<rospm_core::Result<ExitStatus>> {
        Some(delegate(self.0, task, step, command, input))
    }
}

fn address(pid: u32, task: &str) -> io::Result<SocketAddr> {
    let mut hash = DefaultHasher::new();
    task.hash(&mut hash);
    SocketAddr::from_abstract_name(format!("rospm-auth-{pid}-{:016x}", hash.finish()))
}

fn send(mut stream: &UnixStream, value: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() >= MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "authorization frame too large",
        ));
    }
    stream.write_all(&bytes)?;
    stream.write_all(b"\n")
}

fn receive<T: DeserializeOwned>(stream: &mut BufReader<UnixStream>) -> io::Result<T> {
    let mut bytes = Vec::new();
    stream
        .take(MAX_FRAME_BYTES as u64)
        .read_until(b'\n', &mut bytes)?;
    if bytes.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "incomplete authorization frame",
        ));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn delegate(
    interaction: &dyn Interaction,
    task: &str,
    step: usize,
    command: &plan::Command,
    input: Option<&str>,
) -> rospm_core::Result<ExitStatus> {
    let io_error = |e| Error::io("<worker-authorization>", e);
    let state = |state| interaction.event(Event::Interaction { task, step, state });
    let mut command = command.clone();
    if command.cwd.is_none() {
        command.cwd = Some(std::env::current_dir().map_err(io_error)?);
    }
    let request = Request {
        command,
        input: input.map(str::to_owned),
    };
    let listener = UnixListener::bind_addr(&address(std::process::id(), task).map_err(io_error)?)
        .map_err(io_error)?;
    listener.set_nonblocking(true).map_err(io_error)?;
    state(State::WaitingAuthorization);
    let stream = loop {
        if interaction.cancel_requested(task) {
            return Err(Error::Cancelled);
        }
        match listener.accept() {
            Ok((stream, _))
                if peer_identity(&stream)
                    .is_ok_and(|peer| peer.uid == unsafe { libc::geteuid() }) =>
            {
                break stream
            }
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(io_error(error)),
        }
    };
    drop(listener);
    state(State::Running);
    if interaction.cancel_requested(task) {
        return Err(Error::Cancelled);
    }
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(io_error)?;
    // Once the request has left the worker, losing the result cannot prove
    // that the command did not run. Never hand it to a second client.
    send(&stream, &request).map_err(|_| Error::TerminalOutcomeUnknown)?;
    let mut stream = BufReader::new(stream);
    loop {
        match receive(&mut stream).map_err(|_| Error::TerminalOutcomeUnknown)? {
            Reply::Output { stderr, bytes } => interaction.event(Event::Log {
                task,
                step,
                stream: if stderr {
                    Stream::Stderr
                } else {
                    Stream::Stdout
                },
                bytes: &bytes,
            }),
            Reply::Exit(raw) => {
                for stream in [Stream::Stdout, Stream::Stderr] {
                    interaction.event(Event::LogEnd { task, step, stream });
                }
                let status = ExitStatus::from_raw(raw);
                return if status.signal() == Some(libc::SIGINT) || status.code() == Some(130) {
                    Err(Error::Cancelled)
                } else {
                    Ok(status)
                };
            }
            Reply::Failed(error) => return Err(Error::Host(rospm_core::Message::text(error))),
        }
    }
}

pub(super) fn foreground_terminal() -> Result<()> {
    let tty = File::open("/dev/tty").context(tr("worker_cli.authorization_terminal"))?;
    anyhow::ensure!(
        io::stdin().is_terminal() && unsafe { libc::tcgetpgrp(tty.as_raw_fd()) == libc::getpgrp() },
        "{}",
        tr("worker_cli.authorization_terminal")
    );
    Ok(())
}

/// A stale snapshot or a competing client may find the rendezvous already gone.
pub(super) fn authorize(worker: &UnixStream, id: &str) -> Result<()> {
    foreground_terminal()?;
    let peer = peer_identity(worker)?;
    anyhow::ensure!(
        peer.uid == unsafe { libc::geteuid() },
        "invalid worker peer"
    );
    let stream = match UnixStream::connect_addr(&address(peer.pid as u32, id)?) {
        Ok(stream) => stream,
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        peer_identity(&stream)? == peer,
        "invalid authorization peer"
    );
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream);
    let request = receive(&mut reader)?;
    let stream = reader.into_inner();
    match execute(&request, &stream) {
        Ok(status) => send(&stream, &Reply::Exit(status.into_raw()))?,
        Err(error) => send(&stream, &Reply::Failed(format!("{error:#}")))?,
    }
    Ok(())
}

fn execute(request: &Request, stream: &UnixStream) -> Result<ExitStatus> {
    let plan = &request.command;
    anyhow::ensure!(
        plan.env.is_empty() && plan.preamble.is_empty(),
        "invalid terminal command"
    );
    let mut command = if plan.sudo {
        anyhow::ensure!(
            matches!(
                plan.program.as_str(),
                "apt-get"
                    | "locale-gen"
                    | "update-locale"
                    | "add-apt-repository"
                    | "rosdep"
                    | "install"
                    | "mkdir"
                    | "tee"
                    | "mv"
                    | "rm"
            ),
            "invalid system command"
        );
        let mut command = Command::new("sudo");
        command.arg("--").arg(&plan.program);
        command
    } else {
        anyhow::ensure!(
            plan.program == "rosdep" && plan.args.first().is_some_and(|arg| arg == "install"),
            "invalid terminal command"
        );
        Command::new(&plan.program)
    };
    anyhow::ensure!(
        request.input.is_some() == (plan.sudo && plan.program == "tee"),
        "invalid input for terminal command"
    );
    if let Some(cwd) = &plan.cwd {
        command.current_dir(cwd);
    }
    let interrupt = ForegroundInterrupt::new()?;
    let mut child = command
        .args(&plan.args)
        .stdin(if request.input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(if request.input.is_some() {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take().expect("piped stderr");
    let mut stdin = child.stdin.take();
    let output = Mutex::new(stream);
    let drain = |pipe: &mut dyn Read, stderr| -> io::Result<()> {
        let mut bytes = [0; 8192];
        let mut result = Ok(());
        loop {
            let count = pipe.read(&mut bytes)?;
            if count == 0 {
                return result;
            }
            // Keep draining the command if the worker has gone away.
            if result.is_ok() {
                result = send(
                    &output.lock().unwrap(),
                    &Reply::Output {
                        stderr,
                        bytes: bytes[..count].to_vec(),
                    },
                );
            }
        }
    };
    thread::scope(|scope| {
        let out = scope.spawn(|| stdout.as_mut().map_or(Ok(()), |pipe| drain(pipe, false)));
        let err = scope.spawn(|| drain(&mut stderr, true));
        let written = stdin
            .take()
            .zip(request.input.as_ref())
            .map_or(Ok(()), |(mut pipe, text)| pipe.write_all(text.as_bytes()));
        let status = child.wait();
        out.join().expect("stdout reader panicked")?;
        err.join().expect("stderr reader panicked")?;
        let status = status?;
        if status.success() {
            written?;
        } else if interrupt.received() {
            return Ok(ExitStatus::from_raw(libc::SIGINT));
        }
        Ok(status)
    })
}

// Keep the frontend alive while the foreground command handles terminal Ctrl-C.
// exec resets caught signals to their default in the child.
static FOREGROUND_INTERRUPTED: AtomicBool = AtomicBool::new(false);

struct ForegroundInterrupt(libc::sigaction);

impl ForegroundInterrupt {
    fn new() -> io::Result<Self> {
        extern "C" fn record(_: libc::c_int) {
            FOREGROUND_INTERRUPTED.store(true, Ordering::Relaxed);
        }
        FOREGROUND_INTERRUPTED.store(false, Ordering::Relaxed);
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = record as *const () as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        if unsafe { libc::sigaction(libc::SIGINT, &action, &mut previous) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(previous))
    }

    fn received(&self) -> bool {
        FOREGROUND_INTERRUPTED.load(Ordering::Relaxed)
    }
}

impl Drop for ForegroundInterrupt {
    fn drop(&mut self) {
        unsafe { libc::sigaction(libc::SIGINT, &self.0, std::ptr::null_mut()) };
    }
}
