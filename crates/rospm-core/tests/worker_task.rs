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
