//! Real CLI/server transport with mocked host commands; no ROS or sudo required.
mod common;
use common::*;
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
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
        this.start();
        this
    }
    fn start(&mut self) {
        self.child = Some(
            rospm()
                .current_dir(&self.root)
                .arg("worker")
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
        vec!["run", "--interactive"],
        vec!["shell"],
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
