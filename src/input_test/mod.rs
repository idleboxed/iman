// SPDX-License-Identifier: BSD-3-Clause
//! Manager-owned, read-only controller test. No game-port assignment or SD writes.
pub mod guide;
pub mod inventory;
pub mod journal;
pub mod proposal;
pub mod render;

use guide::{axis_tolerance, Axes, AxisState, TimedGuide, STEP_MS};
use imanlib::input_probe::Probe;
use imanlib::evdev::DeviceInfo;
use serde_json::{json, Value};
use std::{collections::{BTreeMap, BTreeSet}, io, path::Path, time::{Duration, Instant}};

pub const DISCOVERY_MS: u64 = 10_000;
pub const PREPARE_MS: u64 = 4_000;
pub const MAX_CONTROLLERS: usize = 4;
const EVENT_BUDGET: usize = 16 * 1024;
const RAW_BUDGET: usize = 8 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stage {
    Discover,
    Prepare { number: usize },
    Controller { number: usize, step: usize, release: bool },
    Finished,
}

/// Only visible state participates in frame invalidation; no poll/event counter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub stage: Stage,
    pub count: usize,
    pub opened: u32,
    pub seconds: u64,
    pub input: String,
    pub notice: String,
}

struct Controller {
    generation: u32,
    info: DeviceInfo,
    steps: Vec<Value>,
    outcome: &'static str,
}

pub struct Completion {
    pub report: Value,
    pub files: BTreeMap<String, Vec<u8>>,
}

pub struct InputTest {
    pub probe: Probe,
    pub guide: TimedGuide,
    controllers: Vec<Controller>,
    current: Option<usize>,
    preparing_since: Option<u64>,
    discovery: bool,
    axes: Axes,
    unknown_axes: BTreeSet<(u32, u16)>,
    records: Vec<Value>,
    inventory: Vec<Value>,
    bytes: usize,
    raw_bytes: usize,
    omitted: u64,
    input: String,
    notice: String,
}

impl Default for InputTest {
    fn default() -> Self {
        Self {
            probe: Probe::input_test(), guide: TimedGuide::default(),
            controllers: Vec::new(), current: None, preparing_since: None, discovery: true,
            axes: Axes::new(), unknown_axes: BTreeSet::new(), records: Vec::new(), inventory: Vec::new(),
            bytes: 0, raw_bytes: 0, omitted: 0, input: "Input: -".into(), notice: String::new(),
        }
    }
}

impl InputTest {
    fn record(&mut self, record: Value) {
        let bytes = serde_json::to_vec(&record).map_or(usize::MAX, |bytes| bytes.len() + 1);
        let raw = record["event"] == "raw";
        if bytes > 8 * 1024 || self.bytes.saturating_add(bytes) > EVENT_BUDGET
            || (raw && self.raw_bytes.saturating_add(bytes) > RAW_BUDGET) {
            self.omitted = self.omitted.saturating_add(1);
        } else {
            self.bytes += bytes;
            if raw { self.raw_bytes += bytes; }
            self.records.push(record);
        }
    }

    pub fn done(&self) -> bool { !self.discovery && self.current.is_none() }

    pub fn view(&self, ms: u64) -> View {
        let (stage, seconds) = if self.discovery {
            (Stage::Discover, DISCOVERY_MS.saturating_sub(ms).div_ceil(1000))
        } else if let Some(index) = self.current {
            if let Some(started) = self.preparing_since {
                (Stage::Prepare { number: index + 1 },
                    started.saturating_add(PREPARE_MS).saturating_sub(ms).div_ceil(1000))
            } else {
                (Stage::Controller { number: index + 1, step: self.guide.index,
                    release: self.guide.waiting_for_release() },
                    (self.guide.started_ms + STEP_MS).saturating_sub(ms).div_ceil(1000))
            }
        } else { (Stage::Finished, 0) };
        View { stage, count: self.controllers.len(), opened: self.probe.connections, seconds,
            input: self.input.clone(), notice: self.notice.clone() }
    }

