//! Exercise owner disconnect and the Linux process group without a ROS installation.
mod common;

use common::{fixture, rospm};
use rospm_core::run::{RunRequest, TargetKind};
use std::{
    fs,
    io::Read,
    os::{
        fd::OwnedFd,
        unix::{fs::PermissionsExt, net::UnixStream},
    },
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

#[test]
fn disconnect_kills_a_suspended_run_and_its_children() {
    let root = fixture("run-owner-disconnect").canonicalize().unwrap();
    let manifest = root.join("rospm.toml");
    fs::write(
        &manifest,
        include_str!("../../rospm-core/tests/fixtures/minimal.toml"),
    )
    .unwrap();
    let node = root.join("ws/install/my_pkg/lib/my_pkg/talker");
    fs::create_dir_all(node.parent().unwrap()).unwrap();
    fs::write(root.join("ws/install/setup.bash"), "").unwrap();
    fs::write(&node, "").unwrap();
    fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).unwrap();
    let bash = root.join("bash");
    fs::write(
        &bash,
        "#!/bin/sh\n/bin/sleep 60 &\necho $! > descendant\nwait\n",
    )
    .unwrap();
    fs::set_permissions(bash, fs::Permissions::from_mode(0o755)).unwrap();
    let request = RunRequest {
        manifest,
        kind: TargetKind::Node,
        package: "my_pkg".into(),
        name: "talker".into(),
        args: Vec::new(),
        venv: false,
        venv_path: None,
        verbosity: 0,
    };
    let (mut owner, supervisor) = UnixStream::pair().unwrap();
    owner
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut child = rospm()
        .arg("run-child")
        .arg(serde_json::to_string(&request).unwrap())
        .env("PATH", &root)
        .stdin(Stdio::from(OwnedFd::from(supervisor)))
        .current_dir(&root)
        .spawn()
        .unwrap();
    let mut bytes = [0; 4];
    owner.read_exact(&mut bytes).unwrap();
    let leader = u32::from_ne_bytes(bytes);
    let descendant = wait_for(|| {
        fs::read_to_string(root.join("descendant"))
            .ok()?
            .trim()
            .parse::<u32>()
            .ok()
    });
    use std::io::Write;
    owner.write_all(b"s").unwrap();
    wait_for(|| (state(leader) == Some(b'T') && state(descendant) == Some(b'T')).then_some(()));
    drop(owner);
    wait_for(|| child.try_wait().unwrap());
    assert!(state(leader).is_none());
    assert!(state(descendant).is_none());
    fs::remove_dir_all(root).unwrap();
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
