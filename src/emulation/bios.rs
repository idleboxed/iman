// SPDX-License-Identifier: BSD-3-Clause
//! Select a platform's firmware directory; cores read its files directly from SD.
use std::{
    ffi::{CStr, CString},
    fs::{File, OpenOptions},
    io,
    os::{fd::{AsRawFd, FromRawFd}, unix::fs::{MetadataExt, OpenOptionsExt}},
    path::{Path, PathBuf},
};

use crate::runtime::paths;

pub(crate) fn valid_platform(platform: &str) -> bool {
    !platform.is_empty() && platform.len() <= 64
        && platform.bytes().next().is_some_and(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        && platform.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b"_-".contains(&b))
}

fn open_directory(parent: &File, name: &CStr) -> io::Result<File> {
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: parent is live, name is NUL-terminated, no creation.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a fresh owned descriptor.
    let file = unsafe { File::from_raw_fd(fd) };
    if file.metadata()?.dev() != parent.metadata()?.dev() {
        return Err(io::Error::other("BIOS directory must be on the same device as the session root"));
    }
    Ok(file)
}

/// Check only the selected directory's ancestry, without opening or listing firmware.
/// An absent BIOS tree is valid. This is not a read-only mount or a core sandbox.
pub fn directory(root: &Path, platform: &str) -> io::Result<Option<PathBuf>> {
    if !valid_platform(platform) {
        return Err(io::Error::other("invalid BIOS platform identifier"));
    }
    let path = root.join(paths::SYSTEM_DIR).join(paths::BIOS_DIR).join(platform);
    let check = || -> io::Result<Option<PathBuf>> {
        let root = OpenOptions::new().read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(root)?;
        let system = open_directory(&root, c"system")?;
        let selected = || -> io::Result<File> {
            let bios = open_directory(&system, c"bios")?;
            open_directory(&bios, &CString::new(platform).expect("validated platform"))
        };
        match selected() {
            Ok(_) => Ok(Some(path)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    };
    check().map_err(|error| io::Error::new(error.kind(),
        format!("Cannot read BIOS: system/bios/{platform}. {error}")))
}
