//! Single-instance lock, ported from upstream `utils.lock_file`: an OS-level
//! exclusive lock on `lock.file` that the kernel releases when we die.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::Path;

/// Held for the miner's lifetime; dropping it releases the lock.
pub struct SingleInstance {
    _file: File,
}

impl SingleInstance {
    /// Create and exclusively lock `path`. `WouldBlock`/`AlreadyExists` errors mean
    /// another process holds it.
    pub fn acquire(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new().create(true).write(true).truncate(false).open(path)?;
        let _ = writeln!(file, "ツ");
        file.flush()?;
        #[cfg(unix)]
        {
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Storage::FileSystem::{LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx};
            use windows_sys::Win32::System::IO::OVERLAPPED;
            let mut overlapped = OVERLAPPED::default();
            // we need to lock at least one byte for this to work
            let ok = unsafe {
                LockFileEx(
                    file.as_raw_handle(),
                    LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                    0,
                    1,
                    0,
                    &mut overlapped,
                )
            };
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_fails() {
        let dir = std::env::temp_dir().join(format!("rustpick-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lock.file");
        let _first = SingleInstance::acquire(&path).unwrap();
        assert!(SingleInstance::acquire(&path).is_err());
        drop(_first);
        let again = SingleInstance::acquire(&path).unwrap();
        drop(again);
        std::fs::remove_dir_all(&dir).ok();
    }
}
