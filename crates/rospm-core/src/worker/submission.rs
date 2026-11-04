//! Durable at-most-once submission reservations, independent of task retention.

use super::*;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionStatus {
    New,
    Existing,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Submission {
    task_id: String,
    workspace: String,
    operation: Operation,
}

impl TaskStore {
    /// Reserve the full request durably before the caller enqueues any work.
    /// Only `New` permits execution. Even an incomplete reservation is never
    /// replayed after a crash; missing snapshots must be reported to the client.
    pub fn register_submission(
        &self,
        request_id: &str,
        task_id: &str,
        workspace: &str,
        operation: &Operation,
    ) -> Result<SubmissionStatus, StoreError> {
        if !valid_identifier(request_id)
            || !valid_identifier(task_id)
            || workspace.is_empty()
            || workspace.contains('\0')
        {
            return Err(StoreError::InvalidSubmission);
        }
        let submission = Submission {
            task_id: task_id.into(),
            workspace: workspace.into(),
            operation: operation.clone(),
        };
        // '@' is not a valid task identifier, so metadata cannot collide with tasks.
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.root.join("@submissions.lock"))?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        let path = self.root.join("@submissions.json");
        // shortcut: rewrite the small reservation ledger; use an indexed store if task volume warrants it.
        let mut records: BTreeMap<String, Submission> = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e.into()),
        };
        if let Some(existing) = records.get(request_id) {
            return if existing == &submission {
                Ok(SubmissionStatus::Existing)
            } else {
                Err(StoreError::SubmissionConflict)
            };
        }
        if records.values().any(|record| record.task_id == task_id) {
            return Err(StoreError::SubmissionConflict);
        }
        match fs::symlink_metadata(self.root.join(task_id)) {
            Ok(_) => return Err(StoreError::AlreadyExists),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
        records.insert(request_id.into(), submission);
        let tmp = self.root.join("@submissions.tmp");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)?;
        serde_json::to_writer(&mut file, &records)?;
        file.sync_all()?;
        fs::rename(tmp, path)?;
        File::open(&self.root)?.sync_all()?;
        self.create_task(&TaskSnapshot {
            task_id: task_id.into(),
            state: TaskState::Pending,
            step: 0,
            total_steps: None,
            log_cursor: 0,
            result: None,
        })?;
        File::open(self.root.join(task_id))?.sync_all()?;
        File::open(&self.root)?.sync_all()?;
        Ok(SubmissionStatus::New)
    }
}
