// SPDX-License-Identifier: BSD-3-Clause
//! Read-only, bounded game CPU diagnostics. No threads, files written or frequency changes.
use std::{
    collections::VecDeque,
    io,
    path::{Path, PathBuf},
    time::Instant,
};

use imanlib::metrics::{read_text, STAT_LIMIT};
use serde::Serialize;

pub const INTERVAL_MS: u64 = 2000;
pub const THREAD_LIMIT: usize = 12;
pub const CPU_LIMIT: usize = 8;
pub const HISTORY_LIMIT: usize = 24;
pub const REPORT_LIMIT: usize = 4096;
const ENUM_LIMIT: usize = 128;

pub use imanlib::metrics::{parse_stat, Task};

#[derive(Clone, Debug, Serialize)]
pub struct Frequency {
    pub cpu: u32,
    /// Kernel scaling report, NOT a measurement of physical clock frequency.
    pub scaling_khz: Option<u64>,
    pub hardware_khz: Option<u64>,
}

pub struct Snapshot {
    /// /proc/PID/stat aggregates the process; /task/TID/stat is per thread.
    pub process: Task,
    pub threads: Vec<Task>,
    pub frequencies: Vec<Frequency>,
    pub threads_limited: bool,
    pub cpus_limited: bool,
    pub read_errors: u32,
}

/// Construction does no I/O. Alternate roots make the real parser/reader testable offline.
pub struct Source {
    proc_root: PathBuf,
    cpu_root: PathBuf,
}
impl Source {
    pub fn new(proc_root: PathBuf, cpu_root: PathBuf) -> Self {
        Self {
            proc_root,
            cpu_root,
        }
    }
    pub fn system() -> Self {
        Self::new("/proc".into(), "/sys/devices/system/cpu".into())
    }
    pub fn sample(&self, pid: u32) -> io::Result<Snapshot> {
        if pid == 0 {
            return Err(io::Error::other("invalid process id"));
        }
        let root = self.proc_root.join(pid.to_string());
        let process = parse_stat(&read_text(&root.join("stat"), STAT_LIMIT)?)?;
        if process.id != pid {
            return Err(io::Error::other("process identity changed"));
        }
        let (tids, threads_limited) = ids(&root.join("task"), "", THREAD_LIMIT)?;
        let mut threads = Vec::new();
        let mut read_errors = 0;
        for tid in tids {
            match read_text(
                &root.join("task").join(tid.to_string()).join("stat"),
                STAT_LIMIT,
            )
            .and_then(|s| parse_stat(&s))
            {
                Ok(task) if task.id == tid => threads.push(task),
                _ => read_errors += 1, // A thread can exit between enumeration and read.
            }
        }
        let (cpus, cpus_limited) = match ids(&self.cpu_root, "cpu", CPU_LIMIT) {
            Ok(v) => v,
            Err(_) => {
                read_errors += 1;
                (Vec::new(), false)
            }
        };
        let mut frequencies = Vec::new();
        for cpu in cpus {
            let root = self.cpu_root.join(format!("cpu{cpu}/cpufreq"));
            let mut khz = |name| match read_text(&root.join(name), STAT_LIMIT)
                .and_then(|s| s.trim().parse::<u64>().map_err(io::Error::other))
            {
                Ok(n) if n > 0 => Some(n),
                _ => {
                    read_errors += 1;
                    None
                }
            };
            frequencies.push(Frequency {
                cpu,
                scaling_khz: khz("scaling_cur_freq"),
                hardware_khz: khz("cpuinfo_cur_freq"),
            });
        }
        let end = parse_stat(&read_text(&root.join("stat"), STAT_LIMIT)?)?;
        if (end.id, end.start_ticks) != (process.id, process.start_ticks) {
            return Err(io::Error::other("process identity changed during sample"));
        }
        Ok(Snapshot {
            process,
            threads,
            frequencies,
            threads_limited,
            cpus_limited,
            read_errors,
        })
    }
}

