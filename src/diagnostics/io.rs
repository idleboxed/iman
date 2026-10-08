// SPDX-License-Identifier: BSD-3-Clause
//! Optional block-I/O observation. No block devices, mount changes or sync calls.
use imanlib::{filesystem::read_filesystem, metrics::read_text};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde::Serialize;

pub const REPORT_LIMIT: usize = 64 * 1024;
pub const HISTORY_LIMIT: usize = 60;
pub const INPUT_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DiskStats {
    pub name: String,
    pub major: u32,
    pub minor: u32,
    pub read_ios: u64,
    pub read_sectors: u64,
    pub read_ms: u64,
    pub write_ios: u64,
    pub write_sectors: u64,
    pub write_ms: u64,
    pub in_flight: u64,
}

/// Select at most two explicitly named devices; never guess a host disk.
pub fn device_names(value: &str) -> io::Result<Vec<String>> {
    let names: Vec<_> = value.split(',').map(str::to_owned).collect();
    if names.is_empty()
        || names.len() > 2
        || names.iter().any(|s| {
            s.is_empty()
                || s.len() > 32
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        })
        || (names.len() == 2 && names[0] == names[1])
    {
        return Err(io::Error::other(
            "expected one or two distinct block device names",
        ));
    }
    Ok(names)
}

/// Follow the application's filesystem, never guess another desktop disk.
pub fn filesystem_devices(root: &Path) -> io::Result<Vec<String>> {
    use std::os::unix::fs::MetadataExt;
    let directory = OpenOptions::new().read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(root)?;
    partition_devices(Path::new("/sys"), directory.metadata()?.dev())
}

/// Resolve a verified filesystem device to its partition and parent disk.
/// The injectable sysfs root is for offline fixtures, never a CLI override.
pub fn partition_devices(sysfs: &Path, device: u64) -> io::Result<Vec<String>> {
    let sysfs = sysfs.canonicalize()?;
    let devices = sysfs.join("devices").canonicalize()?;
    let major = libc::major(device);
    let minor = libc::minor(device);
    let expected = format!("{major}:{minor}");
    let partition = sysfs.join("dev/block").join(&expected).canonicalize()?;
    if !partition.starts_with(&devices)
        || sysfs_value(&partition.join("dev"))? != expected
        || !sysfs_value(&partition.join("partition"))?
            .parse::<u32>()
            .ok()
            .is_some_and(|number| number > 0)
    {
        return Err(io::Error::other("mounted partition sysfs identity mismatch"));
    }
    let disk = partition.parent().ok_or_else(|| io::Error::other("partition has no parent disk"))?;
    let disk_dev = sysfs_value(&disk.join("dev"))?;
    let valid_dev = disk_dev.split_once(':').is_some_and(|(major, minor)| {
        !major.is_empty() && !minor.is_empty()
            && major.bytes().all(|b| b.is_ascii_digit())
            && minor.bytes().all(|b| b.is_ascii_digit())
            && major.parse::<u32>().is_ok() && minor.parse::<u32>().is_ok()
    });
    if !valid_dev || sysfs.join("dev/block").join(&disk_dev).canonicalize()? != disk {
        return Err(io::Error::other("parent disk sysfs identity mismatch"));
    }
    match disk.join("partition").symlink_metadata() {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
        Ok(_) => return Err(io::Error::other("partition parent is not a whole disk")),
    }
    let name = |path: &Path| {
        path.file_name().and_then(|s| s.to_str()).map(str::to_owned)
            .ok_or_else(|| io::Error::other("invalid sysfs block device name"))
    };
    device_names(&format!("{},{}", name(disk)?, name(&partition)?))
}

fn sysfs_value(path: &Path) -> io::Result<String> {
    let file = OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("sysfs attribute is not a regular file"));
    }
    let mut value = String::new();
    file.take(65).read_to_string(&mut value)?;
    if value.len() > 64 || !value.ends_with('\n') {
        return Err(io::Error::other("invalid or oversized sysfs attribute"));
    }
    value.pop();
    Ok(value)
}

