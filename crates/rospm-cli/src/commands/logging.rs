//! Reuse the shared command-output tee for workflow log files.
use rospm_core::plan::{Plan, Step};
use std::path::Path;

/// This configures paths only: previews and declined confirmations write nothing.
/// These logs cover external Run commands, not Conda's internal execution steps.
pub(super) fn commands(plan: &mut Plan, root: &Path, workflow: &str) {
    plan.log = Some(root.join(".rospm/log").join(format!("{workflow}.log")));
    for step in &mut plan.steps {
        if let Step::Run(command) = step {
            if command.log.is_none() {
                command.log = Some(format!("workflow: {workflow}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rospm_core::plan::Command;

    #[test]
    fn preview_is_read_only_and_execution_records_both_streams() {
        let root = std::env::temp_dir().join(format!(
            "rospm-workflow-log-{}",
            super::super::interaction::task_id()
        ));
        let mut plan = Plan::new("log test".to_string());
        plan.run(Command::new("sh").args([
            "-c",
            "printf 'workflow-out\\n'; printf 'workflow-err\\n' >&2",
        ]));
        commands(&mut plan, &root, "deps");
        plan.execute(true, &mut Vec::new()).unwrap();
        assert!(!root.exists());
        plan.execute(false, &mut Vec::new()).unwrap();
        let log = std::fs::read_to_string(root.join(".rospm/log/deps.log")).unwrap();
        assert!(log.contains("workflow-out\n"));
        assert!(log.contains("workflow-err\n"));
        assert!(log.contains("workflow: deps"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
