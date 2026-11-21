//! A bounded terminal transcript shared by the run owner and its worker.
use super::RunExit;
use std::{
    fs::{self, File, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::FileExt, fs::OpenOptionsExt, process::ExitStatusExt},
    },
    path::Path,
    process::ExitStatus,
};

pub const LIMIT: usize = 512 * 1024;
const HEADER: u64 = 12;
const NAME: &str = "/memfd:rospm-run-log (deleted)";

pub fn create() -> io::Result<File> {
    let fd = unsafe { libc::memfd_create(c"rospm-run-log".as_ptr(), libc::MFD_CLOEXEC) };
    if fd == -1 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    if unsafe { libc::fchmod(fd, 0o600) } == -1 {
        return Err(io::Error::last_os_error());
    }
    file.set_len(HEADER)?;
    file.write_all_at(&(-1i32).to_ne_bytes(), 8)?;
    Ok(file)
}

pub fn finish(file: &File, status: Option<ExitStatus>) -> io::Result<()> {
    let _lock = Lock::acquire(file, libc::LOCK_EX)?;
    file.write_all_at(&status.map_or(-2, ExitStatusExt::into_raw).to_ne_bytes(), 8)
}

pub fn exit(file: &File) -> io::Result<Option<RunExit>> {
    let _lock = Lock::acquire(file, libc::LOCK_SH)?;
    let mut bytes = [0; 4];
    file.read_exact_at(&mut bytes, 8)?;
    let raw = i32::from_ne_bytes(bytes);
    if raw == -1 {
        return Ok(None);
    }
    let status = ExitStatus::from_raw(raw);
    Ok(Some(if raw == -2 {
        RunExit::Unknown
    } else if let Some(code) = status.code() {
        RunExit::Code(code)
    } else {
        status.signal().map_or(RunExit::Unknown, RunExit::Signal)
    }))
}

/// Pin and verify the advertised memfd before opening it for normal I/O.
pub fn open(pid: i32, fd: i32, write: bool) -> io::Result<File> {
    if pid <= 0 || fd < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid run output descriptor",
        ));
    }
    let pinned = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH)
        .open(format!("/proc/{pid}/fd/{fd}"))?;
    let path = format!("/proc/self/fd/{}", pinned.as_raw_fd());
    if !is_transcript(pinned.as_raw_fd()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid run output file",
        ));
    }
    OpenOptions::new().read(true).write(write).open(path)
}

pub fn is_transcript(fd: i32) -> bool {
    fs::read_link(format!("/proc/self/fd/{fd}")).is_ok_and(|target| target == Path::new(NAME))
}

pub struct Writer {
    file: File,
    end: u64,
}

impl io::Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.append(bytes)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Writer {
    pub fn new(file: File) -> Self {
        Self { file, end: 0 }
    }

    pub fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        let _lock = Lock::acquire(&self.file, libc::LOCK_EX)?;
        let skipped = bytes.len().saturating_sub(LIMIT);
        let bytes = &bytes[skipped..];
        let offset = ((self.end % LIMIT as u64) as usize + skipped % LIMIT) % LIMIT;
        let first = bytes.len().min(LIMIT - offset);
        self.file
            .write_all_at(&bytes[..first], HEADER + offset as u64)?;
        if first < bytes.len() {
            self.file.write_all_at(&bytes[first..], HEADER)?;
        }
        self.end += (skipped + bytes.len()) as u64;
        self.file.write_all_at(&self.end.to_ne_bytes(), 0)
    }
}

/// Read relative to the oldest retained byte; a live log may drop older pages as it wraps.
pub fn page(file: &File, offset: u64, max: usize) -> io::Result<(Vec<u8>, u64, bool)> {
    let _lock = Lock::acquire(file, libc::LOCK_SH)?;
    let mut header = [0; 8];
    file.read_exact_at(&mut header, 0)?;
    let end = u64::from_ne_bytes(header);
    let start = end.saturating_sub(LIMIT as u64);
    let cursor = (start + offset.min(end - start)).min(end);
    let size = ((end - cursor) as usize).min(max);
    let mut bytes = vec![0; size];
    let physical = (cursor % LIMIT as u64) as usize;
    let first = size.min(LIMIT - physical);
    file.read_exact_at(&mut bytes[..first], HEADER + physical as u64)?;
    if first < size {
        file.read_exact_at(&mut bytes[first..], HEADER)?;
    }
    Ok((bytes, cursor - start, cursor + (size as u64) < end))
}

struct Lock(i32);

impl Lock {
    fn acquire(file: &File, operation: i32) -> io::Result<Self> {
        let fd = file.as_raw_fd();
        loop {
            if unsafe { libc::flock(fd, operation) } == 0 {
                return Ok(Self(fd));
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        unsafe { libc::flock(self.0, libc::LOCK_UN) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_exact_tail_across_wraps_and_oversized_writes() {
        let file = create().unwrap();
        let mut writer =
            Writer::new(open(std::process::id() as i32, file.as_raw_fd(), true).unwrap());
        writer.append(&vec![b'a'; LIMIT - 2]).unwrap();
        writer.append(b"wxyz").unwrap();
        assert_eq!(file.metadata().unwrap().len(), HEADER + LIMIT as u64);
        assert_eq!(page(&file, (LIMIT - 5) as u64, 32).unwrap().0, b"awxyz");
        writer.append(&vec![b'b'; LIMIT + 17]).unwrap();
        let (bytes, start, more) = page(&file, 0, LIMIT + 1).unwrap();
        assert_eq!(bytes, vec![b'b'; LIMIT]);
        assert_eq!(start, 0);
        assert!(!more);
    }
}
