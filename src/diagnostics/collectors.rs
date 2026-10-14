// SPDX-License-Identifier: BSD-3-Clause
//! Optional collectors on one session clock; no thread, file or implicit startup.
use serde_json::{json, Value};
use std::{io, time::Instant};
#[cfg(feature = "diag-display")]
use std::time::Duration;

pub struct Collectors {
    started: Instant,
    #[cfg(feature = "diag-display")]
    display: Option<crate::diagnostics::display::Monitor>,
    #[cfg(feature = "diag-display")]
    manager_display: Option<crate::diagnostics::display::Monitor>,
    #[cfg(feature = "diag-input")]
    input: Option<crate::diagnostics::input::Monitor>,
    emitted: bool,
    #[cfg(feature = "diag-uart")]
    uart: Option<crate::diagnostics::uart::Monitor>,
    #[cfg(feature = "diag-uart")]
    uart_source: crate::diagnostics::uart::Source,
    #[cfg(feature = "diag-memory")]
    memory: Option<crate::diagnostics::memory::Monitor>,
    #[cfg(feature = "diag-klog")]
    klog: Option<crate::diagnostics::klog::Monitor>,
}
impl Default for Collectors {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            #[cfg(feature = "diag-display")]
            display: None,
            #[cfg(feature = "diag-display")]
            manager_display: None,
            #[cfg(feature = "diag-input")]
            input: None,
            emitted: false,
            #[cfg(feature = "diag-uart")]
            uart: None,
            #[cfg(feature = "diag-uart")]
            uart_source: crate::diagnostics::uart::Source::system(),
            #[cfg(feature = "diag-memory")]
            memory: None,
            #[cfg(feature = "diag-klog")]
            klog: None,
        }
    }
}
impl Collectors {
    /// Create collector state relative to the supplied session start instant.
    pub fn on_clock(started: Instant) -> Self {
        Self { started, ..Self::default() }
    }

    /// Alternate sources support offline fixtures without changing production paths.
    #[cfg(feature = "diag-uart")]
    pub fn with_uart_source(source: crate::diagnostics::uart::Source) -> Self {
        Self { uart_source: source, ..Self::default() }
    }

    /// Structured snapshots never stop collectors or depend on text log capacity.
    /// Existing counters/extrema retain collector-epoch (normally session) scope.
    pub fn snapshot(&self) -> std::collections::BTreeMap<String, Value> {
        #[allow(unused_mut)]
        let mut reports = std::collections::BTreeMap::new();
        let wrap = |value: io::Result<Option<String>>| match value {
            Ok(Some(text)) => match serde_json::from_str::<Value>(&text) {
                Ok(data) => json!({"scope":"collector_epoch", "clock":"iman_session", "data":data}),
                Err(error) => json!({"error":error.to_string()}),
            },
            Ok(None) => json!({"state":"disabled", "data":null}),
            Err(error) => json!({"state":"failed", "error":error.to_string()}),
        };
        #[cfg(feature = "diag-memory")]
        reports.insert("diag_memory.json".into(), wrap(self.memory.as_ref().map(|monitor| monitor.report_json()).transpose()));
        #[cfg(feature = "diag-klog")]
        reports.insert("diag_klog.json".into(), wrap(self.klog.as_ref().map(|monitor| monitor.report_json()).transpose()));
        #[cfg(feature = "diag-input")]
        reports.insert("diag_input.json".into(), wrap(self.input.as_ref().map(|monitor| monitor.report_json()).transpose()));
        #[cfg(feature = "diag-display")]
        reports.insert("diag_display.json".into(), wrap(self.display_report_json()));
        #[cfg(feature = "diag-uart")]
        reports.insert("diag_uart.json".into(), wrap(self.uart.as_ref().map(|monitor| monitor.report_json()).transpose()));
        reports
    }

