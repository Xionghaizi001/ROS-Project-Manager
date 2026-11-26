//! Shared lifecycle for TUI-owned and worker-owned ROS invocations.
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    os::{
        fd::{AsFd, AsRawFd, OwnedFd},
        unix::{
            net::UnixStream,
            process::{CommandExt, ExitStatusExt},
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{bail, Result};
use rospm_core::run::{output, RunRequest, RunSignal, RunStatus};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum ChildRequest {
    Run(RunRequest),
    Debug { manifest: PathBuf, tool: String },
}

pub(super) struct ManagedRun {
    owner: UnixStream,
    child: Child,
    status: RunStatus,
    reports: Vec<u8>,
    log: File,
    drain: Option<JoinHandle<()>>,
    stopped: Arc<AtomicBool>,
}

impl ManagedRun {
    pub(super) fn start(
        root: &Path,
        request: RunRequest,
        background: bool,
        log: File,
    ) -> Result<Self> {
        let target = format!("{}/{}", request.package, request.name);
        Self::spawn(root, ChildRequest::Run(request), target, background, log)
    }

    pub(super) fn start_debug(
        root: &Path,
        manifest: PathBuf,
        tool: &str,
        log: File,
    ) -> Result<Self> {
        Self::spawn(
            root,
            ChildRequest::Debug {
                manifest,
                tool: tool.into(),
            },
            tool.into(),
            false,
            log,
        )
    }

    fn spawn(
        root: &Path,
        request: ChildRequest,
        target: String,
        background: bool,
        log: File,
    ) -> Result<Self> {
        let id = super::interaction::task_id();
        let (owner, supervisor) = UnixStream::pair()?;
        let (reader, writer) = UnixStream::pair()?;
        owner.set_nonblocking(true)?;
        reader.set_read_timeout(Some(Duration::from_millis(100)))?;
        let stopped = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stopped);
        let mut output = output::Writer::new(log.try_clone()?);
        let drain = thread::Builder::new().spawn(move || {
            let mut reader = reader;
            let mut bytes = [0; 8192];
            while !flag.load(Ordering::Acquire) {
                match reader.read(&mut bytes) {
                    Ok(0) => return,
                    Ok(count) => {
                        let _ = output.append(&bytes[..count]);
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => return,
                }
            }
            // Take the bytes already queued when the supervisor exited, even if an escaped
            // descendant still holds the write end open.
            let pending = pending_bytes(&reader);
            let _ = io::copy(&mut reader.take(pending as u64), &mut output);
        })?;
        let child = Command::new(std::env::current_exe()?)
            .arg("run-child")
            .arg(serde_json::to_string(&request)?)
            .env("ROSPM_LANG", crate::i18n::current().code())
            .current_dir(root)
            .process_group(0)
            .stdin(Stdio::from(OwnedFd::from(supervisor)))
            .stdout(Stdio::from(OwnedFd::from(writer.try_clone()?)))
            .stderr(Stdio::from(OwnedFd::from(writer)))
            .spawn()?;
        Ok(Self {
            owner,
            child,
            status: RunStatus {
                id,
                target,
                background,
                task: false,
                task_snapshot: None,
                task_connected: false,
                shell: false,
                pid: None,
                suspended: false,
                output_fd: Some(log.as_raw_fd()),
                exit: None,
            },
            reports: Vec::new(),
            log,
            drain: Some(drain),
            stopped,
        })
    }

    pub(super) fn refresh(&mut self) -> bool {
        let status = self.child.try_wait();
        self.read_reports();
        if !matches!(status, Ok(None)) {
            self.finish(status.ok().flatten());
            return false;
        }
        self.status.suspended = self.status.pid.is_some_and(suspended);
        true
    }

    fn read_reports(&mut self) {
        let remaining = 8usize.saturating_sub(self.reports.len());
        let _ = (&self.owner)
            .take(remaining as u64)
            .read_to_end(&mut self.reports);
        if let Some(bytes) = self.reports.first_chunk() {
            self.status.pid = Some(u32::from_ne_bytes(*bytes));
        }
    }

    fn finish(&mut self, fallback: Option<ExitStatus>) {
        if let Some(drain) = self.drain.take() {
            self.stopped.store(true, Ordering::Release);
            let _ = drain.join();
            let status = self
                .reports
                .get(4..8)
                .and_then(|bytes| bytes.try_into().ok())
                .map(|bytes| ExitStatus::from_raw(i32::from_ne_bytes(bytes)))
                .or(fallback);
            let _ = output::finish(&self.log, status);
        }
    }

    pub(super) fn status(&self) -> &RunStatus {
        &self.status
    }

    pub(super) fn signal(&self, signal: RunSignal) -> Result<()> {
        let byte = match signal {
            RunSignal::Terminate => b't',
            RunSignal::Kill => b'k',
            RunSignal::Suspend => b's',
            RunSignal::Resume => b'r',
        };
        (&self.owner).write_all(&[byte])?;
        Ok(())
    }
}

pub(super) fn pending_bytes(source: &impl AsRawFd) -> usize {
    let mut pending: libc::c_int = 0;
    if unsafe { libc::ioctl(source.as_raw_fd(), libc::FIONREAD, &mut pending) } == 0 {
        pending.max(0) as usize
    } else {
        0
    }
}

pub(super) fn suspended(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(") ")
                .and_then(|(_, rest)| rest.as_bytes().first().copied())
        })
        .is_some_and(|state| matches!(state, b'T' | b't'))
}

