//! OS-specific primitives. Permission errors are returned to the caller.
use std::{ffi::CStr, io};

pub unsafe fn rename_exclusive(from: &CStr, to: &CStr) -> i32 {
    #[cfg(target_os = "linux")]
    {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    }
    #[cfg(target_os = "macos")]
    {
        libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL)
    }
}

pub unsafe fn pipe_cloexec(fds: *mut i32) -> i32 {
    #[cfg(target_os = "linux")]
    {
        libc::pipe2(fds, libc::O_CLOEXEC)
    }
    #[cfg(target_os = "macos")]
    {
        if libc::pipe(fds) < 0 {
            return -1;
        }
        // Darwin has no pipe2; mark both ends close-on-exec immediately.
        for i in 0..2 {
            if libc::fcntl(*fds.add(i), libc::F_SETFD, libc::FD_CLOEXEC) < 0 {
                let errno = *libc::__error();
                libc::close(*fds);
                libc::close(*fds.add(1));
                *libc::__error() = errno;
                return -1;
            }
        }
        0
    }
}

#[cfg(target_os = "linux")]
pub fn session_candidates(_session: i32) -> io::Result<Vec<i32>> {
    let mut ids = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        if let Ok(pid) = entry?.file_name().to_string_lossy().parse() {
            ids.push(pid);
        }
    }
    Ok(ids)
}

#[cfg(target_os = "macos")]
pub fn session_candidates(session: i32) -> io::Result<Vec<i32>> {
    // proc_listallpids returns a count, unlike proc_listpids' byte count.
    for _ in 0..4 {
        let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        if count <= 0 {
            return Err(io::Error::last_os_error());
        }
        let mut ids = vec![0i32; count as usize + 256];
        let n = unsafe { libc::proc_listallpids(ids.as_mut_ptr().cast(), (ids.len() * 4) as i32) };
        if n <= 0 {
            return Err(io::Error::last_os_error());
        }
        if n as usize >= ids.len() {
            continue;
        }
        ids.truncate(n as usize);
        let mut found = Vec::new();
        for pid in ids.into_iter().filter(|p| *p > 0) {
            let sid = unsafe { libc::getsid(pid) };
            if sid == session {
                found.push(pid);
            } else if sid < 0 {
                let e = io::Error::last_os_error();
                if e.raw_os_error() != Some(libc::ESRCH) {
                    return Err(e);
                }
            }
        }
        return Ok(found);
    }
    Err(io::Error::other(
        "Process list changed too quickly to inspect cleanup coverage",
    ))
}
