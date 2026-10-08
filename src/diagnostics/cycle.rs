// SPDX-License-Identifier: BSD-3-Clause
//! One active segment and one immutable export. No queues, retries or device access.
use std::{collections::{BTreeMap, VecDeque}, io, path::PathBuf, sync::Arc, thread::JoinHandle};

use serde::Serialize;
use serde_json::{json, Value};

use crate::diagnostics::export::Bundle;

pub const PHASE_HISTORY_LIMIT: usize = 64;
pub const SESSION_EXPORT_LIMIT: u64 = 256;
pub const SESSION_BYTE_LIMIT: usize = 16 * 1024 * 1024;
pub const SETTLING_SECONDS: u64 = 2;

/// Source capabilities, independent of which sources are available on this host.
pub fn compiled_modules() -> &'static [&'static str] {
    &[
        #[cfg(feature = "io-monitor")]
        "io_monitor",
        #[cfg(feature = "diag-perf")]
        "diag_perf",
        #[cfg(feature = "diag-memory")]
        "diag_memory",
        #[cfg(feature = "diag-klog")]
        "diag_klog",
        #[cfg(feature = "diag-uart")]
        "diag_uart",
        #[cfg(feature = "diag-display")]
        "diag_display",
        #[cfg(feature = "diag-input")]
        "diag_input",
        #[cfg(feature = "diag-input")]
        "diag_input_test",
    ]
}

/// Production supplies the guarded board writer; embedders supply their own sink.
/// There is deliberately no CLI path/guard override for this dependency.
pub type Writer = Arc<dyn Fn(&str, &Bundle) -> io::Result<PathBuf> + Send + Sync>;

#[derive(Debug, Serialize)]
pub struct Boundary {
    pub ms: u64,
    pub phase: String,
}

#[derive(Debug, Serialize)]
pub struct Segment {
    pub sequence: u64,
    pub begin_ms: u64,
    pub end_ms: u64,
    pub phases_ms: BTreeMap<String, u64>,
    pub boundaries: VecDeque<Boundary>,
    pub boundaries_dropped: u64,
    pub game: Option<Value>,
    pub game_result: Option<Value>,
    pub gui_result: Option<Value>,
    pub performance: Option<Value>,
    pub output: Value,
    pub gui_output: Value,
    #[serde(skip)]
    pub game_log: Vec<u8>,
    phase: String,
}

impl Segment {
    fn new(sequence: u64, ms: u64) -> Self {
        Self {
            sequence, begin_ms: ms, end_ms: ms, phases_ms: BTreeMap::new(),
            boundaries: VecDeque::new(), boundaries_dropped: 0, game: None,
            game_result: None, gui_result: None, performance: None, output: Value::Null,
            gui_output: Value::Null, game_log: Vec::new(), phase: "initializing".into(),
        }
    }

    pub fn phase(&mut self, ms: u64, phase: &str) {
        // Fixed vocabulary prevents per-cycle IDs from making the map unbounded.
        let phase = if ["initializing", "session_start", "starting_gui", "gui_start", "gui",
            "gui_exit", "input_test", "gui_diagnostics", "gui_error", "game_start", "game", "game_exit",
            "game_cleanup", "display_reclaim", "post_game", "session_end", "diagnostic_export"
        ].contains(&phase) { phase } else { "unknown" };
        *self.phases_ms.entry(self.phase.clone()).or_default() += ms.saturating_sub(self.end_ms);
        self.end_ms = ms.max(self.end_ms);
        if phase != self.phase || self.boundaries.is_empty() {
            if self.boundaries.len() == PHASE_HISTORY_LIMIT {
                self.boundaries.pop_front();
                self.boundaries_dropped += 1;
            }
            self.boundaries.push_back(Boundary { ms: self.end_ms, phase: phase.into() });
            self.phase = phase.into();
        }
    }
}

struct Pending {
    bundle: Arc<Bundle>,
    bytes: usize,
    sequence: u64,
    begin_ms: u64,
    job: JoinHandle<io::Result<PathBuf>>,
}

pub struct Session {
    pub id: String,
    pub active: Option<Segment>,
    pub previous_export: Option<Value>,
    pub saved: u64,
    pub exported_bytes: usize,
    pub error: Option<String>,
    pub skipped: u64,
    next: u64,
    writer: Writer,
    pending: Option<Pending>,
    failed: Option<Arc<Bundle>>,
}

impl Session {
    pub fn new(id: String, writer: Writer, ms: u64) -> Self {
        Self {
            id, active: Some(Segment::new(1, ms)), previous_export: None,
            saved: 0, exported_bytes: 0, error: None, skipped: 0, next: 2,
            writer, pending: None, failed: None,
        }
    }

    pub fn phase(&mut self, ms: u64, phase: &str) {
        if let Some(segment) = &mut self.active {
            segment.phase(ms, phase);
        }
    }

    pub fn begin_next(&mut self, ms: u64) {
        if self.error.is_none() && self.pending.is_none() && self.active.is_none() {
            self.active = Some(Segment::new(self.next, ms));
            self.next += 1;
        } else if self.error.is_some() {
            self.skipped = self.skipped.saturating_add(1);
        }
    }

