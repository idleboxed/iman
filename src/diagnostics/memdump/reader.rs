// SPDX-License-Identifier: BSD-3-Clause
//! Read-only physical access in a sacrificial child. A bus stall is not recoverable by this isolation.
use std::{
    fs::{File, OpenOptions},
    io,
    os::{fd::AsRawFd, unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt}},
    path::PathBuf,
    time::{Duration, Instant},
};

use super::profile::{ReadPlan, DUMP_LIMIT};

const READ_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct Mapping {
    pub page_size: usize,
    pub offset: u64,
    pub length: usize,
    pub protection: &'static str,
    pub flags: &'static str,
}

pub struct Capture {
    pub bytes: Vec<u8>,
    pub error: Option<String>,
    pub mapping: Option<Mapping>,
}

/// Test implementations supply synthetic bytes without acquiring a device.
pub trait Reader {
    /// Return the capture method label recorded in the report.
    fn method(&self) -> &'static str { "provided_reader" }
    /// Perform the explicitly approved read plan and return captured bytes plus any partial-read details.
    fn read(&self, plan: &ReadPlan) -> io::Result<Capture>;
}

pub struct LinuxReader { device: PathBuf }
impl LinuxReader {
    /// Describe a physical-memory device path without opening or reading it.
    pub fn new(device: PathBuf) -> Self { Self { device } }
    /// Select /dev/mem for later explicitly guarded read operations.
    pub fn system() -> Self { Self::new("/dev/mem".into()) }

    fn open_device(&self) -> io::Result<File> {
        let anchor = OpenOptions::new().read(true)
            .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&self.device)?;
        let expected = anchor.metadata()?;
        if !expected.file_type().is_char_device() || libc::major(expected.rdev()) != 1 || libc::minor(expected.rdev()) != 1 {
            return Err(io::Error::other("physical memory source must be character device 1:1"));
        }
        let credentials = imanlib::metrics::read_text(std::path::Path::new("/proc/self/status"), 32 * 1024)?;
        let capabilities = credentials.lines().find_map(|line| line.strip_prefix("CapEff:"))
            .and_then(|value| u64::from_str_radix(value.trim(), 16).ok())
            .ok_or_else(|| io::Error::other("effective capabilities are unavailable"))?;
        if capabilities & (1 << 17) == 0 { return Err(io::Error::other("CAP_SYS_RAWIO is required")); }
        // The checked O_PATH inode is held before invoking any device open callback.
        let device = OpenOptions::new().read(true).custom_flags(libc::O_SYNC | libc::O_CLOEXEC)
            .open(format!("/proc/self/fd/{}", anchor.as_raw_fd()))?;
        let actual = device.metadata()?;
        if actual.dev() != expected.dev() || actual.ino() != expected.ino() || actual.rdev() != expected.rdev() {
            return Err(io::Error::other("physical memory device identity changed"));
        }
        Ok(device)
    }
}

#[repr(C)]
struct Shared {
    count: usize,
    error: libc::c_int,
    bytes: [u8; DUMP_LIMIT],
}