fn ids(path: &Path, prefix: &str, limit: usize) -> io::Result<(Vec<u32>, bool)> {
    let mut ids = Vec::new();
    let mut limited = false;
    for (i, entry) in path.read_dir()?.enumerate() {
        if i == ENUM_LIMIT {
            limited = true;
            break;
        }
        let name = entry?.file_name();
        if let Some(id) = name
            .to_str()
            .and_then(|s| s.strip_prefix(prefix))
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|s| s.parse::<u32>().ok())
        {
            ids.push(id);
        }
    }
    ids.sort_unstable();
    ids.dedup();
    limited |= ids.len() > limit;
    ids.truncate(limit);
    Ok((ids, limited))
}

#[derive(Debug, Serialize)]
pub struct ThreadTotal {
    pub tid: u32,
    pub start_ticks: u64,
    pub name: String,
    pub observed_ms: u64,
    pub cpu_ticks: u64,
    /// 1000 = 100% of one logical CPU, not percent of the whole machine.
    pub max_busy_permille: u64,
    #[serde(skip)]
    previous: Option<(u64, u64)>,
}
#[derive(Debug, Serialize)]
pub struct Sample {
    pub ms: u64,
    pub dt_ms: u64,
    pub process_ticks: Option<u64>,
    /// In Report.threads order. Null = new/missing/reused thread or a counter gap.
    pub thread_ticks: Vec<Option<u64>>,
    pub frequencies: Vec<Frequency>,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub version: u32,
    pub pid: u32,
    pub clock_ticks_per_second: u64,
    pub interval_ms: u64,
    pub samples: u64,
    pub history_dropped: u64,
    pub read_errors: u64,
    pub threads_limited: bool,
    pub cpus_limited: bool,
    pub sample_us_sum: u128,
    pub sample_us_max: u128,
    pub elapsed_ms: u64,
    pub finished: bool,
    pub error: Option<String>,
    pub threads: Vec<ThreadTotal>,
    pub history: VecDeque<Sample>,
}

