// SPDX-License-Identifier: BSD-3-Clause
//! Explicit preparation, immutable reporting, and one active attempt at normal session end.
pub mod profile;
pub mod reader;

use std::{fs::OpenOptions, io::{self, Write}, os::{fd::AsRawFd, unix::fs::OpenOptionsExt}, path::Path};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::runtime::bundle::Root;
use profile::{Identity, LinuxPlatform, Platform, ReadPlan, Selection};
use reader::{LinuxReader, Mapping, Reader};

pub const REPORT_FILE: &str = "diag_memdump.json";
pub const DATA_FILE: &str = "memory-dump.bin";
const ATTEMPT_FILE: &str = "iman-memory-dump.attempt";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State { Unprepared, Ready, Skipped, Reading, Captured, Failed }

#[derive(Debug, Serialize)]
pub struct Report {
    pub version: u32,
    pub policy: &'static str,
    pub state: State,
    pub reason: Option<String>,
    pub identity: Option<Identity>,
    pub profile: Option<String>,
    pub profile_sha256: Option<String>,
    pub description: Option<String>,
    pub plan: Option<ReadPlan>,
    pub attempts: u32,
    pub method: Option<&'static str>,
    pub mapping: Option<Mapping>,
    pub ms: Option<u64>,
    pub captured_bytes: usize,
    pub complete: bool,
    pub sha256: Option<String>,
    pub storage: &'static str,
}

pub struct Monitor {
    report: Report,
    selection: Option<Selection>,
    bytes: Vec<u8>,
    platform: Box<dyn Platform>,
    reader: Box<dyn Reader>,
}
impl Default for Monitor {
    fn default() -> Self { Self::new(LinuxPlatform::system(), LinuxReader::system()) }
}
impl Monitor {
    /// Constructing the monitor has no filesystem, process or hardware effects.
    pub fn new(platform: impl Platform + 'static, reader: impl Reader + 'static) -> Self {
        Self {
            report: Report { version: 1, policy: "normal_session_end_once_external_profile", state: State::Unprepared,
                reason: None, identity: None, profile: None, profile_sha256: None, description: None, plan: None,
                attempts: 0, method: None, mapping: None, ms: None, captured_bytes: 0,
                complete: false, sha256: None, storage: "ram_not_export_confirmation" },
            selection: None, bytes: Vec::new(), platform: Box::new(platform), reader: Box::new(reader),
        }
    }
    pub fn report(&self) -> &Report { &self.report }
    pub fn bytes(&self) -> &[u8] { &self.bytes }
    pub fn is_ready(&self) -> bool { self.report.state == State::Ready }

    pub fn skip(&mut self, reason: &str) {
        if matches!(self.report.state, State::Unprepared | State::Ready) {
            self.report.state = State::Skipped;
            self.report.reason = Some(reason.chars().filter(|value| !value.is_control()).take(512).collect());
        }
    }

    /// Preparation reads software identity and guarded JSON only, never /dev/mem.
    pub fn prepare(&mut self, root: &Root) {
        if self.report.state != State::Unprepared { return; }
        let identity = match self.platform.identify() {
            Ok(identity) => identity,
            Err(error) => { self.skip(&format!("platform identity unavailable: {error}")); return; }
        };
        let selection = profile::select(root, &identity);
        self.report.identity = Some(identity);
        let selected = match selection {
            Ok(Some(selected)) => selected,
            Ok(None) => { self.skip("no matching memory profile"); return; }
            Err(error) => { self.skip(&format!("profile selection refused: {error}")); return; }
        };
        self.report.profile = Some(selected.filename.clone());
        self.report.profile_sha256 = Some(selected.sha256.clone());
        self.report.description = Some(selected.profile.description.clone());
        self.report.plan = match selected.profile.plan() {
            Ok(plan) => Some(plan),
            Err(error) => { self.skip(&format!("memory read plan refused: {error}")); return; }
        };
        if !selected.profile.read.enabled {
            self.skip("physical reading disabled in selected profile");
            return;
        }
        self.selection = Some(selected);
        self.report.state = State::Ready;
    }

