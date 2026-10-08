// SPDX-License-Identifier: BSD-3-Clause
//! One passive UART0 snapshot in the diagnostic interval, never during normal GUI/game operation.
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io,
    os::{fd::AsRawFd, unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt}},
    path::{Path, PathBuf},
    time::Instant,
};

use imanlib::metrics::{read_text, Reading};
use serde::Serialize;

pub const INPUT_LIMIT: usize = 32 * 1024;
pub const TEXT_BUDGET: usize = 8 * 1024;
pub const RECORD_LIMIT: usize = 64;
pub const ENTRY_LIMIT: usize = 4096;
pub const DEPTH_LIMIT: usize = 16;
pub const REPORT_LIMIT: usize = 48 * 1024;
const PATH_LIMIT: usize = 256;
const MATCH_LIMIT: usize = 16;
const CLOCKS: &[&str] = &[
    "sclk_uart0", "pclk_uart0", "uart0_src", "uart0_frac", "uart_pll_clk", "xin24m", "pclk_peri",
];

#[derive(Clone, Debug, Serialize)]
pub struct Text {
    pub text: String,
    pub selection: &'static str,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Node {
    pub kind: &'static str,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub major: Option<u32>,
    pub minor: Option<u32>,
    pub access_tested: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Scan {
    pub entries: usize,
    pub matches: usize,
    pub truncated: bool,
    pub errors: Vec<String>,
}
impl Scan {
    fn failed(&mut self, error: io::Error) {
        if self.errors.len() < 8 {
            self.errors.push(Reading::<()>::from(Err(error)).error.unwrap());
        } else {
            self.truncated = true;
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub files: BTreeMap<String, Reading<Text>>,
    pub dev_mem: Reading<Node>,
    pub scans: BTreeMap<&'static str, Scan>,
    pub text_bytes: usize,
    pub records_truncated: bool,
}
impl Snapshot {
    pub fn read_errors(&self) -> usize {
        self.files.values().filter(|v| v.error.is_some()).count()
            + usize::from(self.dev_mem.error.is_some())
            + self.scans.values().map(|s| s.errors.len()).sum::<usize>()
    }

    fn insert(&mut self, path: &Path, selection: &'static str, result: io::Result<String>) {
        let Some(path) = path.to_str().filter(|s| s.is_ascii() && s.len() <= PATH_LIMIT) else {
            self.records_truncated = true;
            return;
        };
        if self.files.len() >= RECORD_LIMIT {
            self.records_truncated = true;
            return;
        }
        let value = result.map(|text| {
            let limit = (TEXT_BUDGET - self.text_bytes).min(2048);
            let mut retained = String::new();
            let mut truncated = false;
            for ch in text.chars() {
                let ch = if ch.is_control() && !matches!(ch, '\n' | '\t') { '?' } else { ch };
                if retained.len() + ch.len_utf8() > limit {
                    truncated = true;
                    break;
                }
                retained.push(ch);
            }
            self.text_bytes += retained.len();
            Text { text: retained, selection, truncated }
        });
        self.files.insert(format!("/{path}"), value.into());
    }
}

pub struct Source {
    root: PathBuf,
}
impl Source {
    /// Alternate Linux roots are for fixtures; construction performs no I/O.
    pub fn new(root: PathBuf) -> Self { Self { root } }
    pub fn system() -> Self { Self::new("/".into()) }

    fn text(&self, path: &Path) -> io::Result<String> {
        // O_PATH does not invoke a device's open callback. Pin the inode before
        // opening a readable FD; reject devices, FIFOs and final symlinks first.
        let anchor = OpenOptions::new().read(true)
            .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(self.root.join(path))?;
        if !anchor.metadata()?.is_file() {
            return Err(io::Error::other("diagnostic source is not a regular file"));
        }
        read_text(&PathBuf::from(format!("/proc/self/fd/{}", anchor.as_raw_fd())), INPUT_LIMIT)
    }

    fn node(&self) -> io::Result<Node> {
        let meta = fs::symlink_metadata(self.root.join("dev/mem"))?;
        let char_device = meta.file_type().is_char_device();
        Ok(Node {
            kind: if char_device { "character_device" } else if meta.is_symlink() { "symlink" }
                else if meta.is_file() { "regular_file" } else { "other" },
            mode: meta.mode() & 0o7777, uid: meta.uid(), gid: meta.gid(),
            major: char_device.then(|| libc::major(meta.rdev())),
            minor: char_device.then(|| libc::minor(meta.rdev())),
            access_tested: false,
        })
    }

    fn entries(&self, path: &Path, scan: &mut Scan) -> Vec<(String, fs::FileType)> {
        if scan.entries >= ENTRY_LIMIT { scan.truncated = true; return Vec::new(); }
        let entries = match fs::read_dir(self.root.join(path)) {
            Ok(entries) => entries,
            Err(error) => { scan.failed(error); return Vec::new(); }
        };
        let mut found = Vec::new();
        for entry in entries {
            if scan.entries >= ENTRY_LIMIT { scan.truncated = true; break; }
            scan.entries += 1;
            let result = entry.and_then(|entry| {
                let name = entry.file_name().into_string()
                    .map_err(|_| io::Error::other("non-UTF8 diagnostic entry"))?;
                if !name.is_ascii() || path.as_os_str().len() + name.len() + 1 > PATH_LIMIT {
                    return Err(io::Error::other("diagnostic path exceeds limit"));
                }
                Ok((name, entry.file_type()?))
            });
            match result {
                Ok(entry) => found.push(entry),
                Err(error) => scan.failed(error),
            }
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        found
    }

    fn links(&self, root: &str, tty: bool, snapshot: &mut Snapshot) -> Scan {
        let mut scan = Scan::default();
        for (name, _) in self.entries(Path::new(root), &mut scan) {
            let selected = if tty { name.starts_with("ttyS") || name.starts_with("ttyAMA") }
                else { name.contains("serial") || name.contains("uart") };
            if !selected { continue; }
            if scan.matches >= MATCH_LIMIT { scan.truncated = true; break; }
            if snapshot.files.len() >= RECORD_LIMIT {
                snapshot.records_truncated = true;
                scan.truncated = true;
                break;
            }
            scan.matches += 1;
            let base = Path::new(root).join(name);
            let path = base.join(if tty { "device" } else { "driver" });
            let link = fs::read_link(self.root.join(&path)).map(|p| p.to_string_lossy().into_owned());
            snapshot.insert(&path, "symlink_target_only", link);
        }
        scan
    }

    fn clocks(&self, snapshot: &mut Snapshot) -> Scan {
        let mut scan = Scan::default();
        let mut pending = vec![(PathBuf::from("sys/kernel/debug/clk"), 0)];
        while let Some((path, depth)) = pending.pop() {
            for (name, kind) in self.entries(&path, &mut scan).into_iter().rev() {
                if !kind.is_dir() { continue; }
                let child = path.join(&name);
                if CLOCKS.contains(&name.as_str()) {
                    if snapshot.files.len() + 4 > RECORD_LIMIT {
                        snapshot.records_truncated = true;
                        scan.truncated = true;
                        return scan;
                    }
                    scan.matches += 1;
                    for field in ["clk_rate", "clk_enable_count", "clk_prepare_count", "clk_parent"] {
                        let file = child.join(field);
                        snapshot.insert(&file, "clock_framework_cached_state", self.text(&file));
                    }
                }
                if depth < DEPTH_LIMIT { pending.push((child, depth + 1)); }
                else { scan.truncated = true; }
            }
            if scan.entries >= ENTRY_LIMIT { scan.truncated = true; break; }
        }
        scan
    }

    fn pinctrl(&self, snapshot: &mut Snapshot) -> Scan {
        let root = Path::new("sys/kernel/debug/pinctrl");
        let mut scan = Scan::default();
        for (name, kind) in self.entries(root, &mut scan) {
            if !kind.is_dir() { continue; }
            if scan.matches >= MATCH_LIMIT { scan.truncated = true; break; }
            if snapshot.files.len() >= RECORD_LIMIT {
                snapshot.records_truncated = true;
                scan.truncated = true;
                break;
            }
            scan.matches += 1;
            let path = root.join(name).join("pinmux-pins");
            let selected = self.text(&path).map(|text| select_lines(&text, |line| {
                !line.starts_with("pin ") || line.starts_with("pin 16 (") || line.starts_with("pin 17 (")
            }));
            snapshot.insert(&path, "ownership_only_pin_indices_16_17", selected);
        }
        scan
    }

    /// Read software bookkeeping only. Missing debugfs stays missing: never mount
    /// it, open a tty or /dev/mem, export GPIO, or read register-backed debug files.
    pub fn capture(&self) -> io::Result<Snapshot> {
        if !self.root.is_absolute() { return Err(io::Error::other("diagnostic root must be absolute")); }
        let mut snapshot = Snapshot {
            files: BTreeMap::new(), dev_mem: self.node().into(), scans: BTreeMap::new(),
            text_bytes: 0, records_truncated: false,
        };
        for name in ["proc/version", "proc/cmdline", "proc/consoles", "proc/tty/drivers"] {
            let path = Path::new(name);
            let result = self.text(path).map(|text| {
                if name == "proc/tty/drivers" { select_lines(&text, |line| line.contains("serial")) }
                else { text }
            });
            snapshot.insert(path, if name == "proc/tty/drivers" { "serial_driver_rows" } else { "all" }, result);
        }
        let status = Path::new("proc/self/status");
        let credentials = self.text(status).map(|text| select_lines(&text, |line| {
            line.split_once(':').is_some_and(|(key, _)| matches!(key,
                "Uid" | "Gid" | "Groups" | "CapInh" | "CapPrm" | "CapEff" | "CapBnd" | "CapAmb" | "NoNewPrivs" | "Seccomp"))
        }));
        snapshot.insert(status, "credentials_and_capabilities", credentials);
        let platform = self.links("sys/bus/platform/devices", false, &mut snapshot);
        snapshot.scans.insert("platform_serial_devices", platform);
        let tty = self.links("sys/class/tty", true, &mut snapshot);
        snapshot.scans.insert("tty_devices", tty);
        let clocks = self.clocks(&mut snapshot);
        snapshot.scans.insert("uart0_clock_tree", clocks);
        let pinctrl = self.pinctrl(&mut snapshot);
        snapshot.scans.insert("pinctrl_ownership", pinctrl);
        Ok(snapshot)
    }
}

fn select_lines(text: &str, select: impl Fn(&str) -> bool) -> String {
    text.lines().filter(|line| select(line)).map(|line| format!("{line}\n")).collect()
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub version: u32,
    pub policy: &'static str,
    pub samples: u32,
    pub stopped: bool,
    pub ms: Option<u64>,
    pub phase: String,
    pub capture_us: u64,
    pub read_errors: usize,
    pub snapshot: Option<Snapshot>,
    pub error: Option<String>,
}

pub struct Monitor {
    report: Report,
}
impl Default for Monitor {
    fn default() -> Self {
        Self { report: Report { version: 1, policy: "diagnostic_screen_once_per_session_software_state_only",
            samples: 0, stopped: false, ms: None, phase: String::new(), capture_us: 0,
            read_errors: 0, snapshot: None, error: None } }
    }
}
impl Monitor {
    pub fn report(&self) -> &Report { &self.report }
    pub fn stop(&mut self) { self.report.stopped = true; }
    pub fn poll(&mut self, ms: u64, phase: &str, read: impl FnOnce() -> io::Result<Snapshot>) {
        // Only the existing manager diagnostic interval may trigger this one-shot
        // observation. GUI/game/export polls must not read even the first source.
        if phase != "gui_diagnostics" || self.report.stopped || self.report.samples != 0 { return; }
        self.report.samples = 1;
        self.report.ms = Some(ms);
        self.report.phase = phase.chars().take(64).collect();
        let started = Instant::now();
        let reading: Reading<Snapshot> = read().into();
        self.report.capture_us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        self.report.read_errors = reading.value.as_ref().map_or(1, Snapshot::read_errors);
        self.report.snapshot = reading.value;
        self.report.error = reading.error;
    }
    pub fn report_json(&self) -> io::Result<String> {
        let text = serde_json::to_string(&self.report)?;
        if text.len() > REPORT_LIMIT { return Err(io::Error::other("UART report exceeds limit")); }
        Ok(text)
    }
}
