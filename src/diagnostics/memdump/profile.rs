// SPDX-License-Identifier: BSD-3-Clause
//! External platform matching and bounded read plans. No physical-memory access.
use std::{fs, io::{self, Read}, os::fd::AsRawFd, path::{Path, PathBuf}};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use imanlib::metrics::Reading;

use crate::runtime::{bundle::Root, paths::MEMORY_PROFILES_DIR};

pub const PROFILE_LIMIT: usize = 16 * 1024;
pub const PROFILE_COUNT_LIMIT: usize = 16;
pub const DUMP_LIMIT: usize = 16 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct Identity {
    pub architecture: String,
    pub model: Reading<String>,
    pub compatible: Vec<String>,
    pub kernel: Reading<String>,
}

/// Platform selection uses DT compatible; other software identity is report-only.
pub trait Platform {
    /// Read bounded software platform identity without accessing physical memory.
    fn identify(&self) -> io::Result<Identity>;
}

pub struct LinuxPlatform { root: PathBuf }
impl LinuxPlatform {
    /// Alternate roots are for offline fixtures, not a runtime path override.
    pub fn new(root: PathBuf) -> Self { Self { root } }
    /// Select normal Linux identity paths without reading them during construction.
    pub fn system() -> Self { Self::new("/".into()) }
}

fn read_identity_file(path: &Path) -> io::Result<String> {
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt};
    let anchor = OpenOptions::new().read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(path)?;
    if !anchor.metadata()?.is_file() {
        return Err(io::Error::other("platform identity source is not a regular file"));
    }
    imanlib::metrics::read_text(&PathBuf::from(format!("/proc/self/fd/{}", anchor.as_raw_fd())), 8192)
}

fn read_identity_label(path: &Path, ending: char) -> io::Result<String> {
    let value = read_identity_file(path)?.trim_end_matches(ending).to_owned();
    if !valid_label(&value) { return Err(io::Error::other("invalid platform identity label")); }
    Ok(value)
}

impl Platform for LinuxPlatform {
    fn identify(&self) -> io::Result<Identity> {
        if !self.root.is_absolute() { return Err(io::Error::other("platform source root must be absolute")); }
        let compatible = read_identity_file(&self.root.join("sys/firmware/devicetree/base/compatible"))?;
        let compatible: Vec<String> = compatible.split('\0').filter(|value| !value.is_empty()).map(str::to_owned).collect();
        if compatible.is_empty() || compatible.iter().any(|value| !valid_label(value)) {
            return Err(io::Error::other("invalid or incomplete platform compatible"));
        }
        let identity = Identity {
            architecture: std::env::consts::ARCH.into(),
            model: read_identity_label(&self.root.join("sys/firmware/devicetree/base/model"), '\0').into(),
            compatible,
            kernel: read_identity_label(&self.root.join("proc/version"), '\n').into(),
        };
        Ok(identity)
    }
}

