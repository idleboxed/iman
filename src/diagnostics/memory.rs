// SPDX-License-Identifier: BSD-3-Clause
//! Bounded system/manager/child RAM observations. Explicit reads, no files written.
use std::{collections::VecDeque, io, path::PathBuf, time::Instant};

use imanlib::metrics::{parse_stat, read_memory, read_text, MemoryInfo, STAT_LIMIT};
use serde::Serialize;

pub const INTERVAL_MS: u64 = 2000;
pub const HISTORY_LIMIT: usize = 16;
pub const REPORT_LIMIT: usize = 8192;
pub const STATUS_LIMIT: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProcessMemory {
    pub start_ticks: u64,
    /// Kernel estimates, not PSS or a count of exclusively owned physical pages.
    pub rss_bytes: u64,
    pub virtual_bytes: u64,
}

/// Missing/duplicate/malformed fields are errors, not zero resident memory.
pub fn parse_status(text: &str, expected_pid: u32) -> io::Result<(u64, u64)> {
    let bad = || io::Error::other("invalid process memory status");
    if expected_pid == 0 || text.len() > STATUS_LIMIT {
        return Err(bad());
    }
    let (mut pid, mut rss, mut size) = (None, None, None);
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key == "Pid" {
            let value = value.trim().parse::<u32>().map_err(|_| bad())?;
            if pid.replace(value).is_some() {
                return Err(bad());
            }
        } else if matches!(key, "VmRSS" | "VmSize") {
            let mut words = value.split_whitespace();
            let value = words
                .next()
                .ok_or_else(bad)?
                .parse::<u64>()
                .map_err(|_| bad())?
                .checked_mul(1024)
                .ok_or_else(bad)?;
            if words.next() != Some("kB") || words.next().is_some() {
                return Err(bad());
            }
            if (if key == "VmRSS" { &mut rss } else { &mut size })
                .replace(value)
                .is_some()
            {
                return Err(bad());
            }
        }
    }
    if pid != Some(expected_pid) {
        return Err(bad());
    }
    Ok((rss.ok_or_else(bad)?, size.ok_or_else(bad)?))
}

pub use imanlib::metrics::Reading;

#[derive(Clone, Debug, Serialize)]
pub struct ProcessSample {
    pub pid: u32,
    pub reading: Reading<ProcessMemory>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub system: Reading<MemoryInfo>,
    pub manager: ProcessSample,
    pub child: Option<ProcessSample>,
}

pub struct Source {
    proc_root: PathBuf,
}
impl Source {
    /// Construction does not access procfs. Alternate roots support offline fixtures.
    pub fn new(proc_root: PathBuf) -> Self {
        Self { proc_root }
    }
    pub fn system() -> Self {
        Self::new("/proc".into())
    }
    pub fn process(&self, pid: u32) -> io::Result<ProcessMemory> {
        let root = self.proc_root.join(pid.to_string());
        read_process_with(pid, |name| {
            read_text(
                &root.join(name),
                if name == "stat" {
                    STAT_LIMIT
                } else {
                    STATUS_LIMIT
                },
            )
        })
    }
    pub fn sample(&self, manager: u32, child: Option<u32>) -> io::Result<Snapshot> {
        if manager == 0 || child.is_some_and(|p| p == 0 || p == manager) {
            return Err(io::Error::other("invalid memory observation targets"));
        }
        let process = |pid| ProcessSample {
            pid,
            reading: self.process(pid).into(),
        };
        Ok(Snapshot {
            system: read_memory(&self.proc_root.join("meminfo")).into(),
            manager: process(manager),
            child: child.map(process),
        })
    }
}

/// Capture through an explicit reader; check PID/starttime on both sides of status.
pub fn read_process_with(
    pid: u32,
    mut read: impl FnMut(&str) -> io::Result<String>,
) -> io::Result<ProcessMemory> {
    if pid == 0 {
        return Err(io::Error::other("invalid process id"));
    }
    let before = parse_stat(&read("stat")?)?;
    let (rss_bytes, virtual_bytes) = parse_status(&read("status")?, pid)?;
    let after = parse_stat(&read("stat")?)?;
    if before.id != pid || (before.id, before.start_ticks) != (after.id, after.start_ticks) {
        return Err(io::Error::other(
            "process identity changed during memory sample",
        ));
    }
    Ok(ProcessMemory {
        start_ticks: before.start_ticks,
        rss_bytes,
        virtual_bytes,
    })
}