impl Drop for ManagedRun {
    fn drop(&mut self) {
        // Closing the socket also stops the group if the TUI is killed or loses its terminal.
        let _ = self.owner.shutdown(std::net::Shutdown::Write);
        let status = self.child.wait().ok();
        self.read_reports();
        self.finish(status);
    }
}

pub fn child(request: &str) -> Result<()> {
    let request = match serde_json::from_str::<ChildRequest>(request)? {
        ChildRequest::Run(request) => request,
        ChildRequest::Debug { manifest, tool } => return super::run::debug_tool(manifest, &tool),
    };
    anyhow::ensure!(
        !(request.venv && request.venv_path.is_some()),
        "conflicting virtual environments"
    );
    let global = super::Global {
        verbose: request.verbosity > 0,
        manifest: Some(request.manifest.clone()),
        json: false,
        interaction: None,
        task: None,
        interactive: false,
    };
    super::run::managed(&global, request)
}

pub(super) fn supervise(mut command: Command) -> Result<ExitStatus> {
    let owner = UnixStream::from(io::stdin().as_fd().try_clone_to_owned()?);
    owner.set_nonblocking(true)?;
    // Reap orphaned members of the process group after the command leader exits.
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    let mut input = [0];
    let mut owner_ref = &owner;
    match owner_ref.read(&mut input) {
        Ok(0) => bail!("run cancelled before startup"),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
        Err(error) => return Err(error.into()),
    }
    let child = command.stdin(Stdio::null()).process_group(0).spawn()?;
    let mut group = ProcessGroup {
        child,
        reaped: false,
    };
    let pgid = group.child.id();
    (&owner).write_all(&pgid.to_ne_bytes())?;
    let mut poll = libc::pollfd {
        fd: owner.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let mut terminating = None::<Instant>;
    loop {
        // The ros2 wrapper can exit before its node finishes handling SIGTERM.
        if exited(&group.child)?
            && terminating.is_none_or(|sent| sent.elapsed() >= Duration::from_secs(2))
        {
            break;
        }
        match unsafe { libc::poll(&mut poll, 1, 100) } {
            0 => continue,
            -1 => {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            _ => {}
        }
        match owner_ref.read(&mut input) {
            Ok(0) => break,
            Ok(_) => match input[0] {
                b't' => {
                    group.signal(libc::SIGTERM);
                    group.signal(libc::SIGCONT);
                    terminating.get_or_insert_with(Instant::now);
                }
                b'k' => break,
                b's' => group.signal(libc::SIGSTOP),
                b'r' => group.signal(libc::SIGCONT),
                _ => {}
            },
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.into()),
        }
    }
    let status = group.finish()?;
    let _ = (&owner).write_all(&status.into_raw().to_ne_bytes());
    Ok(status)
}

pub(super) fn exited(child: &Child) -> io::Result<bool> {
    loop {
        let mut exit = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // Keep the leader unreaped until cleanup, so its ID cannot be reused.
        if unsafe {
            libc::waitid(
                libc::P_PID,
                child.id(),
                exit.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } == 0
        {
            return Ok(unsafe { exit.assume_init().si_pid() } != 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

struct ProcessGroup {
    child: Child,
    reaped: bool,
}

impl ProcessGroup {
    // shortcut: signals cover the Linux process group, use delegated cgroups if targets daemonize into separate groups.
    fn signal(&self, signal: libc::c_int) {
        unsafe { libc::kill(-(self.child.id() as libc::pid_t), signal) };
    }

    fn finish(&mut self) -> io::Result<ExitStatus> {
        self.signal(libc::SIGKILL);
        let status = self.child.wait()?;
        self.reaped = true;
        let group = -(self.child.id() as libc::pid_t);
        loop {
            let rc = unsafe { libc::waitpid(group, std::ptr::null_mut(), 0) };
            if rc == -1 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                break;
            }
        }
        Ok(status)
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.finish();
        }
    }
}