fn valid_label(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Matcher {
    pub compatible: String,
}
impl Matcher {
    /// Match one exact device-tree compatible value against the observed platform identity.
    pub fn matches(&self, identity: &Identity) -> bool {
        identity.compatible.contains(&self.compatible)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub enabled: bool,
    pub address: String,
    pub length: usize,
    pub unit: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub schema: u32,
    pub description: String,
    #[serde(rename = "match")]
    pub matcher: Matcher,
    pub read: Request,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ReadPlan {
    pub address: u64,
    pub length: usize,
    pub unit: usize,
}

fn parse_address(value: &str) -> io::Result<u64> {
    let hex = value.strip_prefix("0x").filter(|value| !value.is_empty() && value.len() <= 16
        && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| io::Error::other("physical address must be a hexadecimal 0x string"))?;
    u64::from_str_radix(hex, 16).map_err(io::Error::other)
}

impl Profile {
    /// Decode a bounded memory profile and validate its physical-read plan.
    pub fn parse(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() > PROFILE_LIMIT { return Err(io::Error::other("memory profile size limit exceeded")); }
        let profile: Self = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        profile.plan()?;
        Ok(profile)
    }

    /// Validate schema, address alignment and read bounds, returning the requested physical range.
    pub fn plan(&self) -> io::Result<ReadPlan> {
        if self.schema != 1 || !valid_label(&self.description) || !valid_label(&self.matcher.compatible)
            || !matches!(self.read.unit, 1 | 2 | 4) || self.read.length == 0
            || self.read.length > DUMP_LIMIT || self.read.length % self.read.unit != 0
        { return Err(io::Error::other("invalid memory profile match, limits or read request")); }
        let address = parse_address(&self.read.address)?;
        let finish = address.checked_add(self.read.length as u64).ok_or_else(|| io::Error::other("read range overflows"))?;
        if address % self.read.unit as u64 != 0 || finish > i64::MAX as u64 {
            return Err(io::Error::other("read address is out of bounds or unaligned"));
        }
        Ok(ReadPlan { address, length: self.read.length, unit: self.read.unit })
    }
}

#[derive(Debug)]
pub struct Selection {
    pub filename: String,
    pub sha256: String,
    pub profile: Profile,
}

fn read_profile(root: &Root, filename: &str) -> io::Result<Vec<u8>> {
    use std::{ffi::CString, fs::File, os::{fd::FromRawFd, unix::fs::MetadataExt}};
    let directory = root.open_directory(MEMORY_PROFILES_DIR)?;
    let name = CString::new(filename)?;
    // O_PATH avoids device/FIFO open callbacks until type, links and device are checked.
    let descriptor = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if descriptor < 0 { return Err(io::Error::last_os_error()); }
    let anchor = unsafe { File::from_raw_fd(descriptor) };
    let before = anchor.metadata()?;
    if !before.is_file() || before.nlink() != 1 || before.dev() != directory.metadata()?.dev() {
        return Err(io::Error::other("memory profile must be a regular single-link same-device file"));
    }
    let mut file = File::open(format!("/proc/self/fd/{}", anchor.as_raw_fd()))?;
    let mut bytes = Vec::new();
    (&mut file).take(PROFILE_LIMIT as u64 + 1).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() > PROFILE_LIMIT || bytes.len() as u64 != before.len() || before.ino() != after.ino()
        || before.dev() != after.dev() || after.nlink() != 1 || before.len() != after.len()
        || before.mtime_nsec() != after.mtime_nsec() || before.mtime() != after.mtime()
        || before.ctime_nsec() != after.ctime_nsec() || before.ctime() != after.ctime()
    {
        return Err(io::Error::other("memory profile is oversized or changed during reading"));
    }
    Ok(bytes)
}

/// Choose at most one checked profile matching the platform, rejecting ambiguity.
pub fn select(root: &Root, identity: &Identity) -> io::Result<Option<Selection>> {
    let directory = match root.open_directory(MEMORY_PROFILES_DIR) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        result => result?,
    };
    let mut filenames = Vec::new();
    for (index, entry) in fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))?.enumerate() {
        if index >= 64 { return Err(io::Error::other("memory profile directory entry limit exceeded")); }
        let entry = entry?;
        let filename = entry.file_name().into_string().map_err(|_| io::Error::other("non-UTF8 memory profile name"))?;
        if !filename.ends_with(".json") { continue; }
        if filename.len() > 128 || filename.starts_with('.') || !filename.bytes().all(|byte|
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        { return Err(io::Error::other("invalid memory profile filename")); }
        filenames.push(filename);
        if filenames.len() > PROFILE_COUNT_LIMIT { return Err(io::Error::other("memory profile count limit exceeded")); }
    }
    filenames.sort();
    let mut selected = None;
    for filename in filenames {
        let bytes = read_profile(root, &filename)?;
        let profile = Profile::parse(&bytes).map_err(|error| io::Error::other(format!("{filename}: {error}")))?;
        if !profile.matcher.matches(identity) { continue; }
        if selected.is_some() { return Err(io::Error::other("multiple memory profiles match this platform")); }
        selected = Some(Selection { filename, sha256: format!("{:x}", Sha256::digest(&bytes)), profile });
    }
    root.validate()?;
    Ok(selected)
}

impl Selection {
    /// Require the held root and selected profile bytes to remain unchanged.
    pub fn validate(&self, root: &Root) -> io::Result<()> {
        let bytes = read_profile(root, &self.filename)?;
        if format!("{:x}", Sha256::digest(&bytes)) != self.sha256 {
            return Err(io::Error::other("selected memory profile changed"));
        }
        Ok(())
    }
}
