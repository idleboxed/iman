// SPDX-License-Identifier: BSD-3-Clause
//! Immutable diagnostic snapshots. No observation or lifecycle changes in the writer.
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::{Path, PathBuf},
};

use std::collections::BTreeMap;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

// Also respects iman startup's RLIMIT_FSIZE, independently for each file.
pub const FILE_LIMIT: usize = 64 * 1024;

pub const BUNDLE_LIMIT: usize = 256 * 1024;
pub const PROPOSAL_LIMIT: usize = 32 * 1024;
pub const PROPOSAL_FILES_LIMIT: usize = 9;
const FILE_NAMES: &[&str] = &[
    "observation.json", "cycle-io.json", "launch.log", "retroarch.log",
    "diag_memory.json", "diag_klog.json", "diag_display.json", "diag_input.json",
    "diag_perf.json", "diag_input_test.json", "diag_uart.json", "diag_uart_ping.json",
];

fn proposal_basename(name: &str) -> Option<&str> {
    let basename = name.strip_prefix("pads-proposal/")?;
    if basename == "summary.json" { return Some(basename); }
    let stem = basename.strip_suffix(".json").or_else(|| basename.strip_suffix(".cfg"))?;
    if stem.len() != 26 || !stem.bytes().enumerate().all(|(index, byte)| {
        if matches!(index, 4 | 9) { byte == b'-' }
        else { byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) }
    }) { return None; }
    Some(basename)
}

/// Own every input before dispatching a worker; collectors remain live.
#[derive(Debug)]
pub struct Bundle {
    pub metadata: Value,
    pub files: BTreeMap<String, Vec<u8>>,
}

impl Bundle {
    pub fn new(metadata: Value) -> Self {
        Self { metadata, files: BTreeMap::new() }
    }

    pub fn json(&mut self, name: &str, value: &impl serde::Serialize) -> io::Result<()> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        self.files.insert(name.into(), bytes);
        Ok(())
    }

    /// Validates the complete manifest and payload before any persistent creation.
    pub fn manifest(&self) -> io::Result<Vec<u8>> {
        if !self.metadata.is_object()
            || !self.files.contains_key("launch.log")
            || self.files.iter().any(|(name, bytes)| {
                (!FILE_NAMES.contains(&name.as_str()) && proposal_basename(name).is_none()) || bytes.len() > FILE_LIMIT
            })
        {
            return Err(io::Error::other("invalid bundle or file size limit exceeded"));
        }
        let proposals: Vec<_> = self.files.iter().filter(|(name, _)| proposal_basename(name).is_some()).collect();
        if proposals.len() > PROPOSAL_FILES_LIMIT || proposals.iter().map(|(_, bytes)| bytes.len()).sum::<usize>() > PROPOSAL_LIMIT
            || (!proposals.is_empty() && (!self.files.contains_key("pads-proposal/summary.json")
                || !self.files.contains_key("diag_input_test.json"))) {
            return Err(io::Error::other("invalid proposal bundle or size limit exceeded"));
        }
        let files: BTreeMap<_, _> = self.files.iter().map(|(name, bytes)| {
            (name, json!({"bytes": bytes.len(), "sha256": format!("{:x}", Sha256::digest(bytes))}))
        }).collect();
        let mut manifest = serde_json::to_vec(&json!({
            "format": "IMAN-CYCLE-1", "metadata": self.metadata, "files": files,
            "publication": "verified-inputs; final synchronization follows manifest publication"
        }))?;
        manifest.push(b'\n');
        if manifest.len() > FILE_LIMIT || self.files.values().map(Vec::len).sum::<usize>() + manifest.len() > BUNDLE_LIMIT {
            return Err(io::Error::other("diagnostic bundle size limit exceeded"));
        }
        Ok(manifest)
    }

    pub fn byte_len(&self) -> io::Result<usize> {
        Ok(self.manifest()?.len() + self.files.values().map(Vec::len).sum::<usize>())
    }
}

