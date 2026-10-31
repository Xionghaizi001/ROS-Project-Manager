//! Private Unix socket endpoint and peer authentication.

use super::*;

/// A private, per-user endpoint. The directory and socket are both checked
/// before use; an existing non-socket path or symlink is rejected.
pub struct SocketEndpoint {
    pub directory: PathBuf,
    pub path: PathBuf,
    pub listener: UnixListener,
}

impl SocketEndpoint {
    pub fn bind(directory: impl AsRef<Path>) -> io::Result<Self> {
        let directory = directory.as_ref().to_path_buf();
        fs::create_dir_all(&directory)?;
        if fs::symlink_metadata(&directory)?.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "worker directory cannot be a symlink",
            ));
        }
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        let meta = fs::metadata(&directory)?;
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "worker directory must be private and owned by current user",
            ));
        }
        let path = directory.join("worker.sock");
        if path.exists() || fs::symlink_metadata(&path).is_ok() {
            let m = fs::symlink_metadata(&path)?;
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                if m.file_type().is_socket() {
                    "worker socket already exists; do not replace a running worker"
                } else {
                    "worker socket path is not a socket"
                },
            ));
        }
        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            directory,
            path,
            listener,
        })
    }

    pub fn accept_authenticated(&self) -> io::Result<(UnixStream, PeerIdentity)> {
        let (stream, _) = self.listener.accept()?;
        let peer = peer_identity(&stream)?;
        if peer.uid != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                ProtocolError::UnauthorizedPeer,
            ));
        }
        Ok((stream, peer))
    }
}

impl Drop for SocketEndpoint {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerIdentity {
    pub pid: i32,
    pub uid: u32,
    pub gid: u32,
}

fn peer_identity(stream: &UnixStream) -> io::Result<PeerIdentity> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PeerIdentity {
        pid: cred.pid,
        uid: cred.uid,
        gid: cred.gid,
    })
}