    fn prepare_controller(&mut self, index: usize, ms: u64) {
        self.current = Some(index);
        self.preparing_since = Some(ms);
        self.guide = TimedGuide::begin(ms);
        self.input = "Input: -".into();
        self.record(json!({"event":"controller_prepare","controller":index + 1,
            "generation":self.controllers[index].generation,"ms":ms,"duration_ms":PREPARE_MS}));
    }

    /// Called after a bounded reader poll. Other devices never supply this pad's answers.
    pub fn refresh(&mut self, ms: u64) {
        let generation = self.current.map(|index| self.controllers[index].generation);
        for stream in self.probe.streams.iter().flatten() {
            for (&code, info) in &stream.info.abs_info {
                self.axes.entry((stream.generation, u16::from(code))).or_insert(AxisState {
                    value: info[0], baseline: info[0], tolerance: axis_tolerance(info),
                });
            }
        }
        let mut raw = Vec::new();
        let mut invalid = false;
        for mut record in self.probe.take_records() {
            let id = record["generation"].as_u64().unwrap_or(0) as u32;
            if record["event"] == "resync" {
                self.unknown_axes.extend(self.axes.keys().filter(|(g, _)| *g == id).copied());
                invalid |= generation == Some(id);
            }
            if record["event"] == "device_error" {
                self.notice = "Input device unavailable; see report".into();
            }
            if record["event"] == "device" {
                record["axis_baseline_policy"] = json!("opening snapshot, not verified physical neutral");
            }
            if record["event"] == "raw" {
                if self.discovery && ms < DISCOVERY_MS && record["fresh_press"] == true
                    && self.probe.sample_complete && self.controllers.len() < MAX_CONTROLLERS
                    && !self.controllers.iter().any(|controller| controller.generation == id) {
                    if let Some((slot, stream)) = self.probe.streams.iter().enumerate()
                        .filter_map(|(slot, stream)| stream.as_ref().map(|s| (slot, s)))
                        .find(|(_, stream)| stream.generation == id) {
                        if self.probe.ready(slot) {
                            self.controllers.push(Controller { generation: id, info: stream.info.clone(),
                                steps: Vec::new(), outcome: "pending" });
                        }
                    }
                }
                if record["kind"] == 3 {
                    let key = (id, record["code"].as_u64().unwrap_or(0) as u16);
                    self.unknown_axes.remove(&key);
                    if let Some(axis) = self.axes.get_mut(&key) {
                        axis.value = record["value"].as_i64().unwrap_or(0) as i32;
                        record["baseline"] = json!(axis.baseline);
                        record["tolerance"] = json!(axis.tolerance);
                    }
                }
                if generation == Some(id) {
                    record["controller"] = json!(self.current.unwrap() + 1);
                    if self.preparing_since.is_some() {
                        record["phase"] = json!("prepare");
                    } else {
                        record["step"] = json!(self.guide.index);
                        let kind = if record["kind"] == 1 { "KEY" } else { "ABS" };
                        self.input = format!("Input: {kind} {} = {}", record["code"], record["value"]);
                        raw.push(record.clone());
                    }
                }
            }
            self.record(record);
        }
        self.axes.retain(|(id, _), _| self.probe.streams.iter().flatten().any(|s| s.generation == *id));
        self.unknown_axes.retain(|key| self.axes.contains_key(key));
        if self.discovery {
            if ms >= DISCOVERY_MS {
                self.discovery = false;
                if !self.controllers.is_empty() { self.prepare_controller(0, ms); }
            }
            return;
        }
        let Some(index) = self.current else { return; };
        let id = self.controllers[index].generation;
        let stream = self.probe.streams.iter().enumerate()
            .filter_map(|(slot, stream)| stream.as_ref().map(|s| (slot, s)))
            .find(|(_, stream)| stream.generation == id);
        if let Some((slot, stream)) = stream {
            if let Some(started) = self.preparing_since {
                if ms.saturating_sub(started) >= PREPARE_MS {
                    self.preparing_since = None;
                    self.guide = TimedGuide::begin(ms);
                }
                // Even the boundary poll belongs to preparation. Held controls
                // must return to neutral before the first prompt can be armed.
                return;
            }
            invalid |= self.unknown_axes.iter().any(|(g, _)| *g == id);
            if invalid { self.guide.invalidate(); }
            let axes = self.axes.iter().filter(|((g, _), _)| *g == id).map(|(key, axis)| (*key, *axis)).collect();
            let ready = !invalid && self.probe.sample_complete && self.probe.ready(slot);
            self.guide.sample(ms, &raw, ready, stream.buttons_down().is_empty(), &axes);
        } else {
            self.preparing_since = None;
            self.guide.cancel_remaining(ms, "disconnected");
            self.controllers[index].outcome = "disconnected";
        }
        if self.guide.done() {
            self.controllers[index].steps = std::mem::take(&mut self.guide.results);
            if self.controllers[index].outcome == "pending" { self.controllers[index].outcome = "tested"; }
            if index + 1 < self.controllers.len() {
                self.prepare_controller(index + 1, ms);
            } else {
                self.current = None;
                self.preparing_since = None;
                self.input = "Input: -".into();
            }
        }
    }

