//! An interactive PTY whose shell and jobs are supervised by a TUI-owned worker.
use std::{
    collections::VecDeque,
    fs::{self, File},
    io::{self, Read, Write},
    os::{
        fd::{AsFd, AsRawFd, FromRawFd, OwnedFd},
        unix::{
            net::UnixStream,
            process::{CommandExt, ExitStatusExt},
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result};
use rospm_core::run::{output, RunSignal, RunStatus};

use super::{environment, Global};

// shortcut: keep only the detached output tail, add scrollback storage if sessions need history.
const BUFFER_BYTES: usize = 64 * 1024;

pub(super) struct Session {
    worker: Child,
    owner: UnixStream,
    master: File,
    output: Arc<Mutex<Output>>,
    reader: Option<JoinHandle<()>>,
    status: RunStatus,
    log: File,
    exit: Option<ExitStatus>,
}

#[derive(Default)]
struct Output {
    attached: bool,
    stop: bool,
    pending: VecDeque<u8>,
}

impl Session {
    pub(super) fn start(root: &Path, manifest: &Path, log: File) -> Result<Self> {
        let window = terminal_size()?;
        let (mut master, mut slave) = (-1, -1);
        if unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                &window,
            )
        } == -1
        {
            return Err(io::Error::last_os_error().into());
        }
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        for file in [&master, &slave] {
            if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
                return Err(io::Error::last_os_error().into());
            }
        }

        let (owner, worker) = UnixStream::pair()?;
        let worker = Command::new(std::env::current_exe()?)
            .arg("--manifest")
            .arg(std::env::current_dir()?.join(manifest))
            .arg("shell-worker")
            .env("ROSPM_LANG", crate::i18n::current().code())
            .current_dir(root)
            .process_group(0)
            .stdin(Stdio::from(OwnedFd::from(worker)))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave))
            .spawn()?;
        let mut session = Self {
            worker,
            owner,
            master,
            output: Default::default(),
            reader: None,
            status: RunStatus {
                id: format!("shell-{}", super::interaction::task_id()),
                target: "Shell".into(),
                background: false,
                task: false,
                shell: true,
                pid: None,
                suspended: false,
                output_fd: Some(log.as_raw_fd()),
                exit: None,
            },
            log,
            exit: None,
        };
        let master = session.master.try_clone()?;
        let output = Arc::clone(&session.output);
        let transcript = output::Writer::new(session.log.try_clone()?);
        session.reader =
            Some(thread::Builder::new().spawn(move || read_output(master, output, transcript))?);
        Ok(session)
    }

    fn receive_pid(&mut self) -> Result<()> {
        self.owner.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut pid = [0; 4];
        if let Err(error) = self.owner.read_exact(&mut pid) {
            self.finish_reader();
            let output = self.output.lock().unwrap();
            let diagnostic = String::from_utf8_lossy(output.pending.as_slices().0);
            return Err(error).with_context(|| {
                format!(
                    "{} {}",
                    crate::i18n::tr("cli.shell.start_failed"),
                    diagnostic.trim()
                )
            });
        }
        self.owner.set_read_timeout(None)?;
        self.status.pid = Some(u32::from_ne_bytes(pid));
        Ok(())
    }

    /// Returns whether the shell was detached and can be reattached.
    pub(super) fn attach(&mut self) -> Result<bool> {
        if self.status.pid.is_none() {
            self.receive_pid()?;
        }
        // The outer terminal is already raw. Only the PTY slave runs Bash's line discipline.
        let stdin = io::stdin();
        let mut input = File::from(stdin.as_fd().try_clone_to_owned()?);
        {
            let mut output = self.output.lock().unwrap();
            output.flush()?;
            output.attached = true;
        }
        let result = self.relay(&mut input);
        self.output.lock().unwrap().attached = false;
        if matches!(result, Ok(false)) {
            self.finish_reader();
            self.output.lock().unwrap().flush()?;
        }
        result
    }

    fn relay(&mut self, input: &mut File) -> Result<bool> {
        if self.status.pid.is_some_and(super::run_process::suspended) {
            self.signal(RunSignal::Resume)?;
        }
        let mut fds = [poll_input(&self.owner), poll_input(input)];
        let mut bytes = [0; 4096];
        loop {
            let window = terminal_size()?;
            if unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &window) } == -1 {
                return Err(io::Error::last_os_error().into());
            }
            poll(&mut fds, 150)?;
            if fds[0].revents != 0 {
                return self.read_event().map_err(Into::into);
            }
            if fds[1].revents != 0 {
                let count = input.read(&mut bytes)?;
                if count == 0 {
                    return Ok(false);
                }
                match self.master.write_all(&bytes[..count]) {
                    Err(error) if error.raw_os_error() == Some(libc::EIO) => return Ok(false),
                    result => result?,
                }
            }
        }
    }

    pub(super) fn refresh(&mut self) -> Option<RunStatus> {
        if self.worker.try_wait().ok()?.is_some() {
            return None;
        }
        self.status.suspended = self.status.pid.is_some_and(super::run_process::suspended);
        Some(self.status.clone())
    }

    pub(super) fn status(&self) -> &RunStatus {
        &self.status
    }

    fn read_event(&mut self) -> io::Result<bool> {
        let mut byte = [0];
        match (&self.owner).read(&mut byte)? {
            1 if byte[0] == b'b' => Ok(true),
            1 if byte[0] == b'e' => {
                let mut raw = [0; 4];
                (&self.owner).read_exact(&mut raw)?;
                self.exit = Some(ExitStatus::from_raw(i32::from_ne_bytes(raw)));
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    pub(super) fn id(&self) -> &str {
        &self.status.id
    }

    pub(super) fn signal(&self, signal: RunSignal) -> Result<()> {
        (&self.owner).write_all(&[match signal {
            RunSignal::Terminate => b't',
            RunSignal::Kill => b'k',
            RunSignal::Suspend => b's',
            RunSignal::Resume => b'r',
        }])?;
        Ok(())
    }

    fn finish_reader(&mut self) {
        if let Some(reader) = self.reader.take() {
            self.output.lock().unwrap().stop = true;
            let _ = reader.join();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.owner.shutdown(std::net::Shutdown::Write);
        let status = self.worker.wait().ok();
        if self.exit.is_none() && self.status.pid.is_some() {
            while matches!(self.read_event(), Ok(true)) {}
        }
        self.finish_reader();
        let _ = output::finish(&self.log, self.exit.or(status.filter(|s| !s.success())));
    }
}

impl Output {
    fn flush(&mut self) -> io::Result<()> {
        let mut stdout = io::stdout().lock();
        let (first, second) = self.pending.as_slices();
        stdout.write_all(first)?;
        stdout.write_all(second)?;
        stdout.flush()?;
        self.pending.clear();
        Ok(())
    }
}

fn read_output(mut master: File, output: Arc<Mutex<Output>>, mut transcript: output::Writer) {
    let mut fds = [poll_input(&master)];
    let mut bytes = [0; 8192];
    let mut remaining = None::<usize>;
    while poll(&mut fds, 150).is_ok() {
        if remaining.is_none() && output.lock().unwrap().stop {
            remaining = Some(super::run_process::pending_bytes(&master));
        }
        let limit = remaining.unwrap_or(bytes.len()).min(bytes.len());
        if limit == 0 {
            break;
        }
        if fds[0].revents == 0 {
            if remaining.is_some() {
                break;
            }
            continue;
        }
        let count = match master.read(&mut bytes[..limit]) {
            Ok(0) | Err(_) => break,
            Ok(count) => count,
        };
        let _ = transcript.append(&bytes[..count]);
        let mut output = output.lock().unwrap();
        output.pending.extend(&bytes[..count]);
        let excess = output.pending.len().saturating_sub(BUFFER_BYTES);
        output.pending.drain(..excess);
        if output.attached && output.flush().is_err() {
            break;
        }
        remaining = remaining.map(|size| size - count);
    }
}

fn terminal_size() -> io::Result<libc::winsize> {
    let mut size = std::mem::MaybeUninit::uninit();
    if unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, size.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { size.assume_init() })
}

fn poll_input(fd: &impl AsRawFd) -> libc::pollfd {
    libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    }
}

fn poll(fds: &mut [libc::pollfd], timeout: libc::c_int) -> io::Result<()> {
    loop {
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, timeout) } >= 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

static SHELL_EVENT: AtomicU8 = AtomicU8::new(0);
const DETACH: u8 = 1;
const EXIT: u8 = 2;

extern "C" fn shell_event(signal: libc::c_int) {
    SHELL_EVENT.fetch_or(
        if signal == libc::SIGUSR1 {
            EXIT
        } else {
            DETACH
        },
        Ordering::Release,
    );
}

pub fn serve(global: &Global) -> Result<()> {
    let mut owner = UnixStream::from(io::stdin().as_fd().try_clone_to_owned()?);
    let loaded = super::load(global)?;
    let leases = loaded.read_leases(false)?;
    let (_env_file, env_path) = anonymous_rc(&environment::shell_environment(&loaded)?)?;
    let mut rc = environment::shell_rc(&loaded, &env_path);
    rc.push_str(&format!(
        r#"unalias rospm 2>/dev/null || :
rospm() {{
    if [[ $# == 1 ]]; then
        case $1 in
            exit) builtin kill -USR1 {pid}; return ;;
            bakground) builtin kill -USR2 {pid}; return ;;
        esac
    fi
    command rospm "$@"
}}
export -nf rospm
printf '%s\n' {help}
"#,
        pid = std::process::id(),
        help = rospm_core::plan::shell_quote(&crate::i18n::tr("cli.shell.tui_help")),
    ));
    let (_rc_file, rc_path) = anonymous_rc(&rc)?;
    drop(leases);

    unsafe {
        libc::signal(
            libc::SIGUSR1,
            shell_event as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGUSR2,
            shell_event as *const () as libc::sighandler_t,
        );
        if libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) == -1 {
            return Err(io::Error::last_os_error().into());
        }
    }
    let mut fds = [poll_input(&owner)];
    poll(&mut fds, 0)?;
    if fds[0].revents != 0 {
        return Ok(());
    }

    let mut bash = Command::new("bash");
    bash.arg("--rcfile")
        .arg(rc_path)
        .arg("-i")
        .stdin(Stdio::from(io::stdout().as_fd().try_clone_to_owned()?))
        .current_dir(&loaded.root);
    unsafe {
        bash.pre_exec(|| {
            if libc::setsid() == -1 || libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut shell = ShellProcess {
        child: bash
            .spawn()
            .context(crate::i18n::tr("cli.shell.start_failed"))?,
        reaped: false,
    };
    owner.write_all(&shell.child.id().to_ne_bytes())?;

    loop {
        let event = SHELL_EVENT.swap(0, Ordering::AcqRel);
        if event & EXIT != 0 || super::run_process::exited(&shell.child)? {
            break;
        }
        if event & DETACH != 0 {
            owner.write_all(b"b")?;
        }
        poll(&mut fds, 100)?;
        if fds[0].revents == 0 {
            continue;
        }
        let mut byte = [0];
        if owner.read(&mut byte)? == 0 || byte[0] == b'k' {
            break;
        }
        let signal = match byte[0] {
            b't' => libc::SIGTERM,
            b's' => libc::SIGSTOP,
            b'r' => libc::SIGCONT,
            _ => continue,
        };
        shell.signal(signal);
        if signal == libc::SIGTERM {
            shell.signal(libc::SIGCONT);
        }
    }
    let status = shell.finish()?;
    let mut report = [b'e'; 5];
    report[1..].copy_from_slice(&status.into_raw().to_ne_bytes());
    let _ = owner.write_all(&report);
    Ok(())
}

fn anonymous_rc(script: &str) -> io::Result<(File, PathBuf)> {
    let fd = unsafe { libc::memfd_create(c"rospm-shell".as_ptr(), libc::MFD_CLOEXEC) };
    if fd == -1 {
        return Err(io::Error::last_os_error());
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(script.as_bytes())?;
    Ok((
        file,
        PathBuf::from(format!("/proc/{}/fd/{fd}", std::process::id())),
    ))
}

struct ShellProcess {
    child: Child,
    reaped: bool,
}

impl ShellProcess {
    // Bash places jobs in separate process groups within its terminal session.
    fn signal(&self, signal: libc::c_int) {
        let sid = self.child.id() as libc::pid_t;
        let Ok(processes) = fs::read_dir("/proc") else {
            return;
        };
        for entry in processes.flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<libc::pid_t>() else {
                continue;
            };
            if unsafe { libc::getsid(pid) } != sid {
                continue;
            }
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
            if fd >= 0 {
                let fd = unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) };
                if unsafe { libc::getsid(pid) } == sid {
                    unsafe {
                        libc::syscall(
                            libc::SYS_pidfd_send_signal,
                            fd.as_raw_fd(),
                            signal,
                            std::ptr::null::<libc::siginfo_t>(),
                            0,
                        );
                    }
                }
            }
        }
    }
    fn finish(&mut self) -> io::Result<ExitStatus> {
        // Keep the session leader PID reserved while stopping jobs and collecting races with fork.
        self.signal(libc::SIGSTOP);
        self.signal(libc::SIGKILL);
        let _ = self.child.kill();
        let status = self.child.wait()?;
        self.reaped = true;
        while unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) } > 0 {}
        Ok(status)
    }
}

impl Drop for ShellProcess {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.finish();
        }
    }
}
