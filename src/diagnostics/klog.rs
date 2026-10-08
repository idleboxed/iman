// SPDX-License-Identifier: BSD-3-Clause
//! Bounded, non-clearing kernel-ring snapshots; never run a shell or change printk.
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{collections::VecDeque, io, time::Instant};

pub const LIMIT: usize = 16 * 1024;
pub const RECORDS: usize = 8;
pub const INTERVAL_MS: u64 = 5000;
pub const HISTORY_LIMIT: usize = 4;
pub const USB_RECORDS: usize = 32;
pub const USB_EARLY_RECORDS: usize = 16;
pub const REPORT_LIMIT: usize = 32 * 1024;
const TAIL_REPORT_LIMIT: usize = 4096;

#[derive(Clone, Debug, Serialize)]
pub struct RecordInfo {
    /// Kernel-provided timestamp, not converted to the iman session clock.
    pub kernel_us: Option<u64>,
    /// Syslog PRI when supplied by the kernel; severity is PRI & 7.
    pub priority: Option<u8>,
    pub text_truncated: bool,
    pub invalid_utf8: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct UsbRecord {
    /// Session time of retention, not the time the kernel emitted the message.
    pub observed_ms: u64,
    pub text: String,
    pub info: RecordInfo,
    pub first_line_maybe_partial: bool,
    #[serde(skip)]
    fingerprint: [u8; 32],
}

#[derive(Clone, Debug, Default)]
struct UsbCapture {
    matching_lines: usize,
    records: VecDeque<UsbRecord>,
}

#[derive(Clone, Debug, Serialize)]
pub struct UsbReport {
    pub selection: &'static str,
    pub retention: &'static str,
    pub deduplication: &'static str,
    pub last_successful_sample_ms: Option<u64>,
    pub last_matching_sample_ms: Option<u64>,
    /// Repeated snapshots count again; these are not newly emitted messages.
    pub matching_line_observations: u64,
    pub capture_dropped_observations: u64,
    pub retention_evictions: u64,
    pub records: VecDeque<UsbRecord>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub bytes_read: usize,
    pub buffer_full: bool,
    /// Compatibility field: with selection=all this counts every observed line.
    pub matching_lines: usize,
    pub records_dropped: usize,
    pub first_line_maybe_partial: bool,
    pub records: Vec<String>,
    pub record_info: Vec<RecordInfo>,
    #[serde(skip)]
    fingerprints: Vec<[u8; 32]>,
    #[serde(skip)]
    usb: UsbCapture,
}

fn is_usb_related(line: &str) -> bool {
    line.split(|ch: char| !ch.is_ascii_alphanumeric()).any(|token| {
        token.get(..3).is_some_and(|prefix| prefix.eq_ignore_ascii_case("usb") || prefix.eq_ignore_ascii_case("hid"))
            || ["hub", "hcd", "dwc", "dwc2", "dwc3", "ehci", "ohci", "uhci", "xhci"]
                .iter().any(|name| token.eq_ignore_ascii_case(name))
    })
}

pub fn capture_with(read_all: impl FnOnce(&mut [u8]) -> io::Result<usize>) -> io::Result<Snapshot> {
    let mut buffer = vec![0; LIMIT];
    let size = read_all(&mut buffer)?;
    if size > buffer.len() {
        return Err(io::Error::other("invalid kernel log read length"));
    }
    let mut selected = VecDeque::new();
    let mut usb = UsbCapture::default();
    let mut count = 0;
    for raw in buffer[..size].split(|&byte| byte == b'\n') {
        if raw.is_empty() { continue; }
        let line = String::from_utf8_lossy(raw);
        count += 1;
        if selected.len() == RECORDS { selected.pop_front(); }
        let (priority, rest) = line.strip_prefix('<').and_then(|s| s.split_once('>'))
            .and_then(|(pri, rest)| pri.parse::<u8>().ok().filter(|p| *p <= 191).map(|p| (Some(p), rest)))
            .unwrap_or((None, line.as_ref()));
        let kernel_us = rest.trim_start().strip_prefix('[').and_then(|s| s.split_once(']'))
            .and_then(|(stamp, _)| {
                let (seconds, fraction) = stamp.trim().split_once('.')?;
                if fraction.is_empty() || fraction.len() > 6 || !fraction.bytes().all(|b| b.is_ascii_digit()) { return None; }
                seconds.parse::<u64>().ok()?.checked_mul(1_000_000)?
                    .checked_add(fraction.parse::<u64>().ok()? * 10u64.pow(6 - fraction.len() as u32))
            });
        let mut escaped = line.chars().flat_map(char::escape_default);
        let mut record = String::new();
        let mut truncated = false;
        for ch in escaped.by_ref() {
            if record.len() + ch.len_utf8() > 240 { truncated = true; break; }
            record.push(ch);
        }
        let fingerprint: [u8; 32] = Sha256::digest(raw).into();
        let info = RecordInfo { kernel_us, priority, text_truncated: truncated, invalid_utf8: std::str::from_utf8(raw).is_err() };
        // Select from the complete read, before discarding the generic tail.
        if is_usb_related(&line) {
            usb.matching_lines += 1;
            if usb.records.len() == USB_RECORDS { usb.records.remove(USB_EARLY_RECORDS); }
            usb.records.push_back(UsbRecord {
                observed_ms: 0, text: record.clone(), info: info.clone(),
                first_line_maybe_partial: size == LIMIT && count == 1, fingerprint,
            });
        }
        selected.push_back((record, info, fingerprint));
    }
    let mut records = Vec::new();
    let mut record_info = Vec::new();
    let mut fingerprints = Vec::new();
    for (text, info, fingerprint) in selected {
        records.push(text); record_info.push(info); fingerprints.push(fingerprint);
    }
    Ok(Snapshot {
        bytes_read: size, buffer_full: size == LIMIT, matching_lines: count,
        records_dropped: count.saturating_sub(records.len()), first_line_maybe_partial: size == LIMIT,
        records, record_info, fingerprints, usb,
    })
}

#[cfg(target_os = "linux")]
pub fn capture() -> io::Result<Snapshot> {
    capture_with(|buffer| {
        // SYSLOG_ACTION_READ_ALL=3 reads without clearing or consuming /proc/kmsg.
        // SAFETY: writable bounded buffer, kernel receives its exact length.
        let result =
            unsafe { libc::klogctl(3, buffer.as_mut_ptr().cast(), buffer.len() as libc::c_int) };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(result as usize)
        }
    })
}