    pub fn result_message(&self) -> String {
        if self.probe.connections == 0 { "No matching input devices opened".into() }
        else if self.controllers.is_empty() { "Input devices opened, but no fresh button presses".into() }
        else { format!("Controller test complete: {} controller(s)", self.controllers.len()) }
    }

    fn inventory(&mut self, ms: u64, capture: &mut dyn FnMut() -> Value) {
        let data = capture();
        let data = if serde_json::to_vec(&data).map_or(usize::MAX, |bytes| bytes.len()) > inventory::SNAPSHOT_LIMIT {
            json!({"error":"inventory exceeds snapshot limit", "omitted":true})
        } else { data };
        if self.inventory.len() < 2 { self.inventory.push(json!({"ms":ms, "data":data})); }
    }

    pub fn finish(self, ms: u64, reason: &str) -> Value { self.finish_with_proposals(ms, reason).report }

    pub fn finish_with_proposals(mut self, ms: u64, reason: &str) -> Completion {
        if let Some(index) = self.current {
            self.guide.cancel_remaining(ms, "cancelled");
            self.controllers[index].steps = std::mem::take(&mut self.guide.results);
            self.controllers[index].outcome = "cancelled";
        }
        let summary = self.probe.finish(ms, reason);
        for record in self.probe.take_records() { self.record(record); }
        let unconfirmed: Vec<_> = self.probe.streams.iter().flatten()
            .filter(|s| !self.controllers.iter().any(|c| c.generation == s.generation))
            .map(|s| json!({"generation":s.generation,"path":s.info.path,"status":"not_confirmed"})).collect();
        let proposals = proposal::generate(&self.controllers.iter().enumerate().map(|(index, controller)|
            proposal::Participant { number: index + 1, generation: controller.generation,
                info: &controller.info, steps: &controller.steps }).collect::<Vec<_>>());
        let proposal_status = json!({"candidate_only":true,"applied":false,"files":proposals.files.keys().collect::<Vec<_>>(),
            "summary":proposals.files.contains_key("pads-proposal/summary.json").then_some("pads-proposal/summary.json"),
            "physical_labels_verified":false});
        let controllers: Vec<_> = self.controllers.into_iter().enumerate().map(|(i, c)| json!({
            "number":i + 1,"generation":c.generation,"path":c.info.path,"status":c.outcome,"steps":c.steps,
        })).collect();
        let report = json!({"format":"IMAN-INPUT-TEST-2","owner":"iman","clock":"input_test_elapsed",
            "clock_unit":"milliseconds","duration_ms":ms,"reason":reason,"mapping_written":false,
            "physical_players_verified":false,"selection":"fresh key presses during discovery",
            "discovery_ms":DISCOVERY_MS,"prepare_ms":PREPARE_MS,"opened_connections":self.probe.connections,
            "inventory":self.inventory,"inventory_snapshot_limit":inventory::SNAPSHOT_LIMIT,
            "controllers":controllers,"unconfirmed":unconfirmed,
            "probe":summary,"input_stats":self.probe.diagnostic_report(false),
            "records":self.records,"records_omitted":self.omitted,
            "record_bytes_limit":EVENT_BUDGET,"raw_bytes_limit":RAW_BUDGET,
            "step_results_reserved":true,"persistent":false,"proposals":proposal_status});
        Completion { report, files: proposals.files }
    }
}

