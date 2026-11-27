//! Exercise process termination and owner disconnect without a ROS installation.
mod common;

use common::{fixture, rospm};
use rospm_core::run::{RunRequest, TargetKind};
use std::{
    fs::{self, File},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{fs::PermissionsExt, net::UnixStream, process::ExitStatusExt},
    },
    path::PathBuf,
    process::{Child, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

struct ManagedProcess {
    child: Child,
    owner: UnixStream,
    root: PathBuf,
    terminal: Option<File>,
    leader: u32,
}

impl ManagedProcess {
    fn run(name: &str, script: &str) -> Self {
        let root = project(name, script);
        let node = root.join("ws/install/my_pkg/lib/my_pkg/talker");
        fs::create_dir_all(node.parent().unwrap()).unwrap();
        fs::write(root.join("ws/install/setup.bash"), "").unwrap();
        fs::write(&node, "").unwrap();
        fs::set_permissions(node, fs::Permissions::from_mode(0o755)).unwrap();
        let request = RunRequest {
            manifest: root.join("rospm.toml"),
            kind: TargetKind::Node,
            package: "my_pkg".into(),
            name: "talker".into(),
            args: Vec::new(),
            venv: false,
            venv_path: None,
            verbosity: 0,
        };
        let mut command = rospm();
        command
            .arg("run-child")
            .arg(serde_json::to_string(&request).unwrap());
        Self::start(root, command, None)
    }

    fn shell(name: &str) -> Self {
        // Use a real interactive Bash without sourcing a ROS installation or user startup files.
        let root = project(name, "#!/bin/sh\nexec /bin/bash --noprofile --norc -i\n");
        let (mut master, mut slave) = (-1, -1);
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                )
            },
            0
        );
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        for file in [&master, &slave] {
            assert_eq!(
                unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) },
                0
            );
        }
        let mut command = rospm();
        command
            .arg("--manifest")
            .arg(root.join("rospm.toml"))
            .arg("shell-worker")
            .env("ROSPM_SHELL_COMMAND", root.join("last-command"))
            .env("HISTFILE", "/dev/null")
            .env_remove("PROMPT_COMMAND")
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        Self::start(root, command, Some(master))
    }

    fn start(root: PathBuf, mut command: std::process::Command, terminal: Option<File>) -> Self {
        let (owner, supervisor) = UnixStream::pair().unwrap();
        owner
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let child = command
            .env("PATH", &root)
            .stdin(Stdio::from(OwnedFd::from(supervisor)))
            .current_dir(&root)
            .spawn()
            .unwrap();
        let mut process = Self {
            child,
            owner,
            root,
            terminal,
            leader: 0,
        };
        let mut bytes = [0; 4];
        process.owner.read_exact(&mut bytes).unwrap();
        process.leader = u32::from_ne_bytes(bytes);
        process
    }

    fn wait_for_file(&self, name: &str) -> String {
        wait_for(|| {
            fs::read_to_string(self.root.join(name))
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
    }

    fn terminate(&mut self) -> ExitStatus {
        self.owner.write_all(b"t").unwrap();
        // Keep the owner connected: disconnecting would hide a broken terminate request.
        wait_for(|| self.child.try_wait().unwrap());
        if self.terminal.is_some() {
            let mut event = [0];
            self.owner.read_exact(&mut event).unwrap();
            assert_eq!(event, [b'e']);
        }
        let mut bytes = [0; 4];
        self.owner.read_exact(&mut bytes).unwrap();
        ExitStatus::from_raw(i32::from_ne_bytes(bytes))
    }
}

impl Drop for ManagedProcess {
    fn drop(&mut self) {
        let _ = self.owner.shutdown(std::net::Shutdown::Write);
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn project(name: &str, script: &str) -> PathBuf {
    let root = fixture(name).canonicalize().unwrap();
    fs::write(
        root.join("rospm.toml"),
        include_str!("../../rospm-core/tests/fixtures/minimal.toml"),
    )
    .unwrap();
    fs::write(root.join("bash"), script).unwrap();
    fs::set_permissions(root.join("bash"), fs::Permissions::from_mode(0o755)).unwrap();
    root
}

#[test]
fn terminate_escalates_when_the_run_and_its_child_ignore_sigterm() {
    let mut process = ManagedProcess::run(
        "run-terminate-ignored",
        "#!/bin/sh\ntrap '' TERM\n/bin/sleep 60 &\necho $! > descendant\nwait\n",
    );
    let descendant = process.wait_for_file("descendant").trim().parse().unwrap();
    assert_eq!(process.terminate().signal(), Some(libc::SIGKILL));
    assert!(state(process.leader).is_none());
    assert!(state(descendant).is_none());
}

#[test]
fn terminate_preserves_a_graceful_exit() {
    let mut process = ManagedProcess::run(
        "run-terminate-graceful",
        "#!/bin/sh\ntrap 'echo done > cleanup; exit 23' TERM\necho ready > ready\nwhile :; do /bin/sleep 0.1; done\n",
    );
    process.wait_for_file("ready");
    assert_eq!(process.terminate().code(), Some(23));
    assert_eq!(process.wait_for_file("cleanup").trim(), "done");
}

#[test]
fn terminate_allows_child_cleanup_after_the_run_leader_exits() {
    let mut process = ManagedProcess::run(
        "run-terminate-child-cleanup",
        "#!/bin/sh\n/bin/sh -c 'trap \"/bin/sleep 0.2; echo done > cleanup; exit 0\" TERM; echo ready > ready; while :; do /bin/sleep 0.1; done' &\necho $! > descendant\nwait\n",
    );
    process.wait_for_file("ready");
    let descendant = process.wait_for_file("descendant").trim().parse().unwrap();
    assert_eq!(process.terminate().signal(), Some(libc::SIGTERM));
    assert_eq!(process.wait_for_file("cleanup").trim(), "done");
    assert!(state(descendant).is_none());
}

#[test]
fn terminate_closes_an_idle_interactive_shell() {
    let mut process = ManagedProcess::shell("shell-terminate-idle");
    process
        .terminal
        .as_mut()
        .unwrap()
        .write_all(b"echo ready > ready\n")
        .unwrap();
    process.wait_for_file("ready");
    assert_eq!(process.terminate().signal(), Some(libc::SIGKILL));
    assert!(state(process.leader).is_none());
}

#[test]
fn terminate_closes_a_suspended_shell_and_its_jobs() {
    let mut process = ManagedProcess::shell("shell-terminate-suspended");
    process
        .terminal
        .as_mut()
        .unwrap()
        .write_all(b"trap '' TERM; /bin/sleep 60 & echo $! > descendant\n")
        .unwrap();
    let descendant = process.wait_for_file("descendant").trim().parse().unwrap();
    process.owner.write_all(b"s").unwrap();
    wait_for(|| {
        (state(process.leader) == Some(b'T') && state(descendant) == Some(b'T')).then_some(())
    });
    assert_eq!(process.terminate().signal(), Some(libc::SIGKILL));
    assert!(state(process.leader).is_none());
    wait_for(|| state(descendant).is_none().then_some(()));
}

#[test]
fn disconnect_kills_a_suspended_run_and_its_children() {
    let mut process = ManagedProcess::run(
        "run-owner-disconnect",
        "#!/bin/sh\n/bin/sleep 60 &\necho $! > descendant\nwait\n",
    );
    let descendant = process.wait_for_file("descendant").trim().parse().unwrap();
    process.owner.write_all(b"s").unwrap();
    wait_for(|| {
        (state(process.leader) == Some(b'T') && state(descendant) == Some(b'T')).then_some(())
    });
    process.owner.shutdown(std::net::Shutdown::Write).unwrap();
    wait_for(|| process.child.try_wait().unwrap());
    assert!(state(process.leader).is_none());
    assert!(state(descendant).is_none());
}

fn state(pid: u32) -> Option<u8> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(") ")?.1.bytes().next()
}

fn wait_for<T>(mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "run lifecycle timed out");
        thread::sleep(Duration::from_millis(20));
    }
}
