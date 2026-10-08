// SPDX-License-Identifier: BSD-3-Clause
//! Passive aggregation; rendering and KMS measurements stay at each display owner.
use std::{io, time::{Duration, Instant}};

use imanlib::display_metrics::{Backend, KmsState, Statistics, Timing};
use serde::Serialize;

pub use crate::diagnostics::child::{Clock, Entry, Report, HISTORY_LIMIT, REPORT_LIMIT, SAMPLE_LIMIT};
pub use imanlib::display_metrics::Sample;
pub type Monitor = crate::diagnostics::child::Monitor<Sample>;
impl crate::diagnostics::child::Payload for Sample {
    fn valid(&self) -> bool {
        self.validate()
    }
}

/// The same bounded recorder for both owners, with independent histories/totals.
/// Existing top-level counters remain IGUI-only; no manager sample impersonates a child.
pub fn report_json(gui: &Monitor, manager: &Monitor) -> io::Result<String> {
    #[derive(Serialize)]
    struct Sourced {
        source: &'static str,
        #[serde(flatten)]
        report: Report<Sample>,
    }
    #[derive(Serialize)]
    struct Combined {
        #[serde(flatten)]
        gui: Sourced,
        manager: Sourced,
    }
    let mut report = Combined {
        gui: Sourced { source: "igui", report: gui.report().clone() },
        manager: Sourced { source: "iman", report: manager.report().clone() },
    };
    loop {
        let text = serde_json::to_string(&report).map_err(io::Error::other)?;
        if text.len() <= REPORT_LIMIT {
            return Ok(text);
        }
        let gui_time = report.gui.report.history.front().map(|entry| entry.end_ms);
        let manager_time = report.manager.report.history.front().map(|entry| entry.end_ms);
        let oldest = match (gui_time, manager_time) {
            (None, None) => return Err(io::Error::other("display report exceeds limit")),
            (Some(_), None) => &mut report.gui.report,
            (None, Some(_)) => &mut report.manager.report,
            (Some(a), Some(b)) if a <= b => &mut report.gui.report,
            _ => &mut report.manager.report,
        };
        oldest.history.pop_front();
        oldest.history_dropped += 1;
    }
}

/// Per-phase owner measurements, reusing the shared IGUI statistics and timings.
/// In-memory collector; construction performs no device access.
pub struct Measurements {
    started: Instant,
    render: Statistics,
    present: Timing,
    operation_ok: bool,
}

impl Default for Measurements {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            render: Statistics::default(),
            present: Timing::default(),
            operation_ok: true,
        }
    }
}

impl Measurements {
    pub fn render(&mut self, elapsed: Duration, footer_only: bool) {
        let us = elapsed.as_micros();
        self.render.requests += 1;
        self.render.render_us_sum += us;
        self.render.render_us_max = self.render.render_us_max.max(us);
        if footer_only {
            self.render.telemetry += 1;
            self.render.telemetry_only_cost.record(us);
        } else {
            self.render.full += 1;
            self.render.full_cost.record(us);
        }
    }

    /// Diagnostic rows/title updated in the retained CPU frame, not a full redraw.
    pub fn render_partial(&mut self, elapsed: Duration) {
        let us = elapsed.as_micros();
        self.render.requests += 1;
        self.render.telemetry += 1;
        self.render.render_us_sum += us;
        self.render.render_us_max = self.render.render_us_max.max(us);
        self.render.partial_cost.record(us);
    }

    pub fn present(&mut self, elapsed: Duration, success: bool) {
        self.present.record(elapsed.as_micros());
        self.operation_ok &= success;
    }

    pub fn failed(&mut self) {
        self.operation_ok = false;
    }

    pub fn status(&self, kms: Option<&KmsState>) -> String {
        let output = kms.map_or_else(
            || "KMS unavailable (desktop)".into(),
            |kms| format!("{} completed flips", kms.completed_flips),
        );
        format!(
            "Display (iman): {} renders, {} presents, {output}{}",
            self.render.requests, self.present.count,
            if self.operation_ok { "" } else { "; failed" },
        )
    }

    pub fn finish(self, kms: KmsState, cleanup_ok: Option<bool>) -> (Duration, Sample) {
        self.sample(Backend::Drm, Some(kms), cleanup_ok)
    }

    /// Window submission timings are not physical KMS flip measurements.
    pub fn finish_desktop(self, cleanup_ok: Option<bool>) -> (Duration, Sample) {
        self.sample(Backend::Desktop, None, cleanup_ok)
    }

    fn sample(self, backend: Backend, kms: Option<KmsState>, cleanup_ok: Option<bool>) -> (Duration, Sample) {
        let elapsed = self.started.elapsed();
        (elapsed, Sample {
            backend,
            render: Some(self.render),
            kms,
            present: Some(self.present),
            idle_check: None,
            probe: None,
            cleanup_ok,
            operation_ok: Some(self.operation_ok),
        })
    }
}
