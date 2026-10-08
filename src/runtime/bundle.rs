// SPDX-License-Identifier: BSD-3-Clause
//! Descriptor-held Linux installation root; no board, mount or raw-device operations.
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io,
    os::{fd::{AsRawFd, FromRawFd}, unix::fs::{MetadataExt, OpenOptionsExt}},
    path::{Component, Path, PathBuf},
};

pub(crate) fn above_stdio(file: File) -> io::Result<File> {
    if file.as_raw_fd() > 2 { return Ok(file); }
    let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if fd < 0 { return Err(io::Error::last_os_error()); }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn child(parent: &File, name: &std::ffi::CStr, directory: bool) -> io::Result<File> {
    let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        | if directory { libc::O_DIRECTORY } else { libc::O_NONBLOCK };
    // SAFETY: borrowed parent and terminated name; no creation or path fallback.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 { return Err(io::Error::last_os_error()); }
    above_stdio(unsafe { File::from_raw_fd(fd) })
}

/// Reject symlinks in every component, including ancestors of the chosen root.
pub(crate) fn directory(path: &Path) -> io::Result<File> {
    let path = std::path::absolute(path)?;
    let mut directory = above_stdio(OpenOptions::new().read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open("/")?)?;
    for part in path.components() {
        let name = match part {
            Component::RootDir | Component::CurDir => continue,
            Component::Normal(name) => CString::new(name.as_encoded_bytes())?,
            Component::ParentDir => c"..".to_owned(),
            Component::Prefix(_) => return Err(io::Error::other("Linux directory path required")),
        };
        directory = child(&directory, &name, true)?;
    }
    Ok(directory)
}

pub struct Root {
    path: PathBuf,
    directory: File,
}

impl Root {
    /// Explicit read-only acquisition. No directories, reports or processes are created.
    pub fn open(path: &Path) -> io::Result<Self> {
        let directory = directory(path)?;
        let root = Self { path: path.canonicalize()?, directory };
        root.validate()?;
        Ok(root)
    }

    pub fn path(&self) -> &Path { &self.path }

    /// Refuse a renamed/replaced root, rather than switching the session's destination.
    /// This is not protection against privileged writes into an existing inode.
    pub fn validate(&self) -> io::Result<()> {
        let current = directory(&self.path)?.metadata()?;
        let held = self.directory.metadata()?;
        if current.dev() != held.dev() || current.ino() != held.ino() || held.nlink() == 0 {
            return Err(io::Error::other("installation root identity changed"));
        }
        Ok(())
    }

    pub fn directory(&self) -> io::Result<&File> {
        self.validate()?;
        Ok(&self.directory)
    }

    fn open_relative(&self, relative: &str, is_directory: bool) -> io::Result<File> {
        self.validate()?;
        if !imanlib::igui::valid_image_path(relative) {
            return Err(io::Error::other("invalid installation-relative path"));
        }
        let mut parent = self.directory.try_clone()?;
        let device = parent.metadata()?.dev();
        let parts = Path::new(relative).components().collect::<Vec<_>>();
        for (index, part) in parts.iter().enumerate() {
            let name = CString::new(part.as_os_str().as_encoded_bytes())?;
            parent = child(&parent, &name, is_directory || index + 1 < parts.len())?;
            if parent.metadata()?.dev() != device {
                return Err(io::Error::other("installation path must stay on the same device"));
            }
        }
        Ok(parent)
    }

    pub fn open_directory(&self, relative: &str) -> io::Result<File> {
        self.open_relative(relative, true)
    }

    pub fn open_file(&self, relative: &str) -> io::Result<File> {
        let file = self.open_relative(relative, false)?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.nlink() != 1 {
            return Err(io::Error::other("regular single-link installation file required"));
        }
        Ok(file)
    }

    pub fn open_executable(&self, relative: &str) -> io::Result<File> {
        let file = self.open_file(relative)?;
        if file.metadata()?.mode() & 0o111 == 0 {
            return Err(io::Error::other("installation file is not executable"));
        }
        Ok(file)
    }
}
