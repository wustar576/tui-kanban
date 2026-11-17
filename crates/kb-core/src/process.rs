//! Whether a process is still running, with native calls only (docs/board-v2.md section
//! 13.3): `OpenProcess` and `GetExitCodeProcess` on Windows, `kill(pid, 0)` on unix. No
//! other program is started.

/// The current process id.
pub fn current_pid() -> u32 {
    std::process::id()
}

/// Whether process `pid` exists. A process that exists but belongs to another user counts
/// as alive. `pid` 0 is never alive.
pub fn is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    imp::is_alive(pid)
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    /// `GetExitCodeProcess` reports this while the process runs.
    const STILL_ACTIVE: u32 = 259;

    pub fn is_alive(pid: u32) -> bool {
        // SAFETY: plain Win32 calls; the handle is closed before returning.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                // No such process (ERROR_INVALID_PARAMETER), or one we may not open.
                return GetLastError() == ERROR_ACCESS_DENIED;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            ok != 0 && code == STILL_ACTIVE
        }
    }
}

#[cfg(unix)]
mod imp {
    pub fn is_alive(pid: u32) -> bool {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: signal 0 only checks that the process exists and may be signalled.
        let rc = unsafe { libc::kill(pid, 0) };
        if rc == 0 {
            return true;
        }
        // EPERM: it exists but belongs to someone else.
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(not(any(windows, unix)))]
mod imp {
    /// Unknown platform: assume alive (the waiter then ends by its other conditions).
    pub fn is_alive(_pid: u32) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_alive() {
        assert!(is_alive(current_pid()));
        assert!(!is_alive(0));
    }

    #[test]
    fn finished_child_is_not_alive() {
        let mut child = if cfg!(windows) {
            std::process::Command::new("cmd")
                .args(["/C", "exit 0"])
                .spawn()
        } else {
            std::process::Command::new("true").spawn()
        }
        .expect("spawn a short process");
        let pid = child.id();
        child.wait().unwrap();
        // The process is gone once waited for (its handle/zombie is released).
        drop(child);
        assert!(!is_alive(pid));
    }
}
