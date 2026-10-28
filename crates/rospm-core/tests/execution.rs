use rospm_core::{
    execution::{Confirmation, Event, Interaction, Operation},
    plan::{Command, Plan, Step},
    Message,
};
use serde_json::{json, Value};
use std::sync::Mutex;

#[derive(Default)]
struct Recorder(Mutex<Vec<Value>>);
impl Interaction for Recorder {
    fn event(&self, event: Event<'_>) {
        self.0
            .lock()
            .unwrap()
            .push(serde_json::to_value(event).unwrap());
    }
}
impl Recorder {
    fn bytes(&self, stream: &str) -> Vec<u8> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e["event"] == "log" && e["stream"] == stream)
            .flat_map(|e| {
                e["bytes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|b| b.as_u64().unwrap() as u8)
            })
            .collect()
    }
}

#[test]
fn captures_both_pipes_as_bytes_and_closes_stdin() {
    let mut plan = Plan::default();
    plan.note(Message::text("note"));
    plan.run(Command::new("bash").args(["-c", "if read -r line; then exit 9; fi; head -c 200000 /dev/zero & head -c 200000 /dev/zero >&2 & wait; printf '\\377'"]));
    let recorder = Recorder::default();
    plan.perform_with("task-1", &recorder).unwrap();
    let stdout = recorder.bytes("stdout");
    assert_eq!(stdout.len(), 200001);
    assert_eq!(stdout.last(), Some(&255));
    assert_eq!(recorder.bytes("stderr"), vec![0; 200000]);
    let events = recorder.0.lock().unwrap();
    assert_eq!(
        events[0],
        json!({"event":"task", "task":"task-1", "state":"running", "total":2, "error":null})
    );
    assert_eq!(events.last().unwrap()["state"], "succeeded");
    let steps: Vec<_> = events
        .iter()
        .filter(|e| e["event"] == "step")
        .map(|e| (e["step"].clone(), e["state"].clone()))
        .collect();
    assert_eq!(
        steps,
        vec![
            (json!(1), json!("running")),
            (json!(1), json!("succeeded")),
            (json!(2), json!("running")),
            (json!(2), json!("succeeded"))
        ]
    );
}

#[test]
fn failure_stops_plan_and_reuses_diagnostics() {
    let root = std::env::temp_dir().join(format!("rospm-events-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let log = root.join("build.log");
    let mut plan = Plan {
        log: Some(log.clone()),
        ..Plan::default()
    };
    plan.run(
        Command::new("bash")
            .args([
                "-c",
                "printf \"main.cpp:2:3: error: expected ';'\\n\" >&2; exit 7",
            ])
            .log("workspace: main\npython: system\n"),
    );
    plan.steps.push(Step::WriteFile {
        path: root.join("must-not-exist"),
        contents: "bad".into(),
        sudo: false,
    });
    let recorder = Recorder::default();
    let err = plan.perform_with("failed", &recorder).unwrap_err();
    assert!(matches!(err, rospm_core::Error::CommandFailed { .. }));
    assert!(!root.join("must-not-exist").exists());
    let events = recorder.0.lock().unwrap();
    let diagnostic = events.iter().find(|e| e["event"] == "diagnostics").unwrap();
    assert_eq!(diagnostic["summary"]["diagnostic_status"], "parsed");
    assert_eq!(
        diagnostic["summary"]["groups"][0]["diagnostics"][0]["location"]["file"],
        "main.cpp"
    );
    assert_eq!(events.last().unwrap()["state"], "failed");
    assert!(events.iter().all(|e| e["step"] != 2));
    assert!(std::fs::read_to_string(log)
        .unwrap()
        .contains("exit status: 7"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn spawn_and_file_errors_are_failed_steps_and_empty_plan_succeeds() {
    for step in [
        Step::Run(Command::new("/nonexistent/rospm-command")),
        Step::WriteFile {
            path: "/dev/null/child".into(),
            contents: "x".into(),
            sudo: false,
        },
    ] {
        let plan = Plan {
            steps: vec![step],
            ..Plan::default()
        };
        let recorder = Recorder::default();
        assert!(plan.perform_with("error", &recorder).is_err());
        let events = recorder.0.lock().unwrap();
        assert_eq!(events.last().unwrap()["state"], "failed");
        assert!(events
            .iter()
            .any(|e| e["event"] == "step" && e["state"] == "failed"));
    }
    let recorder = Recorder::default();
    Plan::default().perform_with("empty", &recorder).unwrap();
    assert_eq!(recorder.0.lock().unwrap().len(), 2);
}

#[test]
fn absent_confirmation_handler_never_approves() {
    let recorder = Recorder::default();
    let request = Confirmation {
        task: "task-1".into(),
        step: 2,
        operation: Operation::CleanBuild,
        paths: vec!["/project/build".into()],
        prompt: Message::key("cli.build.clean_confirm"),
    };
    assert!(!recorder.confirm(&request));
    let value = serde_json::to_value(request).unwrap();
    assert_eq!(value["operation"], "clean_build");
    assert_eq!(value["paths"], json!(["/project/build"]));
}