    /// Recheck the frozen selection before presenting the start status. Never reselect/fallback.
    pub fn validate(&mut self, root: &Root) {
        if !self.is_ready() { return; }
        let result = (|| {
            let frozen = self.selection.as_ref().ok_or_else(|| io::Error::other("memory profile was not prepared"))?;
            frozen.validate(root)?;
            let identity = self.platform.identify()?;
            if !frozen.profile.matcher.matches(&identity) {
                return Err(io::Error::other("platform compatible no longer matches"));
            }
            let current = profile::select(root, &identity)?;
            if current.as_ref().is_none_or(|selected| selected.filename != frozen.filename || selected.sha256 != frozen.sha256) {
                return Err(io::Error::other("memory profile selection changed"));
            }
            Ok(identity)
        })();
        match result {
            Ok(identity) => self.report.identity = Some(identity),
            Err(error) => self.skip(&format!("pre-read validation refused: {error}")),
        }
    }

    pub fn status(&self) -> String {
        let profile = self.report.profile.as_deref().unwrap_or("none");
        match self.report.state {
            State::Unprepared => "Memory dump: not prepared".into(),
            State::Ready => format!("Memory dump: ready, profile {profile}"),
            State::Reading => format!("Memory dump: reading, profile {profile}, {} bytes",
                self.report.plan.map_or(0, |plan| plan.length)),
            State::Captured => format!("Memory dump: captured {} bytes in RAM", self.report.captured_bytes),
            State::Skipped => format!("Memory dump: skipped - {}", self.report.reason.as_deref().unwrap_or("unavailable")),
            State::Failed => format!("Memory dump: failed - {}", self.report.reason.as_deref().unwrap_or("incomplete reading")),
        }
    }

    pub fn start_status(&self) -> String {
        format!("Memory dump: reading, profile {}, {} bytes",
            self.report.profile.as_deref().unwrap_or("none"), self.report.plan.map_or(0, |plan| plan.length))
    }

    /// The caller must have presented the start status and stopped/reaped all application children.
    pub fn capture_once(&mut self, parent: &Path, ms: u64) {
        if !self.is_ready() { return; }
        let Some(plan) = self.report.plan else { self.skip("memory read plan is unavailable"); return; };
        if let Err(error) = claim_attempt(parent) { self.skip(&format!("attempt guard refused: {error}")); return; }
        self.report.state = State::Reading;
        self.report.attempts = 1;
        self.report.method = Some(self.reader.method());
        self.report.ms = Some(ms);
        match self.reader.read(&plan) {
            Ok(capture) if capture.bytes.len() <= plan.length && capture.bytes.len() % plan.unit == 0 => {
                self.bytes = capture.bytes;
                self.report.mapping = capture.mapping;
                self.report.captured_bytes = self.bytes.len();
                self.report.sha256 = (!self.bytes.is_empty()).then(|| format!("{:x}", Sha256::digest(&self.bytes)));
                self.report.complete = capture.error.is_none() && self.bytes.len() == plan.length;
                self.report.state = if self.report.complete { State::Captured } else { State::Failed };
                self.report.reason = capture.error.or_else(|| (!self.report.complete).then(|| "incomplete memory reading".into()))
                    .map(|reason| reason.chars().filter(|value| !value.is_control()).take(512).collect());
            }
            Ok(_) => { self.report.state = State::Failed; self.report.reason = Some("reader returned invalid byte count".into()); }
            Err(error) => { self.report.state = State::Failed;
                self.report.reason = Some(error.to_string().chars().filter(|value| !value.is_control()).take(512).collect()); }
        }
    }
}

fn claim_attempt(parent: &Path) -> io::Result<()> {
    let directory = crate::runtime::bundle::directory(parent)?;
    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::fstatfs(directory.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { filesystem.assume_init() }.f_type as u64 != libc::TMPFS_MAGIC as u64 {
        return Err(io::Error::other("memory attempt marker requires tmpfs"));
    }
    let mut marker = OpenOptions::new().write(true).create_new(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).mode(0o600)
        .open(format!("/proc/self/fd/{}/{ATTEMPT_FILE}", directory.as_raw_fd()))?;
    marker.write_all(b"One physical-memory attempt reserved; do not reset automatically.\n")?;
    // RAM-only guard deliberately persists after success/failure; no SSD write or fsync.
    Ok(())
}
