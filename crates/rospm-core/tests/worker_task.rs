use rospm_core::plan::{Command, Plan};
use rospm_core::worker::{TaskRunner, TaskState, TaskStore};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

fn fixture() -> (PathBuf, TaskStore, PathBuf) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "rospm-worker-task-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let store = TaskStore::open(root.join("tasks")).unwrap();
    (root, store, workspace)
}

#[test]
fn task_holds_workspace_lease_until_plan_finishes() {
    let (root, store, workspace) = fixture();
    let runner = TaskRunner::new(store.clone(), root.join("runtime"));
    let mut first_plan = Plan::new("first".to_string());
    first_plan.run(Command::new("sh").args(["-c", "sleep 0.35"]));
    let first = runner.start("first", &workspace, first_plan).unwrap();
    thread::sleep(Duration::from_millis(50));
    let mut second_plan = Plan::new("second".to_string());
    second_plan.run(Command::new("true"));
    let second = runner.start("second", &workspace, second_plan).unwrap();
    let started = Instant::now();
    thread::spawn(move || second.join().unwrap())
        .join()
        .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(200));
    first.join().unwrap();
    assert_eq!(
        store.load_snapshot("first").unwrap().state,
        TaskState::Succeeded
    );
    assert_eq!(
        store.load_snapshot("second").unwrap().state,
        TaskState::Succeeded
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cancellation_is_observed_at_step_boundary_and_stops_following_steps() {
    let (root, store, workspace) = fixture();
    let runner = TaskRunner::new(store.clone(), root.join("runtime"));
    let marker = workspace.join("must-not-exist");
    let mut plan = Plan::new("cancel".to_string());
    plan.run(Command::new("sh").args(["-c", "sleep 0.3"]));
    plan.run(Command::new("touch").arg(marker.to_string_lossy()));
    let task = runner.start("cancel", &workspace, plan).unwrap();
    thread::sleep(Duration::from_millis(50));
    task.cancel();
    task.join().unwrap_err();
    assert!(!marker.exists());
    assert_eq!(
        store.load_snapshot("cancel").unwrap().state,
        TaskState::Cancelled
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn confirmation_response_reaches_worker_operation_before_execution() {
    let (root, store, workspace) = fixture();
    let runner = TaskRunner::new(store.clone(), root.join("runtime"));
    let mut plan = Plan::new("confirm".to_string());
    plan.run(Command::new("true"));
    let executed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let executed_in_task = executed.clone();
    let task = runner
        .start_with("confirm", &workspace, move |interaction| {
            rospm_core::confirmation::uninstall("confirm", &plan, false, interaction, |_| {
                executed_in_task.store(true, Ordering::Release);
                Ok(())
            })
        })
        .unwrap();
    for _ in 0..50 {
        if store.load_snapshot("confirm").unwrap().state == TaskState::WaitingConfirmation {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        store.load_snapshot("confirm").unwrap().state,
        TaskState::WaitingConfirmation
    );
    task.respond(rospm_core::worker::ConfirmationResponse::Approve);
    task.join().unwrap();
    assert!(executed.load(Ordering::Acquire));
    assert_eq!(
        store.load_snapshot("confirm").unwrap().state,
        TaskState::Succeeded
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn group_cancel_stops_descendants_and_drains_output() {
    for stubborn in [false, true] {
        let (root, store, workspace) = fixture();
        let runner = TaskRunner::new(store.clone(), root.join("runtime"));
        let ready = workspace.join("ready");
        let mut plan = Plan::new("group cancel".to_string());
        let script = if stubborn {
            "trap '' TERM; sleep 30 & echo $$ > ready; wait"
        } else {
            "sleep 30 & echo $$ > ready; wait"
        };
        plan.run(
            Command::new("sh")
                .args(["-c", script])
                .cwd(&workspace)
                .interruptible(),
        );
        let task = runner.start("group", &workspace, plan).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(ready.exists());
        let pid: i32 = fs::read_to_string(ready).unwrap().trim().parse().unwrap();
        assert_eq!(unsafe { libc::getpgid(pid) }, pid);
        assert_ne!(pid, unsafe { libc::getpgrp() });
        let started = Instant::now();
        task.cancel();
        assert!(matches!(task.join(), Err(rospm_core::Error::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(6));
        if stubborn {
            assert!(started.elapsed() >= Duration::from_secs(2));
        }
        assert_eq!(
            store.load_snapshot("group").unwrap().state,
            TaskState::Cancelled
        );
        // The leader was reaped; descendants closed inherited pipes before join returned.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn protected_step_finishes_before_cancel_and_real_failure_wins() {
    for fail in [false, true] {
        let (root, store, workspace) = fixture();
        let runner = TaskRunner::new(store.clone(), root.join("runtime"));
        let mut plan = Plan::new("protected".to_string());
        plan.run(
            Command::new("sh")
                .args([
                    "-c",
                    if fail {
                        "echo ready > ready; sleep 0.2; exit 7"
                    } else {
                        "echo ready > ready; sleep 0.2; touch finished"
                    },
                ])
                .cwd(&workspace),
        );
        let task = runner.start("protected", &workspace, plan).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !workspace.join("ready").exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(workspace.join("ready").exists());
        task.cancel();
        let result = task.join();
        if fail {
            assert!(matches!(
                result,
                Err(rospm_core::Error::CommandFailed { .. })
            ));
            assert_eq!(
                store.load_snapshot("protected").unwrap().state,
                TaskState::Failed
            );
        } else {
            assert!(matches!(result, Err(rospm_core::Error::Cancelled)));
            assert!(workspace.join("finished").exists());
        }
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn queued_cancel_does_not_wait_for_workspace_lease() {
    let (root, store, workspace) = fixture();
    let runtime = root.join("runtime");
    let _lease = rospm_core::worker::WorkspaceLock::acquire(&runtime, &workspace).unwrap();
    let runner = TaskRunner::new(store.clone(), runtime);
    let task = runner
        .start_with("queued", &workspace, |_| panic!("cancelled task ran"))
        .unwrap();
    task.cancel();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !task.is_finished() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    assert!(matches!(task.join(), Err(rospm_core::Error::Cancelled)));
    let snapshot = store.load_snapshot("queued").unwrap();
    assert_eq!(snapshot.state, TaskState::Cancelled);
    assert!(!snapshot.result.unwrap().success);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn terminal_result_belongs_to_outer_workflow_and_panics_are_persisted() {
    use rospm_core::execution::{Event, State};
    let (root, store, workspace) = fixture();
    let runner = TaskRunner::new(store.clone(), root.join("runtime"));
    let observed = store.clone();
    let task = runner
        .start_with("outer", &workspace, move |interaction| {
            interaction.event(Event::Task {
                task: "outer",
                state: State::Succeeded,
                total: 1,
                error: None,
            });
            assert_eq!(
                observed.load_snapshot("outer").unwrap().state,
                TaskState::Running
            );
            panic!("workflow failed after its child plan");
        })
        .unwrap();
    assert!(task.join().is_err());
    let snapshot = store.load_snapshot("outer").unwrap();
    assert_eq!(snapshot.state, TaskState::Failed);
    assert!(snapshot.result.unwrap().error.unwrap().contains("panicked"));
    assert!(runner.start_with("outer", &workspace, |_| Ok(())).is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn log_persistence_failure_is_not_reported_as_success() {
    use rospm_core::execution::{Event, Stream};
    let (root, store, workspace) = fixture();
    let runner = TaskRunner::new(store.clone(), root.join("runtime"));
    let task = runner
        .start_with("bad-log", &workspace, |interaction| {
            let bytes = vec![b'x'; rospm_core::worker::MAX_FRAME_BYTES + 1];
            interaction.event(Event::Log {
                task: "bad-log",
                step: 0,
                stream: Stream::Stdout,
                bytes: &bytes,
            });
            Ok(())
        })
        .unwrap();
    assert!(task.join().is_err());
    let snapshot = store.load_snapshot("bad-log").unwrap();
    assert_eq!(snapshot.state, TaskState::Failed);
    assert!(!snapshot.result.unwrap().success);
    fs::remove_dir_all(root).unwrap();
}