pub struct Monitor {
    report: Report,
    previous: Option<(Task, u64)>,
    next_ms: u64,
}
impl Monitor {
    /// Pure constructor; caller acquires CLK_TCK explicitly on the runtime path.
    pub fn new(pid: u32, clock_ticks_per_second: u64) -> io::Result<Self> {
        if pid == 0 || clock_ticks_per_second == 0 || clock_ticks_per_second > 1_000_000 {
            return Err(io::Error::other(
                "invalid CPU monitor identity or tick rate",
            ));
        }
        Ok(Self {
            report: Report {
                version: 1,
                pid,
                clock_ticks_per_second,
                interval_ms: INTERVAL_MS,
                samples: 0,
                history_dropped: 0,
                read_errors: 0,
                threads_limited: false,
                cpus_limited: false,
                sample_us_sum: 0,
                sample_us_max: 0,
                elapsed_ms: 0,
                finished: false,
                error: None,
                threads: Vec::new(),
                history: VecDeque::new(),
            },
            previous: None,
            next_ms: 0,
        })
    }
    pub fn report(&self) -> &Report {
        &self.report
    }
    /// No catch-up burst. Errors stop this diagnostic, never the game or I/O observer.
    pub fn poll(&mut self, ms: u64, sample: impl FnOnce() -> io::Result<Snapshot>) {
        if self.report.finished || self.report.error.is_some() || ms < self.next_ms {
            return;
        }
        self.next_ms = ms.saturating_add(INTERVAL_MS);
        let started = Instant::now();
        if let Err(e) = sample().and_then(|s| self.record(ms, s)) {
            self.report.error = Some(e.to_string().chars().take(120).collect());
        }
        let us = started.elapsed().as_micros();
        self.report.sample_us_sum += us;
        self.report.sample_us_max = self.report.sample_us_max.max(us);
    }
    fn record(&mut self, ms: u64, snapshot: Snapshot) -> io::Result<()> {
        let Snapshot {
            process,
            threads,
            frequencies,
            threads_limited,
            cpus_limited,
            read_errors,
        } = snapshot;
        if process.id != self.report.pid
            || threads.len() > THREAD_LIMIT
            || frequencies.len() > CPU_LIMIT
            || threads
                .iter()
                .enumerate()
                .any(|(i, t)| t.id == 0 || threads[..i].iter().any(|p| p.id == t.id))
        {
            return Err(io::Error::other("invalid CPU snapshot"));
        }
        let (dt_ms, process_ticks) = match &self.previous {
            Some((old, previous_ms)) => {
                if process.start_ticks != old.start_ticks {
                    return Err(io::Error::other("process identity changed"));
                }
                let dt = ms
                    .checked_sub(*previous_ms)
                    .filter(|v| *v > 0)
                    .ok_or_else(|| io::Error::other("invalid sample time"))?;
                let ticks = process
                    .cpu_ticks
                    .checked_sub(old.cpu_ticks)
                    .ok_or_else(|| io::Error::other("process CPU counter decreased"))?;
                (dt, Some(ticks))
            }
            None => (0, None),
        };
        let r = &mut self.report;
        r.threads_limited |= threads_limited;
        r.cpus_limited |= cpus_limited;
        r.read_errors += u64::from(read_errors);
        let mut deltas = vec![None; r.threads.len()];
        for task in threads {
            let index = match r
                .threads
                .iter()
                .position(|t| (t.tid, t.start_ticks) == (task.id, task.start_ticks))
            {
                Some(i) => i,
                None if r.threads.len() < THREAD_LIMIT => {
                    r.threads.push(ThreadTotal {
                        tid: task.id,
                        start_ticks: task.start_ticks,
                        name: task.name.chars().take(16).collect(),
                        observed_ms: 0,
                        cpu_ticks: 0,
                        max_busy_permille: 0,
                        previous: None,
                    });
                    deltas.push(None);
                    // Keep history columns aligned when a worker appears later.
                    for old_sample in &mut r.history {
                        old_sample.thread_ticks.push(None);
                    }
                    r.threads.len() - 1
                }
                None => {
                    r.threads_limited = true;
                    continue;
                }
            };
            let total = &mut r.threads[index];
            // pthread names can change after creation; identity is TID/starttime.
            total.name = task.name.chars().take(16).collect();
            if let Some((ticks, when)) = total.previous {
                if Some(when) == self.previous.as_ref().map(|(_, t)| *t) {
                    if let Some(delta) = task.cpu_ticks.checked_sub(ticks) {
                        total.cpu_ticks = total.cpu_ticks.saturating_add(delta);
                        total.observed_ms = total.observed_ms.saturating_add(dt_ms);
                        let busy = u128::from(delta) * 1_000_000
                            / (u128::from(r.clock_ticks_per_second) * u128::from(dt_ms));
                        total.max_busy_permille = total
                            .max_busy_permille
                            .max(busy.min(u128::from(u64::MAX)) as u64);
                        deltas[index] = Some(delta);
                    } else {
                        r.read_errors += 1;
                    }
                }
            }
            total.previous = Some((task.cpu_ticks, ms));
        }
        if r.history.len() == HISTORY_LIMIT {
            r.history.pop_front();
            r.history_dropped += 1;
        }
        r.history.push_back(Sample {
            ms,
            dt_ms,
            process_ticks,
            thread_ticks: deltas,
            frequencies,
        });
        r.elapsed_ms = ms;
        r.samples += 1;
        self.previous = Some((process, ms));
        Ok(())
    }
    /// Trim only old detailed samples to fit the existing bounded RAM launch log.
    /// Aggregates survive trimming. An error is explicit, never a fake empty success.
    pub fn finish_json(mut self, completed: bool) -> io::Result<String> {
        self.report.finished = completed;
        loop {
            let json = serde_json::to_string(&self.report).map_err(io::Error::other)?;
            if json.len() <= REPORT_LIMIT {
                return Ok(json);
            }
            if self.report.history.pop_front().is_none() {
                return Err(io::Error::other("CPU report exceeds limit"));
            }
            self.report.history_dropped += 1;
        }
    }
}
