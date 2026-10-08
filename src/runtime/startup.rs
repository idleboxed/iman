// SPDX-License-Identifier: BSD-3-Clause
//! Application-owned diagnostic startup, independent of the launching process.
use imanlib::filesystem::read_filesystem;
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io::{self, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    },
    path::{Component, Path},
};

pub const LOG_LIMIT: u64 = 65536;

fn directory(path: &Path) -> io::Result<File> {
    crate::runtime::bundle::directory(path)
}

/// A configured, missing leaf may be created only under an already verified tmpfs.
/// Never create ancestor trees or fall back to a disk-backed temporary directory.
pub fn prepare_runtime(path: &Path) -> io::Result<()> {
    match directory(path) {
        Ok(file) => ram_space(&file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or_else(|| io::Error::other("runtime parent missing"))?;
            let name = path.file_name().ok_or_else(|| io::Error::other("runtime name missing"))?;
            let parent = directory(parent)?;
            ram_space(&parent)?;
            let name = CString::new(name.as_encoded_bytes())?;
            if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
                return Err(io::Error::last_os_error());
            }
            ram_space(&directory(path)?)
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn ram_space(fd: &File) -> io::Result<()> {
    let info = read_filesystem(fd)?;
    if !info.is_tmpfs || info.available_bytes < 1024 * 1024 {
        return Err(io::Error::other("tmpfs with at least 1 MiB free required"));
    }
    Ok(())
}

// Keep owned handles above stdio even when init supplies closed descriptors.
fn above_stdio(file: File) -> io::Result<File> {
    if file.as_raw_fd() > 2 {
        return Ok(file);
    }
    let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fcntl returned a new owned descriptor; the original File is dropped.
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub struct RamLog {
    // Cleanup uses the held parent FD, before that descriptor is dropped.
    _temporary: Option<tempfile::TempDir>,
    _parent: Option<File>,
    file: File,
    directory: File,
}

impl RamLog {
    /// Create a private, never-overwritten run on tmpfs. No disk fallback,
    /// process-wide changes or deletion of existing evidence on failure.
    pub fn create(parent: &Path, name: &str) -> io::Result<Self> {
        let mut parts = Path::new(name).components();
        if name.is_empty()
            || name.contains('/')
            || !matches!(parts.next(), Some(Component::Normal(_)))
            || parts.next().is_some()
        {
            return Err(io::Error::other("one runtime directory name required"));
        }
        let name = CString::new(name)?;
        let parent = above_stdio(directory(parent)?)?;
        ram_space(&parent)?;
        // SAFETY: valid parent FD and terminated single-component name.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let directory = above_stdio(unsafe { File::from_raw_fd(fd) })?;
        if directory.metadata()?.dev() != parent.metadata()?.dev() {
            return Err(io::Error::other("runtime directory device changed"));
        }
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                c"igui.log".as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            _temporary: None,
            _parent: None,
            file: above_stdio(unsafe { File::from_raw_fd(fd) })?,
            directory,
        })
    }

    /// One private RAM-only log per normal Linux session, cleaned at owned-resource drop.
    pub fn temporary(parent: &Path) -> io::Result<Self> {
        let parent = directory(parent)?;
        ram_space(&parent)?;
        let temporary = tempfile::Builder::new().prefix("iman-log-")
            .tempdir_in(format!("/proc/self/fd/{}", parent.as_raw_fd()))?;
        let directory = above_stdio(OpenOptions::new().read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(temporary.path())?)?;
        if directory.metadata()?.dev() != parent.metadata()?.dev() {
            return Err(io::Error::other("runtime directory device changed"));
        }
        let fd = unsafe { libc::openat(directory.as_raw_fd(), c"igui.log".as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o600) };
        if fd < 0 { return Err(io::Error::last_os_error()); }
        Ok(Self { _temporary: Some(temporary), _parent: Some(parent),
            file: above_stdio(unsafe { File::from_raw_fd(fd) })?, directory })
    }

    pub fn record(&mut self, text: &str) -> io::Result<()> {
        self.file.write_all(text.as_bytes())
    }

    #[cfg(feature = "diag-cycle")]
    fn checked_reader(&self) -> io::Result<File> {
        ram_space(&self.directory)?;
        let path = Path::new("/proc/self/fd")
            .join(self.directory.as_raw_fd().to_string()).join("igui.log");
        let file = OpenOptions::new().read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(path)?;
        let meta = file.metadata()?;
        let owned = self.file.metadata()?;
        if !meta.is_file() || meta.nlink() != 1 || meta.len() > LOG_LIMIT
            || meta.dev() != self.directory.metadata()?.dev()
            || meta.dev() != owned.dev() || meta.ino() != owned.ino()
        {
            return Err(io::Error::other("segment log identity or size refused"));
        }
        Ok(file)
    }

    /// Snapshot the owned log without moving the shared stdout/stderr offset.
    #[cfg(feature = "diag-cycle")]
    pub fn snapshot(&self) -> io::Result<Vec<u8>> {
        use std::io::Read;
        let file = self.checked_reader()?;
        let mut bytes = Vec::new();
        file.take(LOG_LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() > LOG_LIMIT as usize {
            return Err(io::Error::other("segment log exceeds size limit"));
        }
        Ok(bytes)
    }

    /// Reset only after a synchronized export and after every child was reaped.
    #[cfg(feature = "diag-cycle")]
    pub fn reset(&self) -> io::Result<()> {
        let meta = self.checked_reader()?.metadata()?;
        for fd in [1, 2] {
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: borrowed descriptors and a writable stat buffer.
            if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let stat = unsafe { stat.assume_init() };
            if stat.st_dev as u64 != meta.dev() || stat.st_ino as u64 != meta.ino() {
                return Err(io::Error::other("segment log stdio identity changed"));
            }
        }
        // activate() duplicated this open file description into both streams.
        if unsafe { libc::ftruncate(self.file.as_raw_fd(), 0) } != 0
            || unsafe { libc::lseek(self.file.as_raw_fd(), 0, libc::SEEK_SET) } < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Set this application's own limits, stdin, RAM output and working directory.
    /// Does not close foreign FDs or reset inherited signals/timers.
    ///
    /// # Safety
    /// Call once in a standalone executable before starting other components.
    /// No other thread or owned object may use stdio or depend on the cwd/limits.
    /// A failed call may leave partially applied process settings; exit silently.
    pub unsafe fn activate(&self) -> io::Result<()> {
        let console = crate::session::diagnostic_console();
        let null = above_stdio(
            OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open("/dev/null")?,
        )?;
        let metadata = null.metadata()?;
        if !metadata.file_type().is_char_device()
            || libc::major(metadata.rdev()) != 1
            || libc::minor(metadata.rdev()) != 3
        {
            return Err(io::Error::other("/dev/null identity refused"));
        }
        let mut size = std::mem::MaybeUninit::<libc::rlimit>::uninit();
        if libc::getrlimit(libc::RLIMIT_FSIZE, size.as_mut_ptr()) != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut size = size.assume_init();
        size.rlim_cur = size.rlim_max.min(LOG_LIMIT as libc::rlim_t);
        let core = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // Keep the inherited hard file limit: games restore it for explicit saves.
        if libc::setrlimit(libc::RLIMIT_CORE, &core) != 0
            || libc::setrlimit(libc::RLIMIT_FSIZE, &size) != 0
        {
            return Err(io::Error::last_os_error());
        }
        for (source, target) in [
            (null.as_raw_fd(), 0),
            (self.file.as_raw_fd(), 1),
            (self.file.as_raw_fd(), 2),
        ] {
            if libc::dup2(source, target) != target {
                return Err(io::Error::last_os_error());
            }
        }
        if libc::fchdir(self.directory.as_raw_fd()) != 0 {
            return Err(io::Error::last_os_error());
        }
        crate::session::set_diagnostic_console(console);
        Ok(())
    }
}

/// A diagnostic subprocess reuses its parent's RAM context, never recreates it.
pub fn validate_attached(path: &Path) -> io::Result<()> {
    if std::env::current_dir()? != path {
        return Err(io::Error::other("diagnostic working directory required"));
    }
    for fd in [1, 2] {
        let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: borrowed descriptors and writable stat buffers.
        if unsafe { libc::fstatfs(fd, fs.as_mut_ptr()) } != 0
            || unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let fs = unsafe { fs.assume_init() };
        let stat = unsafe { stat.assume_init() };
        if fs.f_type as u64 != libc::TMPFS_MAGIC as u64
            || stat.st_mode & libc::S_IFMT != libc::S_IFREG
            || stat.st_nlink != 1
        {
            return Err(io::Error::other("regular single-link tmpfs logs required"));
        }
    }
    Ok(())
}