/// Linux 4.4's eleven counters and newer appended fields are accepted.
fn valid_phase(phase: &str) -> bool {
    ["session_start", "gui_start", "gui", "gui_exit", "input_test", "gui_diagnostics", "gui_error",
        "game_start", "game", "game_exit", "session_end", "initializing", "starting_gui",
        "game_cleanup", "display_reclaim", "post_game", "diagnostic_export"].contains(&phase)
}

/// Missing/malformed/duplicate target rows are errors, never zero activity.
pub fn parse_diskstats(text: &str, names: &[String]) -> io::Result<Vec<DiskStats>> {
    if text.len() > INPUT_LIMIT || device_names(&names.join(","))? != names {
        return Err(io::Error::other("invalid diskstats input"));
    }
    names
        .iter()
        .map(|name| {
            let mut rows = text.lines().filter_map(|line| {
                let fields: Vec<_> = line.split_ascii_whitespace().collect();
                (fields.get(2) == Some(&name.as_str())).then_some(fields)
            });
            let fields = rows
                .next()
                .ok_or_else(|| io::Error::other(format!("device missing: {name}")))?;
            if rows.next().is_some() || fields.len() < 14 {
                return Err(io::Error::other("duplicate or short diskstats row"));
            }
            let number = |s: &str| s.parse::<u64>().map_err(io::Error::other);
            // Validate all counters, including fields not currently reported.
            let counters: Vec<_> = fields[3..]
                .iter()
                .map(|s| number(s))
                .collect::<io::Result<_>>()?;
            Ok(DiskStats {
                name: name.clone(),
                major: fields[0].parse().map_err(io::Error::other)?,
                minor: fields[1].parse().map_err(io::Error::other)?,
                read_ios: counters[0],
                read_sectors: counters[2],
                read_ms: counters[3],
                write_ios: counters[4],
                write_sectors: counters[6],
                write_ms: counters[7],
                in_flight: counters[8],
            })
        })
        .collect()
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Writes {
    pub ios: u64,
    pub bytes: u64,
    /// Sum of request times, not wall-clock busy time or NAND wear.
    pub request_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Sample {
    pub elapsed_ms: u64,
    pub interval_ms: u64,
    /// Lifecycle phase at the end of the interval, not attribution to a process.
    pub phase: String,
    pub interval_phase: String,
    pub reads: Vec<Writes>,
    pub writes: Vec<Writes>,
    pub in_flight: Vec<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PhaseWrites {
    pub elapsed_ms: u64,
    pub reads: Vec<Writes>,
    pub writes: Vec<Writes>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub version: u32,
    pub pid: u32,
    pub sector_bytes: u32,
    pub elapsed_ms: u64,
    pub baseline_ms: Option<u64>,
    pub baseline: Vec<DiskStats>,
    pub latest: Vec<DiskStats>,
    /// In baseline device order. Whole disk and partition must NOT be summed.
    pub read_totals: Vec<Writes>,
    pub totals: Vec<Writes>,
    /// Bounded interval aggregates survive history eviction. They are not
    /// attribution of buffered I/O to a process or to the cause of a write.
    pub phases: BTreeMap<String, PhaseWrites>,
    pub samples_seen: u64,
    pub samples_dropped: u64,
    pub history: VecDeque<Sample>,
    pub activity: VecDeque<Sample>,
    pub activity_dropped: u64,
    pub finished: bool,
    pub error: Option<String>,
}

/// Pure bounded accounting; constructors do not open files or start threads.
pub struct Recorder {
    names: Vec<String>,
    report: Report,
}

impl Recorder {
    pub fn new(names: Vec<String>) -> io::Result<Self> {
        device_names(&names.join(","))?;
        Ok(Self {
            names,
            report: Report {
                version: 2,
                pid: std::process::id(),
                sector_bytes: 512,
                elapsed_ms: 0,
                baseline_ms: None,
                baseline: Vec::new(),
                latest: Vec::new(),
                read_totals: Vec::new(),
                totals: Vec::new(),
                phases: BTreeMap::new(),
                samples_seen: 0,
                samples_dropped: 0,
                history: VecDeque::new(),
                activity: VecDeque::new(),
                activity_dropped: 0,
                finished: false,
                error: None,
            },
        })
    }

    pub fn report(&self) -> &Report {
        &self.report
    }

    /// Commit a complete snapshot only after validating every selected device.
    pub fn sample(&mut self, elapsed_ms: u64, phase: &str, text: &str) -> io::Result<()> {
        if self.report.finished || self.report.error.is_some() {
            return Err(io::Error::other("monitor is stopped"));
        }
        if !valid_phase(phase) {
            return Err(io::Error::other("invalid lifecycle phase"));
        }
        let interval_ms = elapsed_ms
            .checked_sub(self.report.elapsed_ms)
            .ok_or_else(|| io::Error::other("monotonic time moved backwards"))?;
        let latest = parse_diskstats(text, &self.names)?;
        let subtract = |a: u64, b: u64| {
            a.checked_sub(b).ok_or_else(|| {
                io::Error::other("disk counter decreased: reset or wrap; observation stopped")
            })
        };
        let read_delta = |now: &DiskStats, old: &DiskStats| -> io::Result<Writes> {
            Ok(Writes {
                ios: subtract(now.read_ios, old.read_ios)?,
                bytes: subtract(now.read_sectors, old.read_sectors)?.checked_mul(512)
                    .ok_or_else(|| io::Error::other("read byte counter overflow"))?,
                request_ms: subtract(now.read_ms, old.read_ms)?,
            })
        };
        let reads = if self.report.latest.is_empty() { vec![Writes::default(); latest.len()] } else {
            latest.iter().zip(&self.report.latest).map(|(now, old)| read_delta(now, old))
                .collect::<io::Result<Vec<_>>>()?
        };
        let writes: Vec<_> = latest
            .iter()
            .zip(&self.report.latest)
            .map(|(now, old)| {
                if (now.major, now.minor) != (old.major, old.minor) {
                    return Err(io::Error::other("block device identity changed"));
                }
                Ok(Writes {
                    ios: subtract(now.write_ios, old.write_ios)?,
                    bytes: subtract(now.write_sectors, old.write_sectors)?
                        .checked_mul(512)
                        .ok_or_else(|| io::Error::other("write byte counter overflow"))?,
                    request_ms: subtract(now.write_ms, old.write_ms)?,
                })
            })
            .collect::<io::Result<_>>()?;
        let writes = if self.report.latest.is_empty() {
            self.report.baseline_ms = Some(elapsed_ms);
            self.report.baseline = latest.clone();
            self.report.totals = vec![Writes::default(); latest.len()];
            vec![Writes::default(); latest.len()]
        } else {
            writes
        };
        // Total subtraction also checks bytes for overflow across the whole run.
        let totals = latest
            .iter()
            .zip(&self.report.baseline)
            .map(|(now, base)| {
                Ok(Writes {
                    ios: subtract(now.write_ios, base.write_ios)?,
                    bytes: subtract(now.write_sectors, base.write_sectors)?
                        .checked_mul(512)
                        .ok_or_else(|| io::Error::other("total byte counter overflow"))?,
                    request_ms: subtract(now.write_ms, base.write_ms)?,
                })
            })
            .collect::<io::Result<_>>()?;
        let read_totals = latest.iter().zip(&self.report.baseline)
            .map(|(now, base)| read_delta(now, base)).collect::<io::Result<Vec<_>>>()?;
        let interval_phase = self
            .report
            .history
            .back()
            .map_or(phase, |s| s.phase.as_str())
            .to_owned();
        if !self.report.latest.is_empty() {
            let aggregate = self
                .report
                .phases
                .entry(interval_phase.clone())
                .or_insert_with(|| PhaseWrites {
                    elapsed_ms: 0,
                    reads: vec![Writes::default(); latest.len()],
                    writes: vec![Writes::default(); latest.len()],
                });
            // Each sum is a subset of the already checked whole-run totals.
            aggregate.elapsed_ms += interval_ms;
            for (sum, delta) in aggregate.reads.iter_mut().zip(&reads) {
                sum.ios += delta.ios;
                sum.bytes += delta.bytes;
                sum.request_ms += delta.request_ms;
            }
            for (sum, delta) in aggregate.writes.iter_mut().zip(&writes) {
                sum.ios += delta.ios;
                sum.bytes += delta.bytes;
                sum.request_ms += delta.request_ms;
            }
        }
        self.report.history.push_back(Sample {
            elapsed_ms,
            interval_ms,
            phase: phase.into(),
            interval_phase,
            reads,
            writes,
            in_flight: latest.iter().map(|s| s.in_flight).collect(),
        });
        let sample = self.report.history.back().unwrap();
        if sample.reads.iter().chain(&sample.writes).any(|v| v.ios != 0 || v.bytes != 0 || v.request_ms != 0) {
            self.report.activity.push_back(sample.clone());
            if self.report.activity.len() > 16 {
                self.report.activity.pop_front();
                self.report.activity_dropped += 1;
            }
        }
        if self.report.history.len() > HISTORY_LIMIT {
            self.report.history.pop_front();
            self.report.samples_dropped += 1;
        }
        self.report.samples_seen += 1;
        self.report.latest = latest;
        self.report.read_totals = read_totals;
        self.report.totals = totals;
        self.report.elapsed_ms = elapsed_ms;
        Ok(())
    }

    /// Share an exact boundary reading without another kernel poll or counter gap.
    pub fn fork_interval(&self, phase: &str) -> io::Result<Self> {
        if !valid_phase(phase) || self.report.latest.is_empty() || self.report.error.is_some() || self.report.finished {
            return Err(io::Error::other("no live I/O boundary"));
        }
        let mut next = Self::new(self.names.clone())?;
        next.report.baseline_ms = Some(self.report.elapsed_ms);
        next.report.elapsed_ms = self.report.elapsed_ms;
        next.report.baseline = self.report.latest.clone();
        next.report.latest = self.report.latest.clone();
        next.report.totals = vec![Writes::default(); self.report.latest.len()];
        next.report.read_totals = next.report.totals.clone();
        next.report.samples_seen = 1;
        next.report.history.push_back(Sample {
            elapsed_ms:self.report.elapsed_ms, interval_ms:0, phase:phase.into(), interval_phase:phase.into(),
            reads:next.report.read_totals.clone(), writes:next.report.totals.clone(), in_flight:self.report.latest.iter().map(|d| d.in_flight).collect(),
        });
        Ok(next)
    }

    pub fn stop(&mut self, error: Option<&str>) {
        self.report.finished = error.is_none();
        self.report.error = error.map(|s| s.chars().take(256).collect());
    }
}

/// One retained, private tmpfs file. Creation is anchored to a checked directory
/// FD, so replacing a parent path cannot redirect the report onto SD.
pub struct RamReport {
    file: File,
    path: PathBuf,
}

impl RamReport {
    pub fn create(parent: &Path) -> io::Result<Self> {
        let dir = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(parent)?;
        let info = read_filesystem(&dir)?;
        if !info.is_tmpfs {
            return Err(io::Error::other(
                "I/O report requires tmpfs; no disk fallback",
            ));
        }
        if info.available_bytes < 1024 * 1024 {
            return Err(io::Error::other("insufficient tmpfs space for I/O report"));
        }
        let anchored = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd()));
        let temp = tempfile::Builder::new()
            .prefix("iman-io-")
            .suffix(".json")
            .tempfile_in(anchored)?;
        let path = parent.join(temp.path().file_name().unwrap());
        let (file, _) = temp.keep().map_err(|e| e.error)?;
        Ok(Self { file, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write(&mut self, report: &Report) -> io::Result<()> {
        let mut bytes = serde_json::to_vec(report)?;
        bytes.push(b'\n');
        if bytes.len() > REPORT_LIMIT {
            return Err(io::Error::other("I/O report exceeds RAM file limit"));
        }
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&bytes)?;
        self.file.set_len(bytes.len() as u64)
        // Deliberately no fsync: this is tmpfs telemetry, not a durable save.
    }
}

pub struct Monitor {
    recorder: Recorder,
    segment: Option<Recorder>,
    output: RamReport,
    read: Box<dyn FnMut() -> io::Result<String> + Send>,
    started: Instant,
    sampled: Instant,
    phase: String,
    stopped: bool,
}

fn diskstats() -> io::Result<String> {
    read_text(Path::new("/proc/diskstats"), INPUT_LIMIT)
        .map_err(|error| io::Error::new(error.kind(), format!("diskstats read failed: {error}")))
}

impl Monitor {
    pub fn report(&self) -> &Report {
        self.recorder.report()
    }

    pub fn start(names: Vec<String>, parent: &Path) -> io::Result<Self> {
        Self::start_at(names, parent, Instant::now())
    }

    pub fn start_at(names: Vec<String>, parent: &Path, started: Instant) -> io::Result<Self> {
        Self::start_using(names, parent, started, Box::new(diskstats))
    }

    /// Explicit source acquisition; offline callers can supply synthetic counters.
    pub fn start_using(
        names: Vec<String>, parent: &Path, started: Instant,
        mut read: Box<dyn FnMut() -> io::Result<String> + Send>,
    ) -> io::Result<Self> {
        let mut recorder = Recorder::new(names)?;
        recorder.sample(started.elapsed().as_millis() as u64, "session_start", &read()?)?;
        let mut output = RamReport::create(parent)?;
        output.write(recorder.report())?;
        crate::session::diagnostic(format_args!(
            "iman: I/O observation report: {}",
            output.path().display()
        ));
        Ok(Self {
            recorder,
            segment: None,
            output,
            read,
            started,
            sampled: started,
            phase: "session_start".into(),
            stopped: false,
        })
    }

    /// Called by existing child wait loops: no monitoring thread or child daemon.
    pub fn poll(&mut self, phase: &str) {
        self.sample(phase, false);
    }

    pub fn boundary(&mut self, phase: &str) {
        self.sample(phase, true);
    }

    /// One source read feeds both recorders. Does not reset the session baseline.
    pub fn begin_segment(&mut self, phase: &str) -> io::Result<()> {
        self.segment = if self.stopped { None } else {
            Some(self.recorder.fork_interval(phase)?)
        };
        Ok(())
    }

    pub fn segment_report(&self) -> Option<&Report> {
        self.segment.as_ref().map(Recorder::report)
    }

    fn sample(&mut self, phase: &str, force: bool) {
        if self.stopped || (!force && phase == self.phase && self.sampled.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        let result = (self.read)()
            .and_then(|text| {
                let ms = self.started.elapsed().as_millis() as u64;
                self.recorder.sample(ms, phase, &text)?;
                if let Some(segment) = &mut self.segment {
                    segment.sample(ms, phase, &text)?;
                }
                Ok(())
            })
            .and_then(|()| self.output.write(self.recorder.report()));
        self.sampled = Instant::now();
        self.phase = phase.into();
        if let Err(error) = result {
            self.stopped = true;
            self.recorder.stop(Some(&error.to_string()));
            if let Some(segment) = &mut self.segment {
                segment.stop(Some(&error.to_string()));
            }
            let _ = self.output.write(self.recorder.report());
            crate::session::diagnostic(format_args!("iman: I/O observation stopped: {error}"));
        }
    }

    pub fn finish(&mut self) {
        if !self.stopped {
            self.poll("session_end");
            if !self.stopped {
                self.recorder.stop(None);
                if let Some(segment) = &mut self.segment {
                    segment.stop(None);
                }
                if let Err(error) = self.output.write(self.recorder.report()) {
                    self.recorder.stop(Some(&error.to_string()));
                    if let Some(segment) = &mut self.segment {
                        segment.stop(Some(&error.to_string()));
                    }
                    crate::session::diagnostic(format_args!(
                        "iman: I/O report finalization failed: {error}"
                    ));
                }
                self.stopped = true;
            }
        }
    }
}