struct SharedMap { pointer: *mut Shared }
impl SharedMap {
    fn acquire() -> io::Result<Self> {
        // Anonymous shared RAM is zero-filled; no disk fallback or executable protection.
        let pointer = unsafe { libc::mmap(std::ptr::null_mut(), std::mem::size_of::<Shared>(),
            libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED | libc::MAP_ANONYMOUS, -1, 0) };
        if pointer == libc::MAP_FAILED { return Err(io::Error::last_os_error()); }
        Ok(Self { pointer: pointer.cast() })
    }
    fn release(mut self) -> io::Result<()> {
        if unsafe { libc::munmap(self.pointer.cast(), std::mem::size_of::<Shared>()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        self.pointer = std::ptr::null_mut();
        Ok(())
    }
}
impl Drop for SharedMap {
    fn drop(&mut self) {
        if !self.pointer.is_null() {
            unsafe { libc::munmap(self.pointer.cast(), std::mem::size_of::<Shared>()); }
        }
    }
}

/// No return to export until the child has exited, including wait/kill failures.
struct ReaderChild { pid: Option<libc::pid_t> }
impl ReaderChild {
    fn poll(&mut self, flags: libc::c_int) -> io::Result<Option<libc::c_int>> {
        let pid = self.pid.ok_or_else(|| io::Error::other("reader child already reaped"))?;
        loop {
            let mut status = 0;
            let result = unsafe { libc::waitpid(pid, &mut status, flags) };
            if result == pid {
                self.pid = None;
                return Ok(Some(status));
            }
            if result == 0 { return Ok(None); }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted { continue; }
            // ECHILD means no live child is owned; do not signal a potentially reused PID.
            if error.raw_os_error() == Some(libc::ECHILD) { self.pid = None; }
            return Err(error);
        }
    }

    fn stop(&mut self) -> io::Result<libc::c_int> {
        let pid = self.pid.ok_or_else(|| io::Error::other("reader child already reaped"))?;
        let mut failure = None;
        if unsafe { libc::kill(pid, libc::SIGKILL) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) { failure = Some(error); }
        }
        // Even if signaling/waiting fails, never proceed while the reader may be live.
        // Kernel/bus stalls can block this boundary; the deadline is not a guarantee.
        loop {
            match self.poll(0) {
                Ok(Some(status)) => return failure.map_or(Ok(status), Err),
                Err(error) if self.pid.is_none() => return Err(error),
                Err(error) => { failure.get_or_insert(error); }
                Ok(None) => (),
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for ReaderChild {
    fn drop(&mut self) {
        if self.pid.is_some() {
            if let Err(error) = self.stop() {
                crate::session::diagnostic(format_args!("iman: memory reader cleanup: {error}"));
            }
        }
    }
}

/// After fork, use only raw syscalls/volatile operations and stack values, never Rust allocation or locks.
unsafe fn read_child(parent: libc::pid_t, fd: libc::c_int, base: libc::off_t, map_length: usize, delta: usize, plan: ReadPlan, shared: *mut Shared) -> ! {
    let no_core = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &no_core) } != 0
        || unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } != 0
        || unsafe { libc::signal(libc::SIGBUS, libc::SIG_DFL) } == libc::SIG_ERR
        || unsafe { libc::signal(libc::SIGSEGV, libc::SIG_DFL) } == libc::SIG_ERR
    { unsafe { (*shared).error = *libc::__errno_location(); libc::_exit(1); } }
    if unsafe { libc::getppid() } != parent { unsafe { libc::_exit(1); } }
    let mapping = unsafe { libc::mmap(std::ptr::null_mut(), map_length, libc::PROT_READ, libc::MAP_SHARED, fd, base) };
    if mapping == libc::MAP_FAILED {
        unsafe { (*shared).error = *libc::__errno_location(); libc::_exit(1); }
    }
    let source = unsafe { mapping.cast::<u8>().add(delta) };
    let mut offset = 0;
    while offset < plan.length {
        // The configured width/alignment was checked before fork. No scan, memcpy of device memory or retry.
        let mut word = [0u8; 4];
        match plan.unit {
            1 => word[0] = unsafe { std::ptr::read_volatile(source.add(offset)) },
            2 => word[..2].copy_from_slice(&unsafe { std::ptr::read_volatile(source.add(offset).cast::<u16>()) }.to_ne_bytes()),
            4 => word.copy_from_slice(&unsafe { std::ptr::read_volatile(source.add(offset).cast::<u32>()) }.to_ne_bytes()),
            _ => unsafe { libc::_exit(2); },
        }
        let mut byte_index = 0;
        while byte_index < plan.unit {
            unsafe { std::ptr::write_volatile(std::ptr::addr_of_mut!((*shared).bytes).cast::<u8>().add(offset + byte_index), word[byte_index]); }
            byte_index += 1;
        }
        offset += plan.unit;
        unsafe { std::ptr::write_volatile(std::ptr::addr_of_mut!((*shared).count), offset); }
    }
    if unsafe { libc::munmap(mapping, map_length) } != 0 {
        unsafe { (*shared).error = *libc::__errno_location(); libc::_exit(1); }
    }
    unsafe { libc::_exit(0); }
}

fn wait_child(child: &mut ReaderChild, timeout: Duration) -> io::Result<(libc::c_int, bool)> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.poll(libc::WNOHANG)? {
            return Ok((status, false));
        }
        if started.elapsed() >= timeout {
            return child.stop().map(|status| (status, true));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

impl Reader for LinuxReader {
    fn method(&self) -> &'static str { "linux_devmem_ro_sync_shared_fork_volatile" }

    fn read(&self, plan: &ReadPlan) -> io::Result<Capture> {
        if !matches!(plan.unit, 1 | 2 | 4) || plan.length == 0 || plan.length > DUMP_LIMIT
            || plan.length % plan.unit != 0 || plan.address % plan.unit as u64 != 0
            || plan.address.checked_add(plan.length as u64).is_none_or(|end| end > i64::MAX as u64)
        { return Err(io::Error::other("invalid physical-memory read plan")); }
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let page = usize::try_from(page).ok().filter(|value| value.is_power_of_two())
            .ok_or_else(|| io::Error::other("invalid mapping page size"))?;
        let base = plan.address & !(page as u64 - 1);
        let delta = usize::try_from(plan.address - base).map_err(io::Error::other)?;
        let map_length = delta.checked_add(plan.length).and_then(|value| value.checked_add(page - 1))
            .map(|value| value & !(page - 1)).ok_or_else(|| io::Error::other("mapping size overflows"))?;
        let mapping = Mapping { page_size: page, offset: base, length: map_length,
            protection: "PROT_READ", flags: "MAP_SHARED" };
        let base = libc::off_t::try_from(base).map_err(io::Error::other)?;
        let device = self.open_device()?;
        let shared = SharedMap::acquire()?;
        let parent = unsafe { libc::getpid() };
        // All buffers/parameters are allocated before fork. The child never resumes the session.
        let pid = unsafe { libc::fork() };
        if pid < 0 { return Err(io::Error::last_os_error()); }
        if pid == 0 { unsafe { read_child(parent, device.as_raw_fd(), base, map_length, delta, *plan, shared.pointer); } }
        let mut child = ReaderChild { pid: Some(pid) };
        let (status, timed_out) = wait_child(&mut child, READ_TIMEOUT)?;
        let count = unsafe { std::ptr::read_volatile(std::ptr::addr_of!((*shared.pointer).count)) };
        if count > plan.length || count % plan.unit != 0 { return Err(io::Error::other("invalid reader child byte count")); }
        let bytes = unsafe { std::slice::from_raw_parts(std::ptr::addr_of!((*shared.pointer).bytes).cast::<u8>(), count) }.to_vec();
        let errno = unsafe { (*shared.pointer).error };
        let error = if timed_out { Some("reader child timed out; no retry".into()) }
            else if libc::WIFSIGNALED(status) { Some(format!("reader child terminated by signal {}; no retry", libc::WTERMSIG(status))) }
            else if !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0 || count != plan.length {
                Some(if errno != 0 { format!("reader child failed: {}", io::Error::from_raw_os_error(errno)) }
                    else { "reader child returned incomplete data".into() })
            } else { None };
        shared.release()?;
        Ok(Capture { bytes, error, mapping: Some(mapping) })
    }
}
