//! Coordination primitives shared by CLI commands and worker task runners.
//!
//! The workspace lease is a kernel advisory lock.  It is deliberately held
//! for the complete task, including confirmation waits, so a second worker
//! cannot mutate the same workspace while the first task is paused.

use super::ConfirmationResponse;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceLockError {
    #[error("workspace lock I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("workspace path cannot be resolved: {0}")]
    InvalidWorkspace(PathBuf),
}

/// An exclusive lease for one workspace. Dropping it releases the kernel lock.
pub struct WorkspaceLock {
    file: File,
    workspace: PathBuf,
    path: PathBuf,
}

impl WorkspaceLock {
    /// Wait until this workspace is available. The lock is process and
    /// worker safe and is not removed on drop, avoiding stale lock files.
    pub fn acquire(
        runtime_dir: impl AsRef<Path>,
        workspace: impl AsRef<Path>,
    ) -> Result<Self, WorkspaceLockError> {
        Self::open(runtime_dir, workspace, false)
            .map(|lock| lock.expect("blocking lock always returns a lease"))
    }

    /// Attempt to acquire without waiting. `Ok(None)` means another worker
    /// currently owns the workspace; callers may report a pending task.
    pub fn try_acquire(
        runtime_dir: impl AsRef<Path>,
        workspace: impl AsRef<Path>,
    ) -> Result<Option<Self>, WorkspaceLockError> {
        Self::open(runtime_dir, workspace, true)
    }

    /// A resource lease shared by every frontend and project for this user.
    /// The namespace is independent of project/runtime and TMPDIR overrides.
    pub fn try_acquire_mutation(
        workspace: impl AsRef<Path>,
    ) -> Result<Option<Self>, WorkspaceLockError> {
        let uid = unsafe { libc::geteuid() };
        let runtime = PathBuf::from(format!("/tmp/rospm-leases-{uid}"));
        match fs::DirBuilder::new().mode(0o700).create(&runtime) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let metadata = fs::symlink_metadata(&runtime)?;
        if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            return Err(
                io::Error::new(io::ErrorKind::PermissionDenied, "unsafe lease directory").into(),
            );
        }
        Self::try_acquire(runtime, workspace)
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }
    pub fn lock_path(&self) -> &Path {
        &self.path
    }

    fn open(
        runtime_dir: impl AsRef<Path>,
        workspace: impl AsRef<Path>,
        nonblocking: bool,
    ) -> Result<Option<Self>, WorkspaceLockError> {
        let workspace = resolve_workspace(workspace.as_ref())?;
        let locks = runtime_dir.as_ref().join("workspace-locks");
        fs::create_dir_all(&locks)?;
        fs::set_permissions(&locks, fs::Permissions::from_mode(0o700))?;
        let mut digest = Sha256::new();
        digest.update(workspace.as_os_str().as_encoded_bytes());
        let path = locks.join(format!("{:x}.lock", digest.finalize()));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&path)?;
        let mut flags = libc::LOCK_EX;
        if nonblocking {
            flags |= libc::LOCK_NB;
        }
        let rc = unsafe { libc::flock(file.as_raw_fd(), flags) };
        if rc != 0 {
            let error = io::Error::last_os_error();
            if nonblocking && error.kind() == io::ErrorKind::WouldBlock {
                return Ok(None);
            }
            return Err(error.into());
        }
        Ok(Some(Self {
            file,
            workspace,
            path,
        }))
    }
}

impl Drop for WorkspaceLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Canonical resource identity, including paths not created yet.
pub fn resolve_workspace(path: &Path) -> Result<PathBuf, WorkspaceLockError> {
    if path.as_os_str().is_empty() {
        return Err(WorkspaceLockError::InvalidWorkspace(path.to_path_buf()));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    // Resolve existing ancestors too: a not-yet-created workspace behind a
    // symlink must keep the same identity after apply creates it.
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        use std::path::Component;
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            other => {
                resolved.push(other.as_os_str());
                match fs::canonicalize(&resolved) {
                    Ok(path) => resolved = path,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
    }
    Ok(resolved)
}

/// A task-scoped confirmation rendezvous. Dropping a client does not answer
/// the request: the worker remains parked until an explicit response or
/// cancellation arrives, so a disconnect can never approve a destructive step.
#[derive(Clone, Default)]
pub struct ConfirmationWait(Arc<(Mutex<Option<ConfirmationResponse>>, Condvar)>);

impl ConfirmationWait {
    pub fn respond(&self, response: ConfirmationResponse) {
        let (state, wake) = &*self.0;
        *state.lock().expect("confirmation poisoned") = Some(response);
        wake.notify_all();
    }

    pub fn cancel(&self) {
        self.respond(ConfirmationResponse::Cancel);
    }

    pub fn wait(&self) -> ConfirmationResponse {
        let (state, wake) = &*self.0;
        let mut state = state.lock().expect("confirmation poisoned");
        loop {
            if let Some(response) = state.take() {
                return response;
            }
            state = wake.wait(state).expect("confirmation poisoned");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn workspace_lock_serializes_same_workspace() {
        let root = std::env::temp_dir().join(format!("rospm-coordination-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let workspace = root.join("ws");
        fs::create_dir_all(&workspace).unwrap();
        let first = WorkspaceLock::acquire(&root, &workspace).unwrap();
        assert!(WorkspaceLock::try_acquire(&root, &workspace)
            .unwrap()
            .is_none());
        drop(first);
        assert!(WorkspaceLock::try_acquire(&root, &workspace)
            .unwrap()
            .is_some());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn mutation_identity_survives_creation_and_resolves_aliases() {
        let root = std::env::temp_dir().join(format!("rospm-lease-alias-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("alias")).unwrap();
        let workspace = root.join("real/new/ws");
        let first = WorkspaceLock::try_acquire_mutation(root.join("alias/new/./ws"))
            .unwrap()
            .unwrap();
        assert!(WorkspaceLock::try_acquire_mutation(&workspace)
            .unwrap()
            .is_none());
        fs::create_dir_all(&workspace).unwrap();
        assert!(
            WorkspaceLock::try_acquire_mutation(root.join("alias/new/ws/../ws"))
                .unwrap()
                .is_none()
        );
        assert!(WorkspaceLock::try_acquire_mutation(root.join("other"))
            .unwrap()
            .is_some());
        drop(first);
        assert!(WorkspaceLock::try_acquire_mutation(&workspace)
            .unwrap()
            .is_some());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn confirmation_wait_requires_explicit_response() {
        let wait = ConfirmationWait::default();
        let other = wait.clone();
        let handle = std::thread::spawn(move || other.wait());
        std::thread::sleep(Duration::from_millis(20));
        assert!(!handle.is_finished());
        wait.respond(ConfirmationResponse::Approve);
        assert_eq!(handle.join().unwrap(), ConfirmationResponse::Approve);
    }
}