#[derive(Clone, Debug, Serialize)]
pub struct Sample {
    pub ms: u64,
    pub phase: String,
    pub reading: Reading<Snapshot>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub interval_ms: u64,
    pub samples: u64,
    pub history_dropped: u64,
    pub read_errors: u64,
    pub stopped: bool,
    pub error: Option<String>,
    pub available_min_bytes: Option<u64>,
    pub manager_rss_max_bytes: Option<u64>,
    /// Peak of any observed child, not a sum of all children over the session.
    pub child_rss_max_bytes: Option<u64>,
    pub sample_us_sum: u64,
    pub sample_us_max: u64,
    pub history: VecDeque<Sample>,
}

pub struct Monitor {
    report: Report,
    next_ms: u64,
    last_ms: Option<u64>,
}
impl Default for Monitor {
    fn default() -> Self {
        Self {
            report: Report {
                interval_ms: INTERVAL_MS,
                samples: 0,
                history_dropped: 0,
                read_errors: 0,
                stopped: false,
                error: None,
                available_min_bytes: None,
                manager_rss_max_bytes: None,
                child_rss_max_bytes: None,
                sample_us_sum: 0,
                sample_us_max: 0,
                history: VecDeque::new(),
            },
            next_ms: 0,
            last_ms: None,
        }
    }
}
impl Monitor {
    pub fn report(&self) -> &Report {
        &self.report
    }
    pub fn stop(&mut self) {
        self.report.stopped = true;
    }
    /// No catch-up bursts. A failed read becomes a new unknown sample, not stale data.
    pub fn poll(&mut self, ms: u64, phase: &str, read: impl FnOnce() -> io::Result<Snapshot>) {
        if self.report.stopped {
            return;
        }
        if self.last_ms.is_some_and(|last| ms < last)
            || phase.len() > 32
            || phase.is_empty()
            || !phase
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_')
        {
            self.report.error = Some("invalid memory sample clock or phase".into());
            self.stop();
            return;
        }
        self.last_ms = Some(ms);
        if ms < self.next_ms {
            return;
        }
        self.next_ms = ms.saturating_add(INTERVAL_MS);
        let started = Instant::now();
        let reading: Reading<Snapshot> = read().into();
        let r = &mut self.report;
        r.read_errors += u64::from(reading.error.is_some());
        if let Some(snapshot) = &reading.value {
            r.read_errors += u64::from(snapshot.system.error.is_some())
                + u64::from(snapshot.manager.reading.error.is_some())
                + u64::from(
                    snapshot
                        .child
                        .as_ref()
                        .is_some_and(|p| p.reading.error.is_some()),
                );
            if let Some(info) = snapshot.system.value {
                r.available_min_bytes = Some(
                    r.available_min_bytes
                        .map_or(info.available_bytes, |n| n.min(info.available_bytes)),
                );
            }
            if let Some(memory) = &snapshot.manager.reading.value {
                r.manager_rss_max_bytes =
                    Some(r.manager_rss_max_bytes.unwrap_or(0).max(memory.rss_bytes));
            }
            if let Some(memory) = snapshot
                .child
                .as_ref()
                .and_then(|p| p.reading.value.as_ref())
            {
                r.child_rss_max_bytes =
                    Some(r.child_rss_max_bytes.unwrap_or(0).max(memory.rss_bytes));
            }
        }
        if r.history.len() == HISTORY_LIMIT {
            r.history.pop_front();
            r.history_dropped += 1;
        }
        r.history.push_back(Sample {
            ms,
            phase: phase.into(),
            reading,
        });
        r.samples += 1;
        let us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        r.sample_us_sum = r.sample_us_sum.saturating_add(us);
        r.sample_us_max = r.sample_us_max.max(us);
    }
    /// Trim a serialized copy, retaining peaks and the live in-RAM history.
    pub fn report_json(&self) -> io::Result<String> {
        let mut report = self.report.clone();
        loop {
            let text = serde_json::to_string(&report).map_err(io::Error::other)?;
            if text.len() <= REPORT_LIMIT {
                return Ok(text);
            }
            if report.history.pop_front().is_none() {
                return Err(io::Error::other("memory report exceeds limit"));
            }
            report.history_dropped += 1;
        }
    }
}
