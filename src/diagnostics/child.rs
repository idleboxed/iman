// SPDX-License-Identifier: BSD-3-Clause
//! Bounded owner-submitted reports on the manager clock. Never opens devices.
use serde::Serialize;
use std::{collections::VecDeque, io};

pub trait Payload: Clone + Serialize {
    fn valid(&self) -> bool;
}

pub const REPORT_LIMIT: usize = 8192;
pub const SAMPLE_LIMIT: usize = 6144;
pub const HISTORY_LIMIT: usize = 8;
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Clock {
    ImanSession,
    ProbeSession,
}

#[derive(Clone, Serialize)]
pub struct Entry<T> {
    pub pid: u32,
    pub begin_ms: u64,
    pub end_ms: u64,
    pub phase: String,
    pub data: T,
}

#[derive(Clone, Serialize)]
pub struct Report<T> {
    pub version: u32,
    pub clock: Clock,
    pub received: u64,
    pub missing: u64,
    pub rejected: u64,
    pub history_dropped: u64,
    pub stopped: bool,
    pub history: VecDeque<Entry<T>>,
}

pub struct Monitor<T> {
    report: Report<T>,
    expected: Option<(u32, u64, bool)>,
    last_ms: u64,
}
impl<T: Payload> Default for Monitor<T> {
    fn default() -> Self {
        Self {
            report: Report {
                version: 1,
                clock: Clock::ImanSession,
                received: 0,
                missing: 0,
                rejected: 0,
                history_dropped: 0,
                stopped: false,
                history: VecDeque::new(),
            },
            expected: None,
            last_ms: 0,
        }
    }
}
impl<T: Payload> Monitor<T> {
    pub fn with_clock(clock: Clock) -> Self {
        let mut monitor = Self::default();
        monitor.report.clock = clock;
        monitor
    }
    pub fn report(&self) -> &Report<T> {
        &self.report
    }

    pub fn begin_gui(&mut self, pid: u32, ms: u64) {
        self.end_gui();
        if !self.report.stopped && pid != 0 {
            self.expected = Some((pid, ms, false));
        }
    }
    pub fn end_gui(&mut self) {
        if self
            .expected
            .take()
            .is_some_and(|(_, _, received)| !received)
        {
            self.report.missing += 1;
        }
    }
    pub fn submit_gui(&mut self, pid: u32, ms: u64, data: T) -> io::Result<()> {
        let Some((expected, begin, false)) = self.expected else {
            self.report.rejected += 1;
            return Err(io::Error::other(
                "GUI report was not requested or already received",
            ));
        };
        if expected != pid {
            self.report.rejected += 1;
            return Err(io::Error::other("GUI report identity mismatch"));
        }
        self.record(pid, begin, ms, "gui", data)?;
        self.expected = Some((pid, begin, true));
        Ok(())
    }
    /// Also used by the explicitly launched KMS probe with its own session clock.
    pub fn record(
        &mut self,
        pid: u32,
        begin_ms: u64,
        end_ms: u64,
        phase: &str,
        data: T,
    ) -> io::Result<()> {
        let valid = !self.report.stopped
            && pid != 0
            && begin_ms <= end_ms
            && end_ms >= self.last_ms
            && !phase.is_empty()
            && phase.len() <= 32
            && phase
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_')
            && data.valid()
            && serde_json::to_vec(&data).is_ok_and(|v| v.len() <= SAMPLE_LIMIT);
        if !valid {
            self.report.rejected += 1;
            return Err(io::Error::other(
                "invalid, oversized or stopped owner report",
            ));
        }
        self.last_ms = end_ms;
        self.report.received += 1;
        if self.report.history.len() == HISTORY_LIMIT {
            self.report.history.pop_front();
            self.report.history_dropped += 1;
        }
        self.report.history.push_back(Entry {
            pid,
            begin_ms,
            end_ms,
            phase: phase.into(),
            data,
        });
        Ok(())
    }
    pub fn stop(&mut self) {
        self.end_gui();
        self.report.stopped = true;
    }
    pub fn report_json(&self) -> io::Result<String> {
        let mut report = self.report.clone();
        loop {
            let json = serde_json::to_string(&report).map_err(io::Error::other)?;
            if json.len() <= REPORT_LIMIT {
                return Ok(json);
            }
            if report.history.pop_front().is_none() {
                return Err(io::Error::other("owner report exceeds limit"));
            }
            report.history_dropped += 1;
        }
    }
}
