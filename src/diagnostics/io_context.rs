// SPDX-License-Identifier: BSD-3-Clause
//! Passive filesystem, writeback and process-I/O context. No writes or device opens.
use std::{collections::{BTreeMap, VecDeque}, io, os::unix::fs::MetadataExt, path::{Path, PathBuf}, time::Instant};
use imanlib::metrics::{parse_kib_fields, parse_stat, read_text, Reading, MEMINFO_LIMIT, STAT_LIMIT};
use serde::Serialize;

pub const INTERVAL_MS: u64 = 2000;
pub const HISTORY_LIMIT: usize = 16;
pub const REPORT_LIMIT: usize = 16 * 1024;
const PROC_IO_LIMIT: usize = 4096;
const COUNTERS: [&str; 7] = ["rchar", "wchar", "syscr", "syscw", "read_bytes", "write_bytes", "cancelled_write_bytes"];

#[derive(Clone, Debug, Serialize)]
pub struct ProcessIo {
    pub pid: u32,
    pub start_ticks: u64,
    /// Absolute process counters, not SD-only I/O or physical write attribution.
    pub counters: BTreeMap<&'static str, Option<u64>>,
}

/// Read counters between two identity checks; missing fields remain null.
pub fn read_process_with(pid: u32, mut read: impl FnMut(&str) -> io::Result<String>) -> io::Result<ProcessIo> {
    if pid == 0 { return Err(io::Error::other("invalid process id")); }
    let before = parse_stat(&read("stat")?)?;
    let text = read("io")?;
    if text.len() > PROC_IO_LIMIT { return Err(io::Error::other("process I/O exceeds limit")); }
    let mut counters: BTreeMap<_, _> = COUNTERS.into_iter().map(|key| (key, None)).collect();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else { continue; };
        if let Some(slot) = counters.get_mut(key) {
            let value = value.trim().parse::<u64>().map_err(io::Error::other)?;
            if slot.replace(value).is_some() { return Err(io::Error::other("duplicate process I/O counter")); }
        }
    }
    if counters.values().all(Option::is_none) { return Err(io::Error::other("missing process I/O counters")); }
    let after = parse_stat(&read("stat")?)?;
    if before.id != pid || (before.id, before.start_ticks) != (after.id, after.start_ticks) {
        return Err(io::Error::other("process identity changed during I/O sample"));
    }
    Ok(ProcessIo { pid, start_ticks: before.start_ticks, counters })
}

#[derive(Clone, Debug, Serialize)]
pub struct Mount {
    pub device: String,
    pub mount_point: String,
    pub filesystem: String,
    pub mount_options: String,
    pub super_options: String,
}

fn unescape_mount(value: &str) -> io::Result<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            let escape = bytes.get(i + 1..i + 4).ok_or_else(|| io::Error::other("invalid mount escape"))?;
            out.push(match escape {
                b"040" => b' ', b"011" => b'\t', b"012" => b'\n', b"134" => b'\\',
                _ => return Err(io::Error::other("invalid mount escape")),
            });
            i += 4;
        } else { out.push(bytes[i]); i += 1; }
    }
    String::from_utf8(out).map_err(io::Error::other)
}

/// Select the longest containing mount with the actual root's device identity.
/// Ambiguous stacked mounts fail rather than selecting an unrelated filesystem.
pub fn parse_mountinfo(text: &str, root: &Path, device: u64) -> io::Result<Mount> {
    if text.len() > 1024 * 1024 || !root.is_absolute() { return Err(io::Error::other("invalid mountinfo input")); }
    let expected = format!("{}:{}", libc::major(device), libc::minor(device));
    let mut selected: Option<Mount> = None;
    let mut ambiguous = false;
    for line in text.lines() {
        let (left, right) = line.split_once(" - ").ok_or_else(|| io::Error::other("invalid mountinfo row"))?;
        let fields: Vec<_> = left.split_whitespace().collect();
        let tail: Vec<_> = right.split_whitespace().collect();
        if fields.len() < 6 || tail.len() < 3 { return Err(io::Error::other("short mountinfo row")); }
        if fields[2] != expected { continue; }
        let mount_point = unescape_mount(fields[4])?;
        if !root.starts_with(&mount_point) { continue; }
        if [mount_point.as_str(), fields[5], tail[0], tail[2]].iter().any(|s| s.len() > 1024) {
            return Err(io::Error::other("mount context exceeds limit"));
        }
        let length = selected.as_ref().map_or(0, |m| m.mount_point.len());
        if mount_point.len() > length {
            selected = Some(Mount { device: expected.clone(), mount_point, filesystem: tail[0].into(),
                mount_options: fields[5].into(), super_options: tail[2].into() });
            ambiguous = false;
        } else if mount_point.len() == length { ambiguous = true; }
    }
    if ambiguous { return Err(io::Error::other("ambiguous filesystem mount")); }
    selected.ok_or_else(|| io::Error::other("application filesystem mount unavailable"))
}

