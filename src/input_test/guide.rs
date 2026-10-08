// SPDX-License-Identifier: BSD-3-Clause
//! Timed physical-label prompts; observed codes never become an automatic mapping.
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const STEP_MS: u64 = 5_000;
pub const ACTIONS: [&str; 25] = [
    "D-pad Up", "D-pad Down", "D-pad Left", "D-pad Right",
    "Cross / A (bottom)", "Circle / B (right)", "Square / X (left)", "Triangle / Y (top)",
    "Select / Back / Share", "Start / Options / Menu", "L1 / LB", "R1 / RB",
    "L2 / LT", "R2 / RT", "L3 / LS", "R3 / RS", "Home / Guide",
    "Left stick up", "Left stick down", "Left stick left", "Left stick right",
    "Right stick up", "Right stick down", "Right stick left", "Right stick right",
];

type Control = (u32, u16, u16);
#[derive(Clone, Copy, serde::Serialize)]
struct AxisEvidence {
    baseline: i32,
    extreme: i32,
    direction: i8,
}
#[derive(Clone, Copy)]
pub struct AxisState {
    pub value: i32,
    pub baseline: i32,
    pub tolerance: i32,
}
impl AxisState {
    pub fn neutral(self) -> bool {
        (i64::from(self.value) - i64::from(self.baseline)).abs() <= i64::from(self.tolerance)
    }
}
pub type Axes = BTreeMap<(u32, u16), AxisState>;

pub fn axis_tolerance(info: &[i32; 6]) -> i32 {
    let range = (i64::from(info[2]) - i64::from(info[1])).abs();
    if range <= 2 { 0 } else {
        i64::from(info[3].max(info[4]).max(0)).max((range / 100).max(1)).min(i64::from(i32::MAX)) as i32
    }
}

#[derive(Default)]
pub struct TimedGuide {
    pub index: usize,
    pub started_ms: u64,
    pub armed: bool,
    candidate: Option<Control>,
    released: bool,
    ambiguous: bool,
    axis_evidence: Option<AxisEvidence>,
    baseline: Axes,
    quiet_since: u64,
    pub results: Vec<Value>,
}

impl TimedGuide {
    pub fn begin(ms: u64) -> Self {
        Self { started_ms: ms, quiet_since: ms, ..Self::default() }
    }

    pub fn waiting_for_release(&self) -> bool { self.candidate.is_some() && !self.released }

    pub fn cancel_remaining(&mut self, ms: u64, outcome: &str) {
        while !self.done() { self.advance(ms, outcome, &mut Vec::new()); }
    }

    pub fn done(&self) -> bool {
        self.index == ACTIONS.len()
    }

    pub fn prompt(&self) -> Option<&'static str> {
        ACTIONS.get(self.index).copied()
    }

    pub fn invalidate(&mut self) {
        self.armed = false;
        self.candidate = None;
        self.released = false;
        self.ambiguous = true;
        self.axis_evidence = None;
    }

    pub fn sample(&mut self, ms: u64, events: &[Value], ready: bool, released: bool, axes: &Axes) -> Vec<Value> {
        let mut records = Vec::new();
        let released = released && axes.values().all(|axis| axis.neutral());
        let pressed_keys: BTreeMap<_, _> = events.iter()
            .filter(|event| event["kind"] == 1 && event["value"] == 1)
            .map(|event| ((event["generation"].as_u64().unwrap_or(0) as u32,
                event["code"].as_u64().unwrap_or(0) as u16), ()))
            .collect();
        let prefer_key = pressed_keys.len() == 1;
        while !self.done() && ms.saturating_sub(self.started_ms) >= STEP_MS {
            let outcome = if self.ambiguous { "ambiguous" }
                else if self.candidate.is_some() || !released { "held" } else { "no_response" };
            self.advance(self.started_ms.saturating_add(STEP_MS), outcome, &mut records);
        }
        if self.done() { return records; }
        if !ready {
            if self.candidate.is_some() || !events.is_empty() { self.invalidate(); }
            return records;
        }
        if !self.armed {
            if events.iter().any(|event| event["kind"] == 3 && axes.get(&(
                event["generation"].as_u64().unwrap_or(0) as u32,
                event["code"].as_u64().unwrap_or(0) as u16,
            )).is_none_or(|axis| !AxisState { value: event["value"].as_i64().unwrap_or(0) as i32, ..*axis }.neutral())) {
                self.quiet_since = ms;
            }
            if ready && released && ms.saturating_sub(self.quiet_since) >= 100 {
                self.baseline = axes.clone();
                self.armed = true;
            }
            return records;
        }
        for event in events {
            let control = (event["generation"].as_u64().unwrap_or(0) as u32,
                event["kind"].as_u64().unwrap_or(0) as u16, event["code"].as_u64().unwrap_or(0) as u16);
            let value = event["value"].as_i64().unwrap_or(0) as i32;
            // Some receivers expose one physical trigger or D-pad action as a
            // key plus an analog axis. Keep the axis in the raw journal, but
            // prefer the sole key press for the guided result. Completion still
            // waits until every key and axis has returned to neutral.
            if control.1 == 3 && (prefer_key || self.candidate.is_some_and(|c| c.1 == 1)) {
                continue;
            }
            let returned = match control.1 {
                1 => value == 0,
                3 => self.baseline.get(&(control.0, control.2)).is_some_and(|axis|
                    AxisState { value, ..*axis }.neutral()),
                _ => continue,
            };
            if control.1 == 1 && value == 2 { continue; }
            if returned && self.candidate != Some(control) { continue; }
            match self.candidate {
                None if !returned => self.candidate = Some(control),
                Some(previous) if previous != control => self.ambiguous = true,
                _ => {}
            }
            if self.candidate == Some(control) {
                self.released = returned;
                if control.1 == 3 && !returned {
                    if let Some(axis) = self.baseline.get(&(control.0, control.2)) {
                        let delta = i64::from(value) - i64::from(axis.baseline);
                        let direction = delta.signum() as i8;
                        match &mut self.axis_evidence {
                            Some(evidence) => {
                                if evidence.direction != direction { self.ambiguous = true; }
                                if delta.abs() > (i64::from(evidence.extreme) - i64::from(evidence.baseline)).abs() {
                                    evidence.extreme = value;
                                }
                            }
                            None => self.axis_evidence = Some(AxisEvidence {
                                baseline: axis.baseline, extreme: value, direction,
                            }),
                        }
                    } else { self.ambiguous = true; }
                }
            }
        }
        if self.candidate.is_some() && self.released && released && !self.ambiguous {
            self.advance(ms, "completed", &mut records);
        }
        records
    }

    fn advance(&mut self, ms: u64, outcome: &str, records: &mut Vec<Value>) {
        let result = json!({"event":"step_result","step":self.index,"action":ACTIONS[self.index],
            "started_ms":self.started_ms,"ms":ms,"outcome":outcome,"control":self.candidate,
            "observed_kind":match self.candidate {Some((_,1,_))=>"key",Some((_,3,_))=>"axis",_=>"none"},
            "axis":self.axis_evidence,
            "mapping_written":false});
        self.results.push(result.clone());
        records.push(result);
        self.index += 1;
        self.started_ms = ms;
        self.armed = false;
        self.candidate = None;
        self.released = false;
        self.ambiguous = false;
        self.axis_evidence = None;
        self.baseline.clear();
        self.quiet_since = ms;
        if let Some(action) = self.prompt() {
            records.push(json!({"event":"stage","step":self.index,"action":action,"ms":ms,"deadline_ms":ms + STEP_MS}));
        }
    }
}