    pub fn freeze(&mut self, ms: u64, error: Option<&str>) -> Option<Bundle> {
        let mut segment = self.active.take()?;
        segment.phase(ms, &segment.phase.clone());
        let game_log = std::mem::take(&mut segment.game_log);
        #[cfg(feature = "diag-perf")]
        let performance = segment.performance.take();
        let mut bundle = Bundle::new(json!({
            "session_id": self.id, "clock":"iman_session", "clock_unit":"milliseconds",
            "clock_origin":"manager_control_created", "clock_kind":"monotonic", "manager_version":env!("CARGO_PKG_VERSION"), "manager_build_utc":env!("IMAN_BUILD_UTC"),
            "architecture":std::env::consts::ARCH, "compiled_modules":compiled_modules(), "segment":segment,
            "session_error":error.map(|s| s.chars().take(2048).collect::<String>()),
            "previous_export":self.previous_export, "saved_segments_before":self.saved,
            "exported_file_bytes_before":self.exported_bytes,
            "cutoff":"before this export and final display restoration",
        }));
        if segment.game.is_some() {
            bundle.files.insert("retroarch.log".into(), game_log);
            #[cfg(feature = "diag-perf")]
            {
                // Explicitly distinguish an absent CPU report from zero CPU activity.
                let report = performance.unwrap_or_else(|| json!({"state":"unavailable",
                    "reason":"game did not start or CPU observation was not active"}));
                if let Err(error) = bundle.json("diag_perf.json", &report) {
                    bundle.metadata["performance_error"] = json!(error.to_string());
                }
            }
        }
        Some(bundle)
    }

    /// One attempt per snapshot. A failed attempt permanently disarms this session.
    pub fn start(&mut self, bundle: Bundle, ms: u64) -> io::Result<()> {
        if self.error.is_some() || self.pending.is_some() {
            return Err(io::Error::other("automatic export is disabled or busy"));
        }
        let bundle = Arc::new(bundle);
        let bytes = bundle.byte_len().and_then(|bytes| {
            if self.saved >= SESSION_EXPORT_LIMIT || bytes > SESSION_BYTE_LIMIT.saturating_sub(self.exported_bytes) {
                Err(io::Error::other("diagnostic session export budget exhausted"))
            } else { Ok(bytes) }
        });
        let bytes = match bytes {
            Ok(bytes) => bytes,
            Err(error) => { self.fail(bundle, &error); return Err(error); }
        };
        let sequence = bundle.metadata["segment"]["sequence"].as_u64().unwrap_or(0);
        let run = format!("{}-cycle-{sequence:06}", self.id);
        let writer = self.writer.clone();
        let input = bundle.clone();
        let job = std::thread::Builder::new().name("iman-export".into())
            .spawn(move || writer(&run, &input));
        match job {
            Ok(job) => {
                self.pending = Some(Pending { bundle, bytes, sequence, begin_ms: ms, job });
                Ok(())
            }
            Err(error) => { self.fail(bundle, &error); Err(error) }
        }
    }

    fn fail(&mut self, bundle: Arc<Bundle>, error: &io::Error) {
        self.error = Some(error.to_string().chars().take(2048).collect());
        self.failed = Some(bundle);
    }

    pub fn pending(&self) -> bool { self.pending.is_some() }

    /// Never blocks the sampler. Returns true exactly once for a completed worker.
    pub fn poll(&mut self, ms: u64) -> bool {
        if !self.pending.as_ref().is_some_and(|pending| pending.job.is_finished()) {
            return false;
        }
        let pending = self.pending.take().unwrap();
        let result = pending.job.join().unwrap_or_else(|_| Err(io::Error::other("export worker panicked")));
        self.previous_export = Some(json!({
            "sequence":pending.sequence, "begin_ms":pending.begin_ms, "end_ms":ms,
            "status":if result.is_ok() {"synchronized"} else {"failed"},
            "path":result.as_ref().ok(), "error":result.as_ref().err().map(ToString::to_string),
        }));
        match result {
            Ok(_) => { self.saved += 1; self.exported_bytes += pending.bytes; }
            Err(error) => self.fail(pending.bundle, &error),
        }
        true
    }

    pub fn failed_snapshot(&self) -> Option<&Bundle> { self.failed.as_deref() }

    pub fn status(&self) -> Value {
        json!({
            "policy":"automatic_segments", "session_id":self.id,
            "state":if self.error.is_some() {"failed"} else if self.pending() {"saving"} else {"observing"},
            "saved":self.saved, "exported_file_bytes":self.exported_bytes,
            "skipped_segments":self.skipped, "error":self.error,
            "failed_snapshot_retained":self.failed.is_some(), "previous_export":self.previous_export,
        })
    }

    pub fn disable(&mut self, error: &str) {
        self.error.get_or_insert_with(|| error.chars().take(2048).collect());
        self.active = None;
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Library callers cannot abandon an accepted persistent write by dropping Control.
        if let Some(pending) = self.pending.take() {
            let _ = pending.job.join();
        }
    }
}