#[derive(Clone, Debug, Serialize)]
pub struct Environment {
    pub ms: u64,
    pub mount: Reading<Mount>,
    pub writeback: BTreeMap<&'static str, Reading<u64>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PendingWrites {
    /// System-wide dirty/writeback RAM, not bytes belonging only to the SD.
    pub dirty_bytes: Option<u64>,
    pub writeback_bytes: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Sample {
    pub ms: u64,
    pub phase: String,
    pub memory: Reading<PendingWrites>,
    pub manager_pid: u32,
    pub manager: Reading<ProcessIo>,
    pub child_pid: Option<u32>,
    pub child: Option<Reading<ProcessIo>>,
}

pub struct Source {
    proc_root: PathBuf,
    application_root: PathBuf,
}
impl Source {
    /// Dependency injection is for offline fixtures, not a production path override.
    pub fn new(proc_root: PathBuf, application_root: PathBuf) -> Self { Self { proc_root, application_root } }
    pub fn system(root: &Path) -> Self { Self::new("/proc".into(), root.into()) }
    pub fn environment(&self, ms: u64) -> Environment {
        let mount = (|| {
            let root = self.application_root.canonicalize()?;
            let before = std::fs::metadata(&root)?;
            let text = read_text(&self.proc_root.join("self/mountinfo"), 1024 * 1024)?;
            let mount = parse_mountinfo(&text, &root, before.dev())?;
            let after = std::fs::metadata(&root)?;
            if (before.dev(), before.ino()) != (after.dev(), after.ino()) {
                return Err(io::Error::other("application root changed during mount sample"));
            }
            Ok(mount)
        })().into();
        let writeback = ["dirty_background_bytes", "dirty_background_ratio", "dirty_bytes", "dirty_ratio",
            "dirty_expire_centisecs", "dirty_writeback_centisecs"].into_iter().map(|name| {
            let value = read_text(&self.proc_root.join("sys/vm").join(name), 32)
                .and_then(|s| s.trim().parse::<u64>().map_err(io::Error::other));
            (name, value.into())
        }).collect();
        Environment { ms, mount, writeback }
    }
    pub fn sample(&self, ms: u64, phase: &str, manager: u32, child: Option<u32>) -> Sample {
        let process = |pid| read_process_with(pid, |name| read_text(&self.proc_root.join(pid.to_string()).join(name),
            if name == "stat" { STAT_LIMIT } else { PROC_IO_LIMIT })).into();
        let memory = read_text(&self.proc_root.join("meminfo"), MEMINFO_LIMIT).and_then(|text| {
            let [dirty_bytes, writeback_bytes] = parse_kib_fields(&text, ["Dirty", "Writeback"])?;
            Ok(PendingWrites { dirty_bytes, writeback_bytes })
        }).into();
        Sample { ms, phase: phase.into(), memory, manager_pid: manager, manager: process(manager),
            child_pid: child, child: child.map(process) }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub version: u32,
    pub clock: &'static str,
    pub scope: &'static str,
    pub interval_ms: u64,
    pub environment: Option<Environment>,
    pub samples: u64,
    pub read_errors: u64,
    pub history_dropped: u64,
    pub sample_us_sum: u64,
    pub sample_us_max: u64,
    pub stopped: bool,
    pub error: Option<String>,
    pub history: VecDeque<Sample>,
}

pub struct Monitor {
    source: Source,
    report: Report,
    last_ms: Option<u64>,
    next_ms: u64,
}
impl Monitor {
    pub fn new(source: Source) -> Self {
        Self { source, last_ms: None, next_ms: 0, report: Report { version: 1, clock: "iman_session",
            scope: "session", interval_ms: INTERVAL_MS, environment: None, samples: 0, read_errors: 0,
            history_dropped: 0, sample_us_sum: 0, sample_us_max: 0, stopped: false, error: None,
            history: VecDeque::new() } }
    }
    pub fn report(&self) -> &Report { &self.report }
    pub fn stop(&mut self) { self.report.stopped = true; }
    pub fn poll(&mut self, ms: u64, phase: &str, manager: u32, child: Option<u32>) {
        if self.report.stopped { return; }
        if self.last_ms.is_some_and(|last| ms < last) || phase.is_empty() || phase.len() > 32
            || !phase.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || manager == 0 || child.is_some_and(|pid| pid == 0 || pid == manager)
        {
            self.report.error = Some("invalid I/O context clock, phase or process".into());
            self.stop();
            return;
        }
        self.last_ms = Some(ms);
        if ms < self.next_ms { return; }
        self.next_ms = ms.saturating_add(INTERVAL_MS);
        let started = Instant::now();
        if self.report.environment.is_none() {
            let environment = self.source.environment(ms);
            self.report.read_errors += u64::from(environment.mount.error.is_some())
                + environment.writeback.values().filter(|v| v.error.is_some()).count() as u64;
            self.report.environment = Some(environment);
        }
        let sample = self.source.sample(ms, phase, manager, child);
        let r = &mut self.report;
        r.read_errors += u64::from(sample.memory.error.is_some()) + u64::from(sample.manager.error.is_some())
            + u64::from(sample.child.as_ref().is_some_and(|s| s.error.is_some()));
        r.samples += 1;
        if r.history.len() == HISTORY_LIMIT { r.history.pop_front(); r.history_dropped += 1; }
        r.history.push_back(sample);
        let us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        r.sample_us_sum = r.sample_us_sum.saturating_add(us);
        r.sample_us_max = r.sample_us_max.max(us);
    }
    pub fn report_json(&self) -> io::Result<String> {
        let mut report = self.report.clone();
        loop {
            let text = serde_json::to_string(&report).map_err(io::Error::other)?;
            if text.len() <= REPORT_LIMIT { return Ok(text); }
            if report.history.len() <= 1 { return Err(io::Error::other("I/O context report exceeds limit")); }
            report.history.pop_front();
            report.history_dropped += 1;
        }
    }
}