/// Public injectable loop: tests use a synthetic source and monotonic clock.
pub fn run_with_source<S: imanlib::input_probe::linux::Source>(
    test: &mut InputTest,
    source: S,
    display: &mut dyn crate::display::Lease,
    tick: &mut dyn FnMut() -> io::Result<u64>,
) -> io::Result<u64> {
    run_with_inventory(test, source, display, tick, &mut || json!({"available":false}))
}

/// Capture only after the screen accepts ownership, and again at completion or
/// cancellation. Injectable inventory never implies access to real host devices.
pub fn run_with_inventory<S: imanlib::input_probe::linux::Source>(
    test: &mut InputTest,
    source: S,
    display: &mut dyn crate::display::Lease,
    tick: &mut dyn FnMut() -> io::Result<u64>,
    inventory: &mut dyn FnMut() -> Value,
) -> io::Result<u64> {
    let initial = test.view(0);
    if !display.input_test_frame(&initial)? {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "No managed input test display"));
    }
    test.inventory(0, inventory);
    let mut reader = imanlib::input_probe::linux::Reader::new(source);
    let mut shown = initial;
    let mut last_ms = 0;
    let result = (|| { loop {
        #[cfg(feature = "pc")]
        display.poll_events()?;
        let ms = tick()?;
        last_ms = ms;
        reader.poll(&mut test.probe, ms);
        test.refresh(ms);
        let view = test.view(ms);
        if shown != view {
            if !display.input_test_frame(&view)? {
                return Err(io::Error::other("Input test display became unavailable"));
            }
            shown = view;
        }
        if test.done() { return Ok(ms); }
    } })();
    test.inventory(last_ms, inventory);
    result
}

/// No devices are opened without a visible manager-owned screen.
pub fn run(
    root: &Path,
    control: &mut crate::session::control::Control,
    observer: &mut dyn FnMut(&'static str),
    display: &mut dyn crate::display::Lease,
) -> io::Result<String> {
    let directory = root.join(crate::runtime::paths::SYSTEM_DIR).join("igui/controls");
    let profiles = match imanlib::controls::load(&directory, None, None) {
        Ok(profiles) => profiles,
        Err(error) => {
            control.record_input_test(json!({"format":"IMAN-INPUT-TEST-2","owner":"iman",
                "reason":"unavailable","error":error.to_string(),"controllers":[],"mapping_written":false}));
            return Ok("Input test unavailable: cannot load control profiles".into());
        }
    };
    let inventory_profiles = profiles.evdev.clone();
    let inventory_conflicts = profiles.conflicts.clone();
    let started = Instant::now();
    let mut test = InputTest::default();
    let result = run_with_inventory(&mut test, imanlib::input_probe::linux::DiagnosticSource::new(profiles.evdev, profiles.conflicts), display, &mut || {
        observer("input_test");
        control.poll("input_test");
        crate::session::check_interrupted()?;
        std::thread::sleep(Duration::from_millis(20));
        Ok(started.elapsed().as_millis() as u64)
    }, &mut || inventory::snapshot_with_conflicts(Path::new("/dev/input"), Path::new("/sys/class/input"),
        Path::new("/sys/bus/usb/devices"), &inventory_profiles, &inventory_conflicts));
    let message = test.result_message();
    let mut completion = test.finish_with_proposals(started.elapsed().as_millis() as u64,
        if result.is_ok() { "completed" } else { "interrupted" });
    if let Err(error) = &result { completion.report["error"] = json!(error.to_string()); }
    control.record_input_test_completion(completion);
    if result.as_ref().is_err_and(|error| error.kind() == io::ErrorKind::Unsupported) {
        return Ok("Input test unavailable: no managed display".into());
    }
    result?;
    Ok(message)
}
