//! Another process's working directory and open files, read in-process with
//! macOS `libproc` instead of starting `lsof` (about 50 ms per call). `None`
//! means the process could not be read (it exited, or belongs to another
//! user); callers fall back to `lsof` then.

use std::path::PathBuf;

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::CStr;
    use std::mem::{size_of, MaybeUninit};
    use std::os::raw::{c_int, c_void};
    use std::path::PathBuf;

    // sys/proc_info.h; libc has the constant's siblings but not these two.
    const PROC_PIDFDVNODEPATHINFO: c_int = 2;

    #[repr(C)]
    struct ProcFileInfo {
        fi_openflags: u32,
        fi_status: u32,
        fi_offset: i64,
        fi_type: i32,
        fi_guardflags: u32,
    }

    #[repr(C)]
    struct VnodeFdInfoWithPath {
        pfi: ProcFileInfo,
        pvip: libc::vnode_info_path,
    }

    fn path_of(info: &libc::vnode_info_path) -> Option<PathBuf> {
        // SAFETY: vip_path is a MAXPATHLEN byte array the kernel NUL-terminates.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                info.vip_path.as_ptr().cast::<u8>(),
                size_of_val(&info.vip_path),
            )
        };
        let path = CStr::from_bytes_until_nul(bytes).ok()?.to_str().ok()?;
        (!path.is_empty()).then(|| PathBuf::from(path))
    }

    pub fn cwd(pid: u32) -> Option<PathBuf> {
        let pid = c_int::try_from(pid).ok()?;
        let mut info = MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
        let size = size_of::<libc::proc_vnodepathinfo>() as c_int;
        // SAFETY: the buffer is exactly the size passed in.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                info.as_mut_ptr().cast::<c_void>(),
                size,
            )
        };
        if written != size {
            return None;
        }
        // SAFETY: the kernel filled the whole struct.
        path_of(&unsafe { info.assume_init() }.pvi_cdir)
    }

    pub fn open_files(pid: u32) -> Option<Vec<PathBuf>> {
        let pid = c_int::try_from(pid).ok()?;
        let entry = size_of::<libc::proc_fdinfo>();
        // SAFETY: a null buffer asks for the required size.
        let needed =
            unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
        if needed <= 0 {
            return None;
        }
        // Room for descriptors opened between the two calls.
        let mut fds = Vec::<libc::proc_fdinfo>::with_capacity(needed as usize / entry + 32);
        let capacity = (fds.capacity() * entry) as c_int;
        // SAFETY: the buffer holds `capacity` bytes of proc_fdinfo entries.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                fds.as_mut_ptr().cast::<c_void>(),
                capacity,
            )
        };
        if written <= 0 {
            return None;
        }
        // SAFETY: the kernel initialised `written` bytes of whole entries.
        unsafe { fds.set_len(written as usize / entry) };
        let mut paths = Vec::new();
        for fd in fds {
            if fd.proc_fdtype != libc::PROX_FDTYPE_VNODE as u32 {
                continue;
            }
            let mut info = MaybeUninit::<VnodeFdInfoWithPath>::zeroed();
            let size = size_of::<VnodeFdInfoWithPath>() as c_int;
            // SAFETY: the buffer is exactly the size passed in.
            let written = unsafe {
                libc::proc_pidfdinfo(
                    pid,
                    fd.proc_fd,
                    PROC_PIDFDVNODEPATHINFO,
                    info.as_mut_ptr().cast::<c_void>(),
                    size,
                )
            };
            // A descriptor closed since the listing is simply skipped.
            if written == size {
                // SAFETY: the kernel filled the whole struct.
                if let Some(path) = path_of(&unsafe { info.assume_init() }.pvip) {
                    paths.push(path);
                }
            }
        }
        Some(paths)
    }
}

#[cfg(target_os = "macos")]
pub fn cwd(pid: u32) -> Option<PathBuf> {
    imp::cwd(pid)
}

#[cfg(target_os = "macos")]
pub fn open_files(pid: u32) -> Option<Vec<PathBuf>> {
    imp::open_files(pid)
}

#[cfg(not(target_os = "macos"))]
pub fn cwd(_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(not(target_os = "macos"))]
pub fn open_files(_pid: u32) -> Option<Vec<PathBuf>> {
    None
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn reads_this_process() {
        let dir = tempfile::tempdir().unwrap();
        let path = std::fs::canonicalize(dir.path()).unwrap().join("held.db");
        let _held = std::fs::File::create(&path).unwrap();
        let pid = std::process::id();
        assert_eq!(cwd(pid), std::env::current_dir().ok());
        assert!(open_files(pid).unwrap().contains(&path));
    }

    #[test]
    fn unreadable_processes_are_none() {
        // launchd belongs to root.
        assert_eq!(open_files(1), None);
        assert_eq!(cwd(1), None);
        assert_eq!(open_files(u32::MAX), None);
    }
}