fn directory(parent: &File, name: &std::ffi::CStr, exclusive: bool) -> io::Result<File> {
    // SAFETY: borrowed directory FD and terminated constant/validated name.
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
        let error = io::Error::last_os_error();
        if exclusive || error.kind() != io::ErrorKind::AlreadyExists {
            return Err(error);
        }
    }
    let path = PathBuf::from(format!("/proc/self/fd/{}", parent.as_raw_fd()))
        .join(std::ffi::OsStr::from_bytes(name.to_bytes()));
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    if directory.metadata()?.dev() != parent.metadata()?.dev() {
        return Err(io::Error::other("export directory device changed"));
    }
    Ok(directory)
}

fn write_verified(target: &File, name: &str, bytes: &[u8]) -> io::Result<()> {
    let anchored = PathBuf::from(format!("/proc/self/fd/{}", target.as_raw_fd()));
    let partial = anchored.join(format!("{name}.partial"));
    let mut output = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&partial)?;
    let meta = output.metadata()?;
    if !meta.is_file() || meta.nlink() != 1 || meta.dev() != target.metadata()?.dev() {
        return Err(io::Error::other("export file identity refused"));
    }
    output.write_all(bytes)?;
    output.sync_all()?;
    output.seek(SeekFrom::Start(0))?;
    let mut readback = Vec::new();
    output
        .take(FILE_LIMIT as u64 + 1)
        .read_to_end(&mut readback)?;
    if readback != bytes {
        return Err(io::Error::other("export file readback mismatch"));
    }
    // The exclusive run directory is never reused. Competing malicious root is
    // outside the diagnostic contract, as for BOX2's pinned executable inodes.
    std::fs::rename(partial, anchored.join(name))
}

/// Export through the session's held root on every Linux architecture.
/// A replaced path is an error, never an instruction to select a new destination.
pub fn write_root(root: &crate::runtime::bundle::Root, run: &str, bundle: &Bundle) -> io::Result<PathBuf> {
    Ok(root.path().join(write_bundle(root.directory()?, run, bundle)?))
}

/// Export under an already validated directory FD. No platform inference or raw
/// device access. Existing runs are never replaced; failures are not retried.
pub fn write_bundle(card: &File, run: &str, bundle: &Bundle) -> io::Result<PathBuf> {
    if !card.metadata()?.is_dir()
        || run.is_empty()
        || run.len() > 120
        || !run.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        return Err(io::Error::other("invalid export root or run name"));
    }
    let manifest = bundle.manifest()?;
    let mut space = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: borrowed directory FD; the kernel initializes the supplied buffer.
    if unsafe { libc::fstatvfs(card.as_raw_fd(), space.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let space = unsafe { space.assume_init() };
    if (space.f_frsize as u64).saturating_mul(space.f_bavail as u64) < 8 * 1024 * 1024 {
        return Err(io::Error::other("insufficient space for diagnostic export"));
    }
    let name = std::ffi::CString::new(run)?;
    let reports = directory(card, c"iman-reports", false)?;
    let target = directory(&reports, &name, true)?;
    let proposals = if bundle.files.contains_key("pads-proposal/summary.json") {
        Some(directory(&target, c"pads-proposal", true)?)
    } else { None };
    for (name, bytes) in &bundle.files {
        if let Some(basename) = proposal_basename(name) {
            write_verified(proposals.as_ref().unwrap(), basename, bytes)?;
        } else { write_verified(&target, name, bytes)?; }
    }
    if let Some(proposals) = &proposals { proposals.sync_all()?; }
    write_verified(&target, "report.json", &manifest)?;
    target.sync_all()?;
    reports.sync_all()?;
    card.sync_all()?;
    // May flush unrelated earlier writes. The master observer remains active.
    if unsafe { libc::syncfs(card.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Path::new("iman-reports").join(run).join("report.json"))
}