    /// Enable only the passive collectors selected at build time.
    pub fn enable_defaults(&mut self) {
        #[cfg(feature = "diag-uart")]
        if self.uart.is_none() {
            self.uart = Some(crate::diagnostics::uart::Monitor::default());
        }
        #[cfg(feature = "diag-display")]
        {
            self.display = Some(crate::diagnostics::display::Monitor::default());
            self.manager_display = Some(crate::diagnostics::display::Monitor::default());
        }
        #[cfg(feature = "diag-input")]
        {
            self.input = Some(crate::diagnostics::input::Monitor::default());
        }
        #[cfg(feature = "diag-memory")]
        {
            self.memory = Some(crate::diagnostics::memory::Monitor::default());
        }
        #[cfg(feature = "diag-klog")]
        {
            self.klog = Some(crate::diagnostics::klog::Monitor::default());
        }
    }
    /// Sample enabled passive collectors for the current phase and process identities.
    pub fn poll(&mut self, _phase: &str, _manager: u32, _child: Option<u32>) {
        #[cfg(feature = "diag-uart")]
        if let Some(monitor) = &mut self.uart {
            let ms = self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
            monitor.poll(ms, _phase, || self.uart_source.capture());
        }
        #[allow(unused_mut)]
        let mut periodic = false;
        #[cfg(feature = "diag-memory")]
        { periodic |= self.memory.as_ref().is_some_and(|monitor| !monitor.report().stopped); }
        #[cfg(feature = "diag-klog")]
        { periodic |= self.klog.as_ref().is_some_and(|monitor| !monitor.report().stopped); }
        if !periodic { return; }
        #[cfg(any(feature = "diag-memory", feature = "diag-klog"))]
        let ms = self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        #[cfg(feature = "diag-memory")]
        if let Some(monitor) = &mut self.memory {
            monitor.poll(ms, _phase, || {
                crate::diagnostics::memory::Source::system().sample(_manager, _child)
            });
        }
        #[cfg(feature = "diag-klog")]
        if let Some(monitor) = &mut self.klog {
            monitor.poll(ms, _phase, crate::diagnostics::klog::capture);
        }
    }
    /// Absent GUI owners cannot supply display/input data.
    /// Reading status never polls sources or starts a compiled-but-disabled module.
    pub fn status_without_gui(&self) -> Vec<String> {
        #[allow(unused_mut)] // Display/input-only builds have no standalone sensor rows.
        let mut lines = Vec::new();
        #[cfg(feature = "diag-memory")]
        if let Some(monitor) = &self.memory {
            let report_item = monitor.report();
            if !report_item.stopped {
                lines.push(format!("RAM: {} samples, {} read errors", report_item.samples, report_item.read_errors));
            }
        }
        #[cfg(feature = "diag-klog")]
        if let Some(monitor) = &self.klog {
            let report_item = monitor.report();
            if !report_item.stopped {
                lines.push(format!("Kernel log: {} samples, {} read errors", report_item.samples, report_item.read_errors));
            }
        }
        #[cfg(feature = "diag-uart")]
        if let Some(monitor) = &self.uart {
            let report_item = monitor.report();
            if !report_item.stopped {
                lines.push(if report_item.samples == 0 {
                    "UART: waiting for diagnostic screen".into()
                } else {
                    format!("UART: {} snapshots, {} source errors", report_item.samples, report_item.read_errors)
                });
            }
        }
        lines
    }
    /// List IPC commands provided by collectors compiled into this build.
    pub fn capabilities() -> Vec<&'static str> {
        #[allow(unused_mut)]
        let mut commands = Vec::new();
        #[cfg(feature = "diag-memory")]
        commands.extend([
            "diag_memory.report",
        ]);
        #[cfg(feature = "diag-klog")]
        commands.extend([ "diag_klog.report"]);
        #[cfg(feature = "diag-display")]
        commands.extend([
            "diag_display.report",
            "diag_display.submit",
        ]);
        #[cfg(feature = "diag-input")]
        commands.extend([
            "diag_input.report",
            "diag_input.submit",
        ]);
        commands
    }
    /// Report whether an active display collector requests GUI reports.
    pub fn display_requested(&self) -> bool {
        #[cfg(feature = "diag-display")]
        return self.display.as_ref().is_some_and(|monitor| !monitor.report().stopped);
        #[cfg(not(feature = "diag-display"))]
        false
    }
    /// Report whether an active input collector requests GUI reports.
    pub fn input_requested(&self) -> bool {
        #[cfg(feature = "diag-input")]
        return self.input.as_ref().is_some_and(|monitor| !monitor.report().stopped);
        #[cfg(not(feature = "diag-input"))]
        false
    }
    /// Authorize the new GUI PID in enabled child-report collectors.
    pub fn begin_gui(&mut self, _pid: u32) {
        #[cfg(any(feature = "diag-display", feature = "diag-input"))]
        let ms = self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        #[cfg(feature = "diag-display")]
        if let Some(monitor) = &mut self.display {
            monitor.begin_gui(_pid, ms);
        }
        #[cfg(feature = "diag-input")]
        if let Some(monitor) = &mut self.input {
            monitor.begin_gui(_pid, ms);
        }
    }
    /// End report authorization for the current GUI in all enabled collectors.
    pub fn end_gui(&mut self) {
        #[cfg(feature = "diag-display")]
        if let Some(monitor) = &mut self.display {
            monitor.end_gui();
        }
        #[cfg(feature = "diag-input")]
        if let Some(monitor) = &mut self.input {
            monitor.end_gui();
        }
    }
    #[cfg(feature = "diag-display")]
    /// Validate and retain the authorized GUI's display sample.
    pub fn submit_display(
        &mut self,
        pid: u32,
        sample: imanlib::display_metrics::Sample,
    ) -> io::Result<()> {
        let ms = self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        self.display
            .as_mut()
            .ok_or_else(|| io::Error::other("display observation is disabled"))?
            .submit_gui(pid, ms, sample)
    }
    #[cfg(feature = "diag-display")]
    /// Retain a manager-owned display sample with its phase and elapsed interval.
    pub fn submit_manager_display(
        &mut self,
        phase: &str,
        elapsed: Duration,
        sample: imanlib::display_metrics::Sample,
    ) -> io::Result<()> {
        let ms = self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        let elapsed_ms = elapsed.as_millis().min(u128::from(u64::MAX)) as u64;
        self.manager_display.as_mut()
            .ok_or_else(|| io::Error::other("display observation is disabled"))?
            .record(std::process::id(), ms.saturating_sub(elapsed_ms), ms, phase, sample)
    }

    #[cfg(feature = "diag-display")]
    fn display_report_json(&self) -> io::Result<Option<String>> {
        self.display.as_ref().zip(self.manager_display.as_ref())
            .map(|(gui, manager)| crate::diagnostics::display::report_json(gui, manager))
            .transpose()
    }
    #[cfg(feature = "diag-input")]
    /// Validate and retain the authorized GUI's input sample.
    pub fn submit_input(
        &mut self,
        pid: u32,
        sample: imanlib::input_metrics::Sample,
    ) -> io::Result<()> {
        let ms = self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        self.input
            .as_mut()
            .ok_or_else(|| io::Error::other("input observation is disabled"))?
            .submit_gui(pid, ms, sample)
    }
    /// Dispatch one supported diagnostic command or return an error for an unavailable command.
    pub fn command(&mut self, command: &str) -> io::Result<Value> {
        match command {
            #[cfg(feature = "diag-input")]
            "diag_input.report" => self
                .input
                .as_ref()
                .map(|monitor| {
                    monitor.report_json()
                        .and_then(|sample_item| serde_json::from_str(&sample_item).map_err(io::Error::other))
                })
                .unwrap_or(Ok(Value::Null)),
            #[cfg(feature = "diag-display")]
            "diag_display.report" => self.display_report_json()?
                .map(|sample_item| serde_json::from_str(&sample_item).map_err(io::Error::other))
                .unwrap_or(Ok(Value::Null)),
            #[cfg(feature = "diag-memory")]
            "diag_memory.report" => self
                .memory
                .as_ref()
                .map(|monitor| {
                    monitor.report_json()
                        .and_then(|sample_item| serde_json::from_str(&sample_item).map_err(io::Error::other))
                })
                .unwrap_or(Ok(Value::Null)),
            #[cfg(feature = "diag-klog")]
            "diag_klog.report" => self
                .klog
                .as_ref()
                .map(|monitor| {
                    monitor.report_json()
                        .and_then(|sample_item| serde_json::from_str(&sample_item).map_err(io::Error::other))
                })
                .unwrap_or(Ok(Value::Null)),
            _ => {
                let compiled = (cfg!(feature = "diag-input") && command.starts_with("diag_input."))
                    || (cfg!(feature = "diag-display") && command.starts_with("diag_display."))
                    || (cfg!(feature = "diag-memory") && command.starts_with("diag_memory."))
                    || (cfg!(feature = "diag-klog") && command.starts_with("diag_klog."));
                Err(io::Error::new(
                    if compiled {
                        io::ErrorKind::NotFound
                    } else {
                        io::ErrorKind::Unsupported
                    },
                    "unknown or unavailable diagnostic command",
                ))
            }
        }
    }
    /// Retain final collector state and emit it once to the existing RAM log.
    pub fn stop_and_emit(&mut self) {
        if self.emitted {
            return;
        }
        self.emitted = true;
        #[cfg(feature = "diag-uart")]
        if let Some(monitor) = &mut self.uart {
            monitor.stop();
            // Keep the detailed snapshot only in its structured export, not twice in RAM logs.
            crate::session::diagnostic(format_args!("IMAN diag_uart: snapshots={} source_errors={}",
                monitor.report().samples, monitor.report().read_errors));
        }
        #[cfg(feature = "diag-input")]
        if let Some(monitor) = &mut self.input {
            monitor.stop();
            emit("diag_input", monitor.report_json());
        }
        #[cfg(feature = "diag-display")]
        if let Some(monitor) = &mut self.display {
            monitor.stop();
            if let Some(monitor) = &mut self.manager_display {
                monitor.stop();
            }
            if let Some(report) = self.display_report_json().transpose() {
                emit("diag_display", report);
            }
        }
        #[cfg(feature = "diag-memory")]
        if let Some(monitor) = &mut self.memory {
            monitor.stop();
            emit("diag_memory", monitor.report_json());
        }
        #[cfg(feature = "diag-klog")]
        if let Some(monitor) = &mut self.klog {
            monitor.stop();
            emit("diag_klog", monitor.report_json());
        }
    }
}
#[cfg(any(feature = "diag-memory", feature = "diag-klog", feature = "diag-display", feature = "diag-input"))]
fn emit(name: &str, report: io::Result<String>) {
    match report {
        Ok(report) => crate::session::diagnostic(format_args!("IMAN {name}: {report}")),
        Err(error) => {
            crate::session::diagnostic(format_args!("IMAN {name} report failed: {error}"))
        }
    }
}
