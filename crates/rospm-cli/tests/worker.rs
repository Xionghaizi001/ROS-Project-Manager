//! Real CLI/server transport with mocked host commands; no ROS or sudo required.
mod common;
use common::*;
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::fs::PermissionsExt,
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Child, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Worker {
    root: PathBuf,
    runtime: PathBuf,
    child: Option<Child>,
}
impl Worker {
    fn new(name: &str) -> Self {
        Self::with_timeout(name, None)
    }
    fn with_timeout(name: &str, timeout: Option<u64>) -> Self {
        let root = fixture(name);
        fs::write(
            root.join("rospm.toml"),
            include_str!("../../rospm-core/tests/fixtures/minimal.toml"),
        )
        .unwrap();
        fs::create_dir(root.join("bin")).unwrap();
        for (name, script) in [
            ("bash", "#!/bin/sh\ncase \"$*\" in *'colcon build'*) echo started; echo $$ > started; if [ -f fast ]; then exit 0; fi; while [ ! -f fast ]; do sleep 0.05 & wait; done;; *) exec /bin/bash \"$@\";; esac\n"),
            ("dpkg-query", "#!/bin/sh\nprintf 'ros-jazzy-demo\\tinstalled\\t1.0\\n'\n"),
            ("sudo", "#!/bin/sh\necho unexpected-sudo >&2; exit 99\n"),
        ] {
            let path = root.join("bin").join(name);
            fs::write(&path, script).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let runtime = root.join("runtime");
        let mut this = Self {
            root,
            runtime,
            child: None,
        };
        this.start_with_timeout(timeout);
        this
    }
    fn start(&mut self) {
        self.start_with_timeout(None);
    }
    fn start_with_timeout(&mut self, timeout: Option<u64>) {
        let mut command = rospm();
        command.arg("worker");
        if let Some(timeout) = timeout {
            command.args(["--auto-exit-timeout", &timeout.to_string()]);
        }
        self.child = Some(
            command
                .current_dir(&self.root)
                .arg("--runtime")
                .arg(&self.runtime)
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        self.root.join("bin").display(),
                        std::env::var("PATH").unwrap()
                    ),
                )
                .env("ROSPM_NO_NETWORK", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                self.child.as_mut().unwrap().try_wait().unwrap().is_none(),
                "worker exited during startup"
            );
            if std::os::unix::net::UnixStream::connect(self.runtime.join("worker.sock")).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "startup timeout");
            thread::sleep(Duration::from_millis(20));
        }
    }
    fn connect(&self) -> BufReader<UnixStream> {
        let stream = UnixStream::connect(self.runtime.join("worker.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut stream = BufReader::new(stream);
        stream
            .get_mut()
            .write_all(b"{\"version\":1,\"type\":\"hello\",\"client\":\"test\"}\n")
            .unwrap();
        let mut reply = String::new();
        stream.read_line(&mut reply).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&reply).unwrap()["type"],
            "hello"
        );
        stream
    }
    fn wait_exit(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {
                assert!(status.success());
                assert!(!self.runtime.join("worker.sock").exists());
                self.child.take();
                return;
            }
            assert!(Instant::now() < deadline, "worker did not exit");
            thread::sleep(Duration::from_millis(20));
        }
    }
    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            unsafe {
                libc::kill(child.id() as i32, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_secs(8);
            while child.try_wait().unwrap().is_none() {
                if Instant::now() > deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
    fn command(&self, args: &[&str]) -> std::process::Command {
        let mut command = rospm();
        command
            .current_dir(&self.root)
            .args(args)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root.join("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("ROSPM_NO_NETWORK", "1");
        command
    }
    fn wait_started(&self) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(pid) = fs::read_to_string(self.root.join("ws/started")) {
                if !pid.trim().is_empty() {
                    return pid;
                }
            }
            assert!(Instant::now() < deadline, "build did not start");
            thread::sleep(Duration::from_millis(20));
        }
    }
    fn request(&self, args: &[&str]) -> std::process::Output {
        rospm()
            .current_dir(&self.root)
            .args(["task", "--runtime"])
            .arg(&self.runtime)
            .args(args)
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> Value {
        let output = self.request(args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        json(&output)
    }
    fn wait(&self, id: &str, state: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let snapshot = self.ok(&["status", id])["snapshot"].clone();
            if snapshot["state"] == state {
                return snapshot;
            }
            assert!(Instant::now() < deadline, "expected {state}: {snapshot}");
            thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn rebuild_removes_artifacts_without_removing_sources() {
    let worker = Worker::new("worker-rebuild");
    for path in ["build/cache", "install/cache"] {
        let path = worker.root.join("ws").join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "old artifact").unwrap();
    }
    fs::write(worker.root.join("ws/fast"), "").unwrap();
    worker.ok(&["submit", "rebuild", "--id", "rebuild"]);
    let snapshot = worker.wait("rebuild", "succeeded");
    assert_eq!(snapshot["result"]["success"], true);
    assert!(!worker.root.join("ws/build").exists());
    assert!(!worker.root.join("ws/install").exists());
    assert!(worker.root.join("ws/src/my_pkg/package.xml").exists());
    assert!(worker
        .ok(&["logs", "rebuild"])
        .to_string()
        .contains("started"));
}

#[test]
fn interactive_worker_build_preserves_clean_retry_confirmation() {
    let worker = Worker::new("worker-build-retry");
    fs::create_dir(worker.root.join("ws/build")).unwrap();
    fs::write(worker.root.join("ws/build/cache"), "stale").unwrap();
    fs::write(
        worker.root.join("bin/bash"),
        "#!/bin/sh\ncase \"$*\" in *'colcon build'*) if [ -d build ]; then echo first-failure >&2; exit 1; fi; echo retried;; *) exec /bin/bash \"$@\";; esac\n",
    )
    .unwrap();
    worker.ok(&["submit", "build", "--interactive", "--id", "retry"]);
    let step = worker.wait("retry", "waiting_confirmation")["step"].to_string();
    assert!(worker.root.join("ws/build/cache").exists());
    let logs = worker.ok(&["logs", "retry"]).to_string();
    assert!(logs.contains("first-failure") && logs.contains("clean_build"));
    worker.ok(&["confirm", "retry", "--step", &step, "approve"]);
    worker.wait("retry", "succeeded");
    assert!(!worker.root.join("ws/build").exists());
    assert!(worker.root.join("ws/src/my_pkg/package.xml").exists());
    assert!(worker
        .ok(&["logs", "retry"])
        .to_string()
        .contains("retried"));
}

#[test]
fn cli_build_cancel_reconnect_deduplicate_and_restart() {
    let mut worker = Worker::new("worker-build");
    worker.ok(&["submit", "build", "--id", "build-1"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !worker.root.join("ws/started").exists() {
        assert!(
            Instant::now() < deadline,
            "build did not start: {}",
            worker.ok(&["status", "build-1"])
        );
        thread::sleep(Duration::from_millis(20));
    }
    let pid: i32 = fs::read_to_string(worker.root.join("ws/started"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::getpgid(pid) }, pid);
    worker.ok(&["submit", "build", "--id", "build-1"]);
    worker.ok(&["cancel", "build-1"]);
    let cancelled = worker.wait("build-1", "cancelled");
    assert_eq!(cancelled["result"]["success"], false);
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert!(!worker.ok(&["logs", "build-1"])["entries"]
        .as_array()
        .unwrap()
        .is_empty());
    worker.stop();
    assert_eq!(worker.ok(&["status", "build-1"])["snapshot"], cancelled);
    assert!(!worker.ok(&["logs", "build-1"])["entries"]
        .as_array()
        .unwrap()
        .is_empty());
    worker.start();
    let _connection = worker.connect();
    assert_eq!(worker.ok(&["status", "build-1"])["snapshot"], cancelled);
    worker.ok(&["submit", "build", "--id", "build-1"]);
    assert_eq!(worker.ok(&["status", "build-1"])["snapshot"], cancelled);
    assert!(!worker
        .request(&["submit", "check", "--id", "build-1"])
        .status
        .success());
    fs::write(worker.root.join("ws/fast"), "").unwrap();
    worker.ok(&["submit", "build", "--id", "build-2"]);
    assert_eq!(
        worker.wait("build-2", "succeeded")["result"]["exit_code"],
        0
    );
    worker.ok(&["submit", "apply_locked", "--id", "missing-lock"]);
    assert!(worker.wait("missing-lock", "failed")["result"]["error"]
        .as_str()
        .unwrap()
        .contains("lock"));
}

#[test]
fn cli_confirmation_is_reviewable_and_requires_matching_step() {
    let worker = Worker::new("worker-confirm");
    worker.ok(&["submit", "uninstall", "--id", "remove"]);
    let waiting = worker.wait("remove", "waiting_confirmation");
    let blocked = worker.command(&["build"]).output().unwrap();
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("Workspace is busy"));
    let logs = worker.ok(&["logs", "remove"]);
    let review: Value = logs["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|e| serde_json::from_str::<Value>(e["text"].as_str().unwrap()).ok())
        .unwrap();
    assert_eq!(review["operation"]["kind"], "uninstall");
    assert!(review.to_string().contains("ros-jazzy-demo"));
    assert!(!worker
        .request(&["confirm", "remove", "--step", "999", "approve"])
        .status
        .success());
    assert_eq!(
        worker.ok(&["status", "remove"])["snapshot"]["state"],
        "waiting_confirmation"
    );
    let step = waiting["step"].as_u64().unwrap().to_string();
    worker.ok(&["confirm", "remove", "--step", &step, "reject"]);
    worker.wait("remove", "failed");
    assert!(!worker
        .request(&["confirm", "remove", "--step", &step, "approve"])
        .status
        .success());
}

#[test]
fn cli_excludes_second_worker_and_recovers_crashed_confirmation_without_replay() {
    let mut worker = Worker::new("worker-recovery");
    let second = rospm()
        .current_dir(&worker.root)
        .args(["worker", "--runtime"])
        .arg(worker.root.join("other-runtime"))
        .output()
        .unwrap();
    assert!(!second.status.success());
    worker.ok(&["submit", "uninstall", "--id", "crash"]);
    worker.wait("crash", "waiting_confirmation");
    let mut child = worker.child.take().unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    worker.start();
    worker.wait("crash", "unknown");
    worker.ok(&["submit", "uninstall", "--id", "crash"]);
    worker.wait("crash", "unknown");
}

#[test]
fn worker_excludes_sync_mutations_and_shared_workspace_but_not_other_projects() {
    let worker = Worker::new("worker-sync-mutex");
    worker.ok(&["submit", "build", "--id", "holding"]);
    let pid = worker.wait_started();
    for args in [
        vec!["build"],
        vec!["apply", "--build"],
        vec!["uninstall", "--yes"],
        vec!["deps", "install"],
        vec!["venv", "create"],
        vec!["lock"],
        vec!["init", "--force"],
    ] {
        let out = worker.command(&args).output().unwrap();
        assert!(!out.status.success(), "accepted {args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("Workspace is busy"),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(worker
        .command(&["build", "--dry-run"])
        .output()
        .unwrap()
        .status
        .success());
    assert_eq!(worker.wait_started(), pid);
    assert_eq!(
        worker.ok(&["status", "holding"])["snapshot"]["state"],
        "running"
    );

    let other = Worker::new("worker-independent");
    let _connection = other.connect();
    fs::write(other.root.join("ws/fast"), "").unwrap();
    assert!(other.command(&["build"]).output().unwrap().status.success());
    other.ok(&["submit", "build", "--id", "independent"]);
    other.wait("independent", "succeeded");

    // A different manifest/runtime still conflicts through a workspace alias.
    fs::rename(other.root.join("ws"), other.root.join("own-ws")).unwrap();
    std::os::unix::fs::symlink(worker.root.join("ws"), other.root.join("ws")).unwrap();
    let out = other.command(&["build"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Workspace is busy"));
    other.ok(&["submit", "build", "--id", "shared"]);
    assert!(other.wait("shared", "failed")["result"]["error"]
        .as_str()
        .unwrap()
        .contains("Workspace is busy"));
    assert_eq!(worker.wait_started(), pid);

    worker.ok(&["cancel", "holding"]);
    worker.wait("holding", "cancelled");
    fs::write(worker.root.join("ws/fast"), "").unwrap();
    assert!(other.command(&["build"]).output().unwrap().status.success());
}

#[test]
fn synchronous_build_makes_worker_wait_and_release_allows_progress() {
    let worker = Worker::new("sync-worker-mutex");
    let mut build = worker
        .command(&["build"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = worker.wait_started();
    worker.ok(&["submit", "build", "--id", "queued"]);
    worker.wait("queued", "pending");
    thread::sleep(Duration::from_millis(100));
    assert_eq!(
        worker.ok(&["status", "queued"])["snapshot"]["state"],
        "pending"
    );
    assert_eq!(worker.wait_started(), pid);
    fs::write(worker.root.join("ws/fast"), "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = build.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "sync build did not finish");
        thread::sleep(Duration::from_millis(20));
    }
    worker.wait("queued", "succeeded");
}

#[test]
fn flat_workspace_reuses_project_lease_without_self_conflict() {
    let worker = Worker::new("worker-flat-mutex");
    let manifest = fs::read_to_string(worker.root.join("rospm.toml"))
        .unwrap()
        .replace("path = \"ws\"", "path = \".\"");
    fs::write(worker.root.join("rospm.toml"), manifest).unwrap();
    fs::write(worker.root.join("fast"), "").unwrap();
    assert!(worker
        .command(&["build"])
        .output()
        .unwrap()
        .status
        .success());
    worker.ok(&["submit", "build", "--id", "flat"]);
    worker.wait("flat", "succeeded");
}

#[test]
fn read_only_run_waits_for_build_and_multiple_readers_share_workspace() {
    let mut worker = Worker::new("worker-readers");
    worker.stop();
    fs::write(
        worker.root.join("bin/bash"),
        "#!/bin/sh\ncase \"$*\" in *'colcon build'*) mkdir -p install; : > install/setup.bash; echo $$ > started; while [ ! -f fast ]; do sleep 0.05 & wait; done; mkdir -p install/my_pkg/lib/my_pkg; printf '#!/bin/sh\\n' > install/my_pkg/lib/my_pkg/talker; chmod +x install/my_pkg/lib/my_pkg/talker;; *) exec /bin/bash \"$@\";; esac\n",
    )
    .unwrap();
    let mut build = worker
        .command(&["build"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    worker.wait_started();

    let mut readers = (0..2)
        .map(|_| {
            worker
                .command(&["run", "--list"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();
    thread::sleep(Duration::from_millis(200));
    assert!(readers
        .iter_mut()
        .all(|reader| reader.try_wait().unwrap().is_none()));
    assert!(worker.root.join("ws/install/setup.bash").is_file());
    assert!(!worker
        .root
        .join("ws/install/my_pkg/lib/my_pkg/talker")
        .exists());

    fs::write(worker.root.join("ws/fast"), "").unwrap();
    assert!(build.wait().unwrap().success());
    for reader in readers {
        let output = reader.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("my_pkg/talker"));
    }
}

#[test]
fn idle_disconnect_times_out_and_one_remaining_client_keeps_worker_alive() {
    let mut worker = Worker::with_timeout("worker-idle-exit", Some(1));
    let first = worker.connect();
    let second = worker.connect();
    drop(first);
    thread::sleep(Duration::from_millis(1200));
    assert!(worker.child.as_mut().unwrap().try_wait().unwrap().is_none());
    drop(second);
    thread::sleep(Duration::from_millis(200));
    assert!(worker.child.as_mut().unwrap().try_wait().unwrap().is_none());
    worker.wait_exit();
}

#[test]
fn disconnected_tasks_finish_before_exit_including_failure() {
    for (name, fail) in [
        ("worker-exit-success", false),
        ("worker-exit-failure", true),
    ] {
        // Success must exit promptly even before the default timeout; failure outlives its timeout.
        let mut worker = Worker::with_timeout(name, if fail { Some(1) } else { None });
        if fail {
            fs::write(worker.root.join("bin/bash"), "#!/bin/sh\necho $$ > started\nwhile [ ! -f fast ]; do sleep 0.05; done\necho failed >&2\nexit 7\n").unwrap();
        }
        worker.ok(&["submit", "build", "--id", "task"]);
        worker.wait_started();
        thread::sleep(Duration::from_millis(1200));
        assert!(worker.child.as_mut().unwrap().try_wait().unwrap().is_none());
        fs::write(worker.root.join("ws/fast"), "").unwrap();
        worker.wait_exit();
        assert_eq!(
            worker.ok(&["status", "task"])["snapshot"]["state"],
            if fail { "failed" } else { "succeeded" }
        );
        assert!(!worker.ok(&["logs", "task"])["entries"]
            .as_array()
            .unwrap()
            .is_empty());
    }
}

#[test]
fn reconnect_clears_pending_exit_after_task_finishes() {
    let mut worker = Worker::with_timeout("worker-exit-reconnect", Some(1));
    worker.ok(&["submit", "build", "--id", "task"]);
    worker.wait_started();
    thread::sleep(Duration::from_millis(1200));
    let client = worker.connect();
    fs::write(worker.root.join("ws/fast"), "").unwrap();
    worker.wait("task", "succeeded");
    thread::sleep(Duration::from_millis(1200));
    assert!(worker.child.as_mut().unwrap().try_wait().unwrap().is_none());
    drop(client);
    thread::sleep(Duration::from_millis(200));
    assert!(worker.child.as_mut().unwrap().try_wait().unwrap().is_none());
    worker.wait_exit();
}

#[test]
fn waiting_confirmation_is_not_idle_and_disconnect_does_not_approve() {
    let mut worker = Worker::with_timeout("worker-exit-confirm", Some(1));
    worker.ok(&["submit", "uninstall", "--id", "task"]);
    worker.wait("task", "waiting_confirmation");
    thread::sleep(Duration::from_millis(1200));
    assert!(worker.child.as_mut().unwrap().try_wait().unwrap().is_none());
    let snapshot = worker.ok(&["status", "task"]);
    assert_eq!(snapshot["snapshot"]["state"], "waiting_confirmation");
    worker.ok(&["cancel", "task"]);
    worker.wait_exit();
    assert_eq!(
        worker.ok(&["status", "task"])["snapshot"]["state"],
        "cancelled"
    );
}

#[test]
fn idle_startup_without_hello_also_times_out() {
    let mut worker = Worker::with_timeout("worker-exit-no-hello", Some(1));
    let _unhandshaken = UnixStream::connect(worker.runtime.join("worker.sock")).unwrap();
    worker.wait_exit();
}

#[test]
fn synchronous_cli_detaches_and_preserves_arguments_logs_and_live_ownership() {
    let mut worker = Worker::new("sync-detach");
    worker.stop();
    fs::write(worker.root.join("bin/bash"), "#!/bin/sh\ncase \"$*\" in *'colcon build'*) printf '%s' \"$*\" > argv; echo $$ > started; echo out; echo err >&2; while [ ! -f fast ]; do sleep 0.05; done;; *) exec /bin/bash \"$@\";; esac\n").unwrap();
    let mut cli = worker
        .command(&["--json", "build", "--", "--packages-select", "my_pkg"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = worker.wait_started();
    let store =
        rospm_core::worker::TaskStore::open(worker.root.join(".rospm/worker/tasks")).unwrap();
    let id = fs::read_dir(worker.root.join(".rospm/worker/tasks"))
        .unwrap()
        .map(|entry| entry.unwrap())
        .find(|entry| entry.file_type().unwrap().is_dir())
        .unwrap()
        .file_name()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(fs::read_to_string(worker.root.join("ws/argv"))
        .unwrap()
        .contains("--packages-select my_pkg"));
    cli.kill().unwrap();
    cli.wait().unwrap();
    assert!(store
        .recover_active_tasks(rospm_core::worker::RecoveryCause::HostFailure)
        .unwrap()
        .is_empty());
    assert_eq!(
        store.load_snapshot(&id).unwrap().state,
        rospm_core::worker::TaskState::Running
    );
    assert_eq!(worker.wait_started(), pid);
    fs::write(worker.root.join("ws/fast"), "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = store.load_snapshot(&id).unwrap();
        if snapshot.state.is_terminal() {
            assert_eq!(snapshot.state, rospm_core::worker::TaskState::Succeeded);
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    let entries = store.read_logs(&id, 0, 256).unwrap();
    let stdout: String = entries
        .iter()
        .filter(|entry| entry.stream == rospm_core::worker::LogStream::Stdout)
        .map(|entry| entry.text.as_str())
        .collect();
    assert_eq!(
        serde_json::from_str::<Value>(&stdout).unwrap(),
        serde_json::json!({"ok": true})
    );
    let stderr: String = entries
        .iter()
        .filter(|entry| entry.stream == rospm_core::worker::LogStream::Stderr)
        .map(|entry| entry.text.as_str())
        .collect();
    assert!(stderr.contains("out") && stderr.contains("err"));
    let runtime = worker.root.join(".rospm/worker/runtime");
    while runtime.exists() {
        assert!(Instant::now() < deadline, "detached worker did not exit");
        thread::sleep(Duration::from_millis(20));
    }
    let out = worker
        .command(&[
            "task",
            "--runtime",
            runtime.to_str().unwrap(),
            "status",
            &id,
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(json(&out)["snapshot"]["state"], "succeeded");
}

#[test]
fn synchronous_confirmation_survives_disconnect_and_can_be_cancelled() {
    use std::os::fd::FromRawFd;
    let mut worker = Worker::new("sync-confirm-disconnect");
    worker.stop();
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
    let _master = unsafe { std::fs::File::from_raw_fd(master) };
    let slave = unsafe { std::fs::File::from_raw_fd(slave) };
    let mut cli = worker
        .command(&["uninstall", "--keep-source"])
        .stdin(Stdio::from(slave))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let store =
        rospm_core::worker::TaskStore::open(worker.root.join(".rospm/worker/tasks")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let id = loop {
        let waiting = fs::read_dir(worker.root.join(".rospm/worker/tasks"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .find_map(|entry| {
                let id = entry.file_name().to_str()?.to_owned();
                let snapshot = store.load_snapshot(&id).ok()?;
                (snapshot.state == rospm_core::worker::TaskState::WaitingConfirmation).then_some(id)
            });
        if let Some(id) = waiting {
            break id;
        }
        assert!(Instant::now() < deadline, "confirmation did not appear");
        thread::sleep(Duration::from_millis(20));
    };
    cli.kill().unwrap();
    cli.wait().unwrap();
    assert_eq!(
        store.load_snapshot(&id).unwrap().state,
        rospm_core::worker::TaskState::WaitingConfirmation
    );
    let entries = store.read_logs(&id, 0, 256).unwrap();
    let request: Value = entries
        .iter()
        .find_map(|entry| {
            let value: Value = serde_json::from_str(&entry.text).ok()?;
            (value["operation"]["kind"] == "uninstall").then_some(value)
        })
        .unwrap();
    assert_eq!(request["task"], id);
    assert!(request.to_string().contains("ros-jazzy-demo"));
    let runtime = worker
        .root
        .join(".rospm/worker/runtime")
        .to_string_lossy()
        .into_owned();
    let out = worker
        .command(&["task", "--runtime", &runtime, "cancel", &id])
        .output()
        .unwrap();
    assert!(out.status.success());
    loop {
        let snapshot = store.load_snapshot(&id).unwrap();
        if snapshot.state.is_terminal() {
            assert_eq!(snapshot.state, rospm_core::worker::TaskState::Cancelled);
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!store
        .read_logs(&id, 0, 256)
        .unwrap()
        .iter()
        .any(|entry| entry.text.contains("unexpected-sudo")));
}

#[test]
fn interactive_system_step_waits_and_attaches_to_a_foreground_terminal() {
    use std::os::{fd::FromRawFd, unix::process::CommandExt};

    let worker = Worker::new("worker-authorization");
    fs::write(
        worker.root.join("bin/sudo"),
        "#!/bin/sh\n[ \"$1\" = -- ] && [ \"$2\" = apt-get ] || exit 97\n         printf 'mock-authorization\\n' >/dev/tty || exit 98\n         printf 'system stdout\\n'; printf 'system stderr\\n' >&2\n",
    )
    .unwrap();
    worker.ok(&["submit", "uninstall", "--interactive", "--id", "auth"]);
    let step = worker.wait("auth", "waiting_confirmation")["step"].to_string();
    worker.ok(&["confirm", "auth", "--step", &step, "approve"]);
    worker.wait("auth", "waiting_authorization");
    assert!(!worker.request(&["attach", "auth"]).status.success());
    worker.wait("auth", "waiting_authorization");
    assert!(!worker
        .request(&["submit", "uninstall", "--id", "auth"])
        .status
        .success());

    let (mut master, mut slave) = (-1, -1);
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    let master_fd = master;
    let master = unsafe { fs::File::from_raw_fd(master) };
    let slave = unsafe { fs::File::from_raw_fd(slave) };
    let mut command = worker.command(&[
        "task",
        "--runtime",
        worker.runtime.to_str().unwrap(),
        "attach",
        "auth",
    ]);
    command.stdin(Stdio::from(slave));
    unsafe {
        command.pre_exec(move || {
            libc::close(master_fd);
            if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    worker.wait("auth", "succeeded");
    let mut tty = BufReader::new(master);
    let mut prompt = String::new();
    tty.read_line(&mut prompt).unwrap();
    assert_eq!(prompt.trim(), "mock-authorization");
    let logs = worker.ok(&["logs", "auth"]).to_string();
    assert!(logs.contains("system stdout") && logs.contains("system stderr"));
    assert!(!logs.contains("mock-authorization"));
}