#[cfg(not(target_os = "linux"))]
pub fn capture() -> io::Result<Snapshot> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "kernel snapshots require Linux",
    ))
}

#[derive(Clone, Debug, Serialize)]
pub struct Sample {
    pub ms: u64,
    pub phase: String,
    pub snapshot: Option<Snapshot>,
    /// Content overlap is not proof of continuity: identical messages may recur.
    pub matching_suffix_prefix_records: Option<usize>,
    pub comparison: &'static str,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub version: u32,
    pub selection: &'static str,
    pub kernel_clock: &'static str,
    pub unseen_records: &'static str,
    pub interval_ms: u64,
    pub samples: u64,
    pub read_errors: u64,
    pub history_dropped: u64,
    pub records_dropped_for_json: u64,
    pub stopped: bool,
    pub error: Option<String>,
    pub sample_us_sum: u64,
    pub sample_us_max: u64,
    pub history: VecDeque<Sample>,
    pub usb: UsbReport,
}

pub struct Monitor {
    report: Report,
    next_ms: u64,
    last_ms: Option<u64>,
    previous: Option<Vec<[u8; 32]>>,
    previous_usb: Vec<[u8; 32]>,
}
impl Default for Monitor {
    fn default() -> Self {
        Self {
            report: Report {
                version: 3, selection: "all", kernel_clock: "kernel_printk_unconverted", unseen_records: "unknown",
                interval_ms: INTERVAL_MS,
                samples: 0,
                read_errors: 0,
                history_dropped: 0,
                records_dropped_for_json: 0,
                stopped: false,
                error: None,
                sample_us_sum: 0,
                sample_us_max: 0,
                history: VecDeque::new(),
                usb: UsbReport {
                    selection: "usb_hub_hid_host_driver_tokens",
                    retention: "first_16_latest_16",
                    deduplication: "raw_content_not_event_identity",
                    last_successful_sample_ms: None,
                    last_matching_sample_ms: None,
                    matching_line_observations: 0,
                    capture_dropped_observations: 0,
                    retention_evictions: 0,
                    records: VecDeque::new(),
                },
            },
            next_ms: 0,
            last_ms: None,
            previous: None,
            previous_usb: Vec::new(),
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
    /// Snapshots can overlap; samples are NOT counts of newly occurring kernel errors.
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
            self.report.error = Some("invalid kernel sample clock or phase".into());
            self.stop();
            return;
        }
        self.last_ms = Some(ms);
        if ms < self.next_ms {
            return;
        }
        self.next_ms = ms.saturating_add(INTERVAL_MS);
        let started = Instant::now();
        let (mut snapshot, error) = match read().and_then(|snapshot| {
            if snapshot.bytes_read > LIMIT
                || snapshot.records.len() > RECORDS
                || snapshot.records.iter().any(|s| s.len() > 240)
                || snapshot.record_info.len() != snapshot.records.len()
                || snapshot.fingerprints.len() != snapshot.records.len()
            {
                return Err(io::Error::other("kernel snapshot exceeds limit"));
            }
            Ok(snapshot)
        }) {
            Ok(snapshot) => (Some(snapshot), None),
            Err(error) => (
                None,
                Some(
                    error
                        .to_string()
                        .chars()
                        .take(120)
                        .map(|c| if c.is_control() { '?' } else { c })
                        .collect(),
                ),
            ),
        };
        let overlap = snapshot.as_ref().and_then(|current| self.previous.as_ref().map(|previous| {
            (1..=previous.len().min(current.fingerprints.len())).rev()
                .find(|&n| previous[previous.len() - n..] == current.fingerprints[..n]).unwrap_or(0)
        }));
        let comparison = if self.report.samples == 0 { "initial_inventory" }
            else if error.is_some() { "read_error" }
            else if self.previous.is_none() { "after_read_error" }
            else { "content_overlap_only" };
        self.previous = snapshot.as_ref().map(|s| s.fingerprints.clone());
        let r = &mut self.report;
        if let Some(current) = snapshot.as_mut() {
            let usb = &mut r.usb;
            usb.last_successful_sample_ms = Some(ms);
            if current.usb.matching_lines > 0 { usb.last_matching_sample_ms = Some(ms); }
            usb.matching_line_observations = usb.matching_line_observations.saturating_add(current.usb.matching_lines as u64);
            usb.capture_dropped_observations = usb.capture_dropped_observations.saturating_add(
                current.usb.matching_lines.saturating_sub(current.usb.records.len()) as u64,
            );
            let fingerprints = current.usb.records.iter().map(|record| record.fingerprint).collect();
            // Keep early boot evidence after it leaves READ_ALL and the generic
            // history. Content deduplication never claims distinct kernel events.
            for mut record in current.usb.records.drain(..) {
                if self.previous_usb.contains(&record.fingerprint)
                    || usb.records.iter().any(|saved| saved.fingerprint == record.fingerprint)
                { continue; }
                if usb.records.len() == USB_RECORDS {
                    usb.records.remove(USB_EARLY_RECORDS);
                    usb.retention_evictions = usb.retention_evictions.saturating_add(1);
                }
                record.observed_ms = ms;
                usb.records.push_back(record);
            }
            self.previous_usb = fingerprints;
        }
        r.samples += 1;
        r.read_errors += u64::from(error.is_some());
        if r.history.len() == HISTORY_LIMIT {
            r.history.pop_front();
            r.history_dropped += 1;
        }
        r.history.push_back(Sample {
            ms,
            phase: phase.into(),
            snapshot,
            matching_suffix_prefix_records: overlap,
            comparison,
            error,
        });
        let us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        r.sample_us_sum = r.sample_us_sum.saturating_add(us);
        r.sample_us_max = r.sample_us_max.max(us);
    }
    pub fn report_json(&self) -> io::Result<String> {
        let mut report = self.report.clone();
        let usb_bytes = serde_json::to_vec(&report.usb).map_err(io::Error::other)?.len();
        loop {
            let text = serde_json::to_string(&report).map_err(io::Error::other)?;
            if text.len() <= REPORT_LIMIT && text.len().saturating_sub(usb_bytes) <= TAIL_REPORT_LIMIT {
                return Ok(text);
            }
            if report.history.len() > 1 {
                report.history.pop_front();
                report.history_dropped += 1;
            } else if let Some(snapshot) = report
                .history
                .front_mut()
                .and_then(|s| s.snapshot.as_mut())
                .filter(|s| !s.records.is_empty())
            {
                snapshot.records.remove(0);
                snapshot.record_info.remove(0);
                snapshot.fingerprints.remove(0);
                report.records_dropped_for_json += 1;
            } else {
                return Err(io::Error::other("kernel report exceeds limit"));
            }
        }
    }
}
