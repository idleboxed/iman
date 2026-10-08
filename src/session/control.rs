// SPDX-License-Identifier: BSD-3-Clause
//! Session-owned RPC dispatch. Only the registered child can submit GUI state.
use crate::emulation::launcher;
use imanlib::{
    igui::{Bootstrap, GuiState, Handoff, LaunchRequest, CONTROL_COMMANDS},
    protocol::{Message, Outcome, Request, Response, RpcError},
    server::{Incoming, PollError, Server},
};
#[cfg(feature = "diag-input")]
use imanlib::igui::InputTestAcknowledgement;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    process::ExitStatus,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, RpcError>;
fn decode<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|e| RpcError::new("invalid_params", e))
}
#[derive(serde::Deserialize)]
struct OperationQuery {
    operation_id: u64,
}

pub struct Control {
    server: Server,
    root: PathBuf,
    root_handle: Option<std::sync::Arc<crate::runtime::bundle::Root>>,
    parent: PathBuf,
    launch_log: Option<crate::runtime::startup::RamLog>,
    phase: String,
    child: Option<u32>,
    gui: Option<u64>,
    bootstrap: Bootstrap,
    handoff: Option<Handoff>,
    closed: bool,
    close_sent: bool,
    exit_operation: Option<u64>,
    gui_started: Instant,
    operations: BTreeMap<u64, Value>,
    next_operation: u64,
    #[cfg(feature = "diag-input")]
    input_test: crate::input_test::journal::Journal,
    #[cfg(feature = "diag-input")]
    input_test_requested: bool,
    #[cfg(feature = "diag-cycle")]
    started: Instant,
    #[cfg(feature = "diag-cycle")]
    cycles: Option<Box<crate::diagnostics::cycle::Session>>,
    #[cfg(feature = "diag-cycle")]
    settling: Duration,
    #[cfg(any(
        feature = "diag-memory",
        feature = "diag-klog", feature = "diag-uart",
        feature = "diag-display",
        feature = "diag-input"
    ))]
    diagnostics: crate::diagnostics::collectors::Collectors,
    #[cfg(feature = "io-monitor")]
    monitor: Option<crate::diagnostics::io::Monitor>,
    #[cfg(feature = "io-monitor")]
    io_context: Option<crate::diagnostics::io_context::Monitor>,
    #[cfg(feature = "io-monitor")]
    monitor_error: Option<String>,
    #[cfg(feature = "io-monitor")]
    monitor_requested: bool,
}
impl Control {
    /// Explicitly acquire a private IPC socket under the verified tmpfs parent.
    pub fn bind(root: &Path, parent: &Path) -> io::Result<Self> {
        let server = Server::bind(parent)?;
        crate::session::diagnostic(format_args!(
            "iman: IPC socket: {}",
            server.path().display()
        ));
        #[cfg(feature = "diag-cycle")]
        let started = Instant::now();
        let control = Self {
            server,
            root: root.into(),
            root_handle: None,
            parent: parent.into(),
            launch_log: None,
            phase: "starting".into(),
            child: None,
            gui: None,
            bootstrap: Bootstrap {
                input_test: cfg!(feature = "diag-input"),
                diag_display: false,
                diag_input: false,
                preferences_writable: true,
                root: root.into(),
                state: GuiState::default(),
                notice: None,
            },
            handoff: None,
            closed: false,
            close_sent: false,
            exit_operation: None,
            gui_started: Instant::now(),
            operations: BTreeMap::new(),
            next_operation: 1,
            #[cfg(feature = "diag-input")]
            input_test: crate::input_test::journal::Journal::default(),
            #[cfg(feature = "diag-input")]
            input_test_requested: false,
            #[cfg(feature = "diag-cycle")]
            started,
            #[cfg(feature = "diag-cycle")]
            cycles: None,
            #[cfg(feature = "diag-cycle")]
            settling: Duration::ZERO,
            #[cfg(any(
                feature = "diag-memory",
                feature = "diag-klog", feature = "diag-uart",
                feature = "diag-display",
                feature = "diag-input"
            ))]
            diagnostics: crate::diagnostics::collectors::Collectors::on_clock(started),
            #[cfg(feature = "io-monitor")]
            monitor: None,
            #[cfg(feature = "io-monitor")]
            io_context: None,
            #[cfg(feature = "io-monitor")]
            monitor_error: None,
            #[cfg(feature = "io-monitor")]
            monitor_requested: false,
        };
        Ok(control)
    }
    /// Keep startup's actual log separate from the configurable IPC/game parent.
    pub fn attach_launch_log(&mut self, log: crate::runtime::startup::RamLog) {
        self.launch_log = Some(log);
    }

    pub fn attach_root(&mut self, root: crate::runtime::bundle::Root) -> io::Result<()> {
        if self.root_handle.is_some() || root.path() != self.root {
            return Err(io::Error::other("installation root is already held or mismatched"));
        }
        root.validate()?;
        self.root_handle = Some(std::sync::Arc::new(root));
        Ok(())
    }

    /// Acquire once, then refuse changes to the selected installation root.
    pub fn prepare_root(&mut self) -> io::Result<std::sync::Arc<crate::runtime::bundle::Root>> {
        if self.root_handle.is_none() {
            self.root_handle = Some(std::sync::Arc::new(crate::runtime::bundle::Root::open(&self.root)?));
        }
        let root = self.root_handle.as_ref().unwrap();
        root.validate()?;
        Ok(root.clone())
    }

    /// Application entry starts exactly the passive collectors selected at build time.
    pub fn start_diagnostics(&mut self) -> io::Result<()> {
        #[cfg(feature = "diag-cycle")]
        {
            if crate::diagnostics::cycle::compiled_modules().is_empty() { return Ok(()); }
            let root = self.prepare_root()?;
            let writer: crate::diagnostics::cycle::Writer = std::sync::Arc::new(move |run, bundle| {
                crate::diagnostics::export::write_root(&root, run, bundle)
            });
            self.enable_cycle_exports(writer, Duration::from_secs(crate::diagnostics::cycle::SETTLING_SECONDS))?;
            #[cfg(any(feature = "diag-memory", feature = "diag-klog", feature = "diag-uart", feature = "diag-display", feature = "diag-input"))]
            self.diagnostics.enable_defaults();
            #[cfg(feature = "io-monitor")]
            {
                self.io_context = Some(crate::diagnostics::io_context::Monitor::new(
                    crate::diagnostics::io_context::Source::system(&self.root)));
                let root = self.prepare_root()?;
                self.enable_monitor_with(|| {
                    use std::os::unix::fs::MetadataExt;
                    crate::diagnostics::io::partition_devices(std::path::Path::new("/sys"), root.directory()?.metadata()?.dev())
                });
            }
        }
        Ok(())
    }

    pub fn set_preferences_writable(&mut self, writable: bool) {
        self.bootstrap.preferences_writable = writable;
    }
    pub fn runtime_parent(&self) -> &Path {
        &self.parent
    }
    pub fn path(&self) -> &Path {
        self.server.path()
    }
    #[cfg(feature = "io-monitor")]
    pub fn enable_monitor(&mut self, names: Vec<String>) {
        self.enable_monitor_with(|| Ok(names));
    }
    /// Discovery failures are observation errors, not session startup failures.
    #[cfg(feature = "io-monitor")]
    pub fn enable_monitor_with(
        &mut self,
        devices: impl FnOnce() -> io::Result<Vec<String>>,
    ) {
        self.monitor_requested = true;
        match devices().and_then(|names| crate::diagnostics::io::Monitor::start_at(names, &self.parent, self.started)) {
            Ok(mut m) => {
                if self.cycles.is_some() {
                    if let Err(error) = m.begin_segment("session_start") {
                        self.monitor_error = Some(error.to_string());
                    }
                }
                self.monitor = Some(m);
            },
            Err(e) => {
                crate::session::diagnostic(format_args!("iman: I/O observation unavailable: {e}"));
                self.monitor_error = Some(e.to_string());
            }
        }
    }
    pub(crate) fn diagnostic_clock(&self) -> Option<Instant> {
        #[cfg(feature = "diag-cycle")]
        { return Some(self.started); }
        #[cfg(not(feature = "diag-cycle"))]
        None
    }

    pub fn input_test_requested(&self) -> bool {
        #[cfg(feature = "diag-input")]
        { return self.input_test_requested; }
        #[cfg(not(feature = "diag-input"))]
        false
    }

    #[cfg(feature = "diag-input")]
    pub fn take_input_test_request(&mut self) -> bool {
        if self.child.is_some() { return false; }
        std::mem::take(&mut self.input_test_requested) && self.exit_operation.is_none()
    }

    #[cfg(feature = "diag-input")]
    pub fn record_input_test(&mut self, report: Value) {
        self.input_test.complete_manager(report);
    }

    #[cfg(feature = "diag-input")]
    pub fn record_input_test_completion(&mut self, completion: crate::input_test::Completion) {
        self.input_test.complete_with_proposals(completion);
    }

    /// Available only after the worker returned successfully from synchronization.
    pub fn synchronized_report_path(&self) -> Option<&Path> {
        #[cfg(feature = "diag-cycle")]
        {
            let cycles = self.cycles.as_ref()?;
            if cycles.pending() || cycles.error.is_some() { return None; }
            let receipt = cycles.previous_export.as_ref()?;
            if receipt["status"] != "synchronized" { return None; }
            return receipt["path"].as_str().map(Path::new);
        }
        #[cfg(not(feature = "diag-cycle"))]
        None
    }

    pub fn begin_gui(&mut self, pid: u32, state: GuiState, notice: Option<String>) {
        #[cfg(feature = "diag-input")]
        { self.input_test_requested = false; }
        #[cfg(feature = "diag-cycle")]
        {
            let ms = self.elapsed_ms();
            if let Some(segment) = self.cycle_segment() {
                segment.gui_result = Some(json!({"pid":pid,"spawn_observed_ms":ms}));
            }
        }
        self.child = Some(pid);
        self.gui = None;
        self.handoff = None;
        self.closed = false;
        self.close_sent = false;
        self.gui_started = Instant::now();
        self.phase = "gui".into();
        self.bootstrap.state = state;
        self.bootstrap.notice = notice;
    }
    pub fn end_gui(&mut self, status: ExitStatus) -> io::Result<Option<Handoff>> {
        #[cfg(feature = "diag-cycle")]
        {
            use std::os::unix::process::ExitStatusExt;
            let pid = self.child;
            let ms = self.elapsed_ms();
            if let Some(segment) = self.cycle_segment() {
                let result = segment.gui_result.get_or_insert_with(|| json!({"pid":pid}));
                result["exit_code"] = json!(status.code());
                result["signal"] = json!(status.signal());
                result["exit_observed_ms"] = json!(ms);
            }
        }
        #[cfg(any(
            feature = "diag-memory",
            feature = "diag-klog", feature = "diag-uart",
            feature = "diag-display",
            feature = "diag-input"
        ))]
        self.diagnostics.end_gui();
        self.child = None;
        self.gui = None;
        if !status.success() {
            #[cfg(feature = "diag-input")]
            { self.input_test_requested = false; }
            return Err(io::Error::other(format!("IGUI exited with {status}")));
        }
        if !self.closed && self.handoff.is_none() && !self.input_test_requested() {
            return Err(io::Error::other(
                "IGUI exited without an explicit IPC handoff or close",
            ));
        }
        Ok(self.handoff.take())
    }
    /// The launch adapter has already killed/reaped any failed GUI child.
    pub fn gui_failed(&mut self) {
        #[cfg(feature = "diag-input")]
        { self.input_test_requested = false; }
        #[cfg(any(
            feature = "diag-memory",
            feature = "diag-klog", feature = "diag-uart",
            feature = "diag-display",
            feature = "diag-input"
        ))]
        self.diagnostics.end_gui();
        self.child = None;
        self.gui = None;
        self.handoff = None;
        self.phase = "gui_error".into();
    }
    pub fn set_game(&mut self, pid: Option<u32>) {
        self.child = pid;
        self.phase = if pid.is_some() { "game" } else { "game_exit" }.into();
    }
    pub fn gui_health(&self) -> io::Result<()> {
        if self.closed || self.handoff.is_some() || self.input_test_requested() {
            return Ok(());
        }
        if self.gui.is_some_and(|p| !self.server.connected(p)) {
            return Err(io::Error::other(
                "IGUI IPC disconnected without exit intent",
            ));
        }
        if self.gui.is_none() && self.gui_started.elapsed() > Duration::from_secs(10) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "IGUI IPC bootstrap timed out",
            ));
        }
        Ok(())
    }
    fn operation(&mut self) -> u64 {
        if self.operations.len() >= 32 {
            if let Some(id) = self
                .operations
                .iter()
                .find(|(_, v)| v["status"] != "pending")
                .map(|(id, _)| *id)
            {
                self.operations.remove(&id);
            }
        }
        let id = self.next_operation;
        self.next_operation += 1;
        self.operations.insert(id, json!({"status":"pending"}));
        id
    }

    pub fn poll(&mut self, phase: &str) {
        self.phase = phase.into();
        #[cfg(any(
            feature = "diag-memory",
            feature = "diag-klog", feature = "diag-uart",
            feature = "diag-display",
            feature = "diag-input"
        ))]
        self.diagnostics.poll(phase, std::process::id(), self.child);
        #[cfg(feature = "io-monitor")]
        if let Some(m) = &mut self.monitor {
            m.poll(phase);
        }
        #[cfg(feature = "io-monitor")]
        {
            let ms = self.elapsed_ms();
            if let Some(m) = &mut self.io_context { m.poll(ms, phase, std::process::id(), self.child); }
        }
        #[cfg(feature = "diag-cycle")]
        if self.cycles.is_some() {
            let ms = self.elapsed_ms();
            if let Some(cycles) = &mut self.cycles {
                cycles.phase(ms, phase);
                if cycles.poll(ms) {
                    if let Some(receipt) = &mut cycles.previous_export {
                        #[cfg(feature = "io-monitor")]
                        { receipt["io"] = self.monitor.as_ref().and_then(|m| m.segment_report())
                            .map(|r| json!({"baseline":r.baseline,"latest":r.latest,"totals":r.totals,
                                "error":r.error,"samples_seen":r.samples_seen})).unwrap_or(Value::Null); }
                        #[cfg(not(feature = "io-monitor"))]
                        { receipt["io"] = Value::Null; }
                    }
                    if let Some(error) = &cycles.error {
                        crate::session::diagnostic(format_args!("iman: automatic export disabled: {error}"));
                    }
                }
            }
        }
        let polled = self.server.poll(
            self.gui,
            (self.phase == "gui").then_some(self.child).flatten(),
        );
        for error in polled.errors {
            crate::session::diagnostic(format_args!("iman: {error}"));
        }
        for incoming in polled.incoming {
            if CONTROL_COMMANDS.contains(&incoming.request.command.as_str())
                && incoming.request.valid()
            {
                let result = if self.phase == "gui"
                    && !self.closed
                    && self.handoff.is_none()
                    && !self.close_sent
                {
                    self.gui
                        .ok_or_else(|| io::Error::other("GUI not registered"))
                        .and_then(|peer| self.server.forward(peer, &incoming))
                } else {
                    Err(io::Error::other("No active GUI"))
                };
                if let Err(error) = result {
                    self.send_message(
                        incoming.peer,
                        &Message::Response(Response::error(
                            incoming.request.id,
                            "unavailable",
                            error,
                        )),
                    );
                }
                continue;
            }
            let id = incoming.request.id;
            let result = if !incoming.request.valid() {
                Err(RpcError::new(
                    "invalid_request",
                    "Namespaced command and object parameters required",
                ))
            } else {
                self.dispatch(&incoming)
            };
            let response = Response {
                id,
                outcome: result.unwrap_or_else(|error| Outcome::Error { error }),
            };
            self.send_message(incoming.peer, &Message::Response(response));
        }
        if self.exit_operation.is_some() && !self.close_sent {
            if let Some(peer) = self.gui {
                self.close_sent = self.send_message(
                    peer,
                    &Message::Event {
                        event: "igui.close".into(),
                        params: json!({}),
                    },
                );
            }
        }
    }
    fn send_message(&mut self, peer: u64, message: &Message) -> bool {
        match self.server.send(peer, message) {
            Ok(()) => true,
            Err(error) => {
                let error = PollError::new(Some(peer), "send", error);
                crate::session::diagnostic(format_args!("iman: {error}"));
                false
            }
        }
    }
    fn dispatch(&mut self, incoming: &Incoming) -> Result<Outcome> {
        let Request {
            command, params, ..
        } = &incoming.request;
        let done = |result| Ok(Outcome::Done { result });
        match command.as_str() {
            "core.capabilities" => {
                let mut commands = vec![
                    "core.capabilities",
                    "core.status",
                    "core.operation_status",
                    "core.exit",
                    "igui.bootstrap",
                    "igui.closed",
                ];
                commands.extend_from_slice(CONTROL_COMMANDS);
                commands.extend(["igui.prepare_launch", "igui.handoff"]);
                #[cfg(feature = "diag-input")]
                commands.extend(["igui.input_test", "diag_input_test.report"]);
                #[cfg(any(
                    feature = "diag-memory",
                    feature = "diag-klog", feature = "diag-uart",
                    feature = "diag-display",
                    feature = "diag-input"
                ))]
                commands.extend(crate::diagnostics::collectors::Collectors::capabilities());
                #[cfg(feature = "io-monitor")]
                if self.monitor_requested {
                    commands.extend(["io_monitor.status", "io_monitor.report"]);
                }
                #[cfg(feature = "diag-cycle")]
                if self.cycles.is_some() { commands.push("diag_cycle.status"); }
                done(
                    json!({"commands":commands,"mode":"linux"}),
                )
            }
            #[cfg(feature = "diag-cycle")]
            "diag_cycle.status" => {
                done(self.cycle_status())
            }
            "core.status" => {
                done(
                    json!({"pid":std::process::id(),"phase":self.phase,"child_pid":self.child,"gui_connected":self.gui.is_some_and(|p|self.server.connected(p))}),
                )
            }
            "core.operation_status" => {
                let query: OperationQuery = decode(params.clone())?;
                done(
                    self.operations
                        .get(&query.operation_id)
                        .cloned()
                        .ok_or_else(|| {
                            RpcError::new("operation_unknown", "Operation missing or expired")
                        })?,
                )
            }
            "core.exit" => {
                if self.phase != "gui"
                    || self.child.is_none()
                    || self.handoff.is_some()
                    || self.closed
                {
                    return Err(RpcError::new("invalid_state","Graceful close requires an active GUI; game termination is not implemented"));
                }
                let id = if let Some(id) = self.exit_operation {
                    id
                } else {
                    let id = self.operation();
                    self.exit_operation = Some(id);
                    id
                };
                Ok(Outcome::Accepted { operation_id: id })
            }
            "igui.bootstrap" => {
                if self.phase != "gui" || self.child != Some(incoming.pid) || self.gui.is_some() {
                    return Err(RpcError::new(
                        "forbidden",
                        "Bootstrap requires the current unregistered GUI child",
                    ));
                }
                #[cfg(any(
                    feature = "diag-memory",
                    feature = "diag-klog", feature = "diag-uart",
                    feature = "diag-display",
                    feature = "diag-input"
                ))]
                {
                    self.bootstrap.diag_display = self.diagnostics.display_requested();
                    self.bootstrap.diag_input = self.diagnostics.input_requested();
                    self.diagnostics.begin_gui(incoming.pid);
                }
                #[cfg(feature = "diag-cycle")]
                {
                    let ms = self.elapsed_ms();
                    if let Some(result) = self.cycle_segment().and_then(|s| s.gui_result.as_mut()) {
                        result["bootstrap_received_ms"] = json!(ms);
                    }
                }
                self.gui = Some(incoming.peer);
                done(serde_json::to_value(&self.bootstrap).unwrap())
            }
            "igui.prepare_launch" | "igui.handoff" | "igui.closed" | "igui.input_test" => {
                if self.gui != Some(incoming.peer)
                    || self.child != Some(incoming.pid)
                    || self.closed
                    || self.handoff.is_some()
                {
                    return Err(RpcError::new(
                        "forbidden",
                        "Command requires the active registered GUI connection",
                    ));
                }
                if command == "igui.closed" {
                    self.closed = true;
                    return done(json!({}));
                }
                if self.exit_operation.is_some() {
                    return Err(RpcError::new("invalid_state", "GUI is closing"));
                }
                if self.input_test_requested() {
                    return Err(RpcError::new("invalid_state", "Input test handoff already received"));
                }
                if command == "igui.input_test" {
                    #[cfg(feature = "diag-input")]
                    {
                        self.input_test_requested = true;
                        return done(json!(InputTestAcknowledgement {
                            handoff_received: true,
                            test_started: false,
                        }));
                    }
                    #[cfg(not(feature = "diag-input"))]
                    return Err(RpcError::new("module_unavailable", "Input diagnostics were not compiled"));
                }
                if command == "igui.prepare_launch" {
                    let game: LaunchRequest = decode(params.clone())?;
                    self.prepare_root()
                        .and_then(|_| launcher::resolve(&self.root, &game))
                        .map_err(|e| RpcError::new("launch_refused", launcher::failure_details(&self.root, &game, e)))?;
                    done(json!({"ready":true}))
                } else {
                    let handoff: Handoff = decode(params.clone())?;
                    self.prepare_root()
                        .and_then(|_| launcher::resolve(&self.root, &handoff.game))
                        .map_err(|e| RpcError::new("launch_refused", launcher::failure_details(&self.root, &handoff.game, e)))?;
                    self.handoff = Some(handoff);
                    // Confirms only the handoff: spawning still requires successful waitpid.
                    done(json!({"handoff_received":true,"game_started":false}))
                }
            }
            "diag_display.submit" => {
                self.require_gui_report(incoming)?;
                #[cfg(feature = "diag-display")]
                {
                    let sample: imanlib::display_metrics::Sample = decode(params.clone())?;
                    self.diagnostics
                        .submit_display(incoming.pid, sample)
                        .map_err(|e| RpcError::new("invalid_state", e))?;
                    done(json!({"received":true}))
                }
                #[cfg(not(feature = "diag-display"))]
                Err(RpcError::new(
                    "module_unavailable",
                    "Owner diagnostics were not compiled",
                ))
            }
            "diag_input_test.report" => {
                #[cfg(feature = "diag-input")]
                return done(self.input_test.report());
                #[cfg(not(feature = "diag-input"))]
                Err(RpcError::new("module_unavailable", "Input diagnostics were not compiled"))
            }
            "diag_input.submit" => {
                self.require_gui_report(incoming)?;
                #[cfg(feature = "diag-input")]
                {
                    let sample: imanlib::input_metrics::Sample = decode(params.clone())?;
                    self.diagnostics
                        .submit_input(incoming.pid, sample)
                        .map_err(|e| RpcError::new("invalid_state", e))?;
                    done(json!({"received":true}))
                }
                #[cfg(not(feature = "diag-input"))]
                Err(RpcError::new(
                    "module_unavailable",
                    "Owner diagnostics were not compiled",
                ))
            }
            _ if command.starts_with("io_monitor.") => self.io_command(command),
            _ if command.starts_with("diag_memory.")
                || command.starts_with("diag_klog.")
                || command.starts_with("diag_display.")
                || command.starts_with("diag_input.") =>
            {
                #[cfg(any(
                    feature = "diag-memory",
                    feature = "diag-klog", feature = "diag-uart",
                    feature = "diag-display",
                    feature = "diag-input"
                ))]
                return self
                    .diagnostics
                    .command(command)
                    .map(|result| Outcome::Done { result })
                    .map_err(|error| {
                        RpcError::new(
                            match error.kind() {
                                io::ErrorKind::Unsupported => "module_unavailable",
                                io::ErrorKind::NotFound => "unknown_command",
                                _ => "invalid_state",
                            },
                            error,
                        )
                    });
                #[cfg(not(any(
                    feature = "diag-memory",
                    feature = "diag-klog", feature = "diag-uart",
                    feature = "diag-display",
                    feature = "diag-input"
                )))]
                Err(RpcError::new(
                    "module_unavailable",
                    "Diagnostics were not compiled",
                ))
            }
            _ => Err(RpcError::new(
                "unknown_command",
                "Unknown namespaced command",
            )),
        }
    }
    fn require_gui_report(&self, incoming: &Incoming) -> Result<()> {
        if self.gui != Some(incoming.peer)
            || self.child != Some(incoming.pid)
            || self.phase != "gui"
            || self.closed
        {
            return Err(RpcError::new(
                "forbidden",
                "Report requires the registered GUI connection",
            ));
        }
        Ok(())
    }

    #[cfg(not(feature = "io-monitor"))]
    fn io_command(&mut self, _command: &str) -> Result<Outcome> {
        Err(RpcError::new(
            "module_unavailable",
            "I/O monitoring was not compiled",
        ))
    }
    #[cfg(feature = "io-monitor")]
    fn io_command(&mut self, command: &str) -> Result<Outcome> {
        if !self.monitor_requested {
            return Err(RpcError::new(
                "module_unavailable",
                "I/O monitoring is not enabled",
            ));
        }
        let report = self.monitor.as_ref().map(|m| m.report());
        let result = match command {
            "io_monitor.status" => {
                json!({"state":match report {Some(r) if r.error.is_some()=>"failed",Some(r) if r.finished=>"stopped",Some(_)=>"observing",None=>"unavailable"},"elapsed_ms":report.map(|r|r.elapsed_ms),"totals":report.map(|r|&r.totals),"error":self.monitor_error.as_deref().or_else(||report.and_then(|r|r.error.as_deref()))})
            }
            "io_monitor.report" => {
                json!({"observation":report,"observation_error":self.monitor_error})
            }
            _ => {
                return Err(RpcError::new(
                    "unknown_command",
                    "Unknown I/O monitor command",
                ))
            }
        };
        Ok(Outcome::Done { result })
    }
    #[cfg(feature = "io-monitor")]
    pub fn observation_report(&self) -> Option<&crate::diagnostics::io::Report> {
        self.monitor.as_ref().map(|m| m.report())
    }
    pub fn stop_monitor(&mut self) {
        #[cfg(feature = "io-monitor")]
        if let Some(m) = &mut self.monitor {
            m.finish();
        }
    }
    /// Acquire in-memory collectors for low-level library callers. The application
    /// uses start_diagnostics, which also configures source discovery and export.
    pub fn enable_diagnostics(&mut self) -> io::Result<()> {
        #[cfg(any(
            feature = "diag-memory", feature = "diag-klog", feature = "diag-uart",
            feature = "diag-display", feature = "diag-input"
        ))]
        {
            self.diagnostics.enable_defaults();
            Ok(())
        }
        #[cfg(not(any(
            feature = "diag-memory", feature = "diag-klog", feature = "diag-uart",
            feature = "diag-display", feature = "diag-input"
        )))]
        Err(io::Error::other("this build has no diagnostic collectors"))
    }

    pub fn display_diagnostics_requested(&self) -> bool {
        #[cfg(feature = "diag-display")]
        return self.diagnostics.display_requested();
        #[cfg(not(feature = "diag-display"))]
        false
    }

    /// A local owner reports directly, never through the child-authorized IPC command.
    pub fn record_manager_display(
        &mut self,
        _phase: &str,
        _observation: Option<(Duration, imanlib::display_metrics::Sample)>,
    ) {
        #[cfg(feature = "diag-display")]
        if self.display_diagnostics_requested() {
            if let Some((elapsed, sample)) = _observation {
                if let Err(error) = self.diagnostics.submit_manager_display(_phase, elapsed, sample) {
                    crate::session::diagnostic(format_args!("iman: manager display report failed: {error}"));
                }
            }
        }
    }
    /// Status for a bounded diagnostic interval without a working GUI.
    pub fn diagnostics_requested(&self) -> bool {
        #[cfg(any(
            feature = "diag-memory",
            feature = "diag-klog", feature = "diag-uart",
            feature = "diag-display",
            feature = "diag-input"
        ))]
        if self.diagnostics.display_requested() || self.diagnostics.input_requested() {
            return true;
        }
        !self.diagnostics_status().is_empty()
    }

    /// Hide unavailable child-only data when no GUI is running.
    pub fn diagnostics_status(&self) -> Vec<String> {
        #[allow(unused_mut)] // Every diagnostic feature can be compiled out.
        let mut lines = Vec::new();
        #[cfg(feature = "io-monitor")]
        if let Some(error) = &self.monitor_error {
            lines.push(format!("I/O: unavailable ({})", error.escape_debug()));
        } else if let Some(m) = &self.monitor {
            let r = m.report();
            if let Some(error) = &r.error {
                lines.push(format!("I/O: failed ({})", error.escape_debug()));
            } else if !r.finished {
                let writes = r.baseline.iter().zip(&r.totals)
                    .map(|(device, writes)| format!("{} {} B", device.name, writes.bytes))
                    .collect::<Vec<_>>().join(", ");
                lines.push(format!("I/O: {} samples; written {writes}", r.samples_seen));
            }
        }
        #[cfg(any(
            feature = "diag-memory",
            feature = "diag-klog", feature = "diag-uart",
            feature = "diag-display",
            feature = "diag-input"
        ))]
        lines.extend(self.diagnostics.status_without_gui());
        lines
    }
    /// Finalize RAM collectors; segment snapshots do not call this operation.
    pub fn stop_diagnostics(&mut self) {
        self.stop_monitor();
        #[cfg(feature = "io-monitor")]
        if let Some(m) = &mut self.io_context { m.stop(); }
        #[cfg(any(
            feature = "diag-memory",
            feature = "diag-klog", feature = "diag-uart",
            feature = "diag-display",
            feature = "diag-input"
        ))]
        self.diagnostics.stop_and_emit();
    }
    #[cfg(feature = "diag-cycle")]
    pub fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
    }

    /// Explicit dependency injection for embedders, not a production CLI bypass.
    #[cfg(feature = "diag-cycle")]
    pub fn enable_cycle_exports(&mut self, writer: crate::diagnostics::cycle::Writer, settling: Duration) -> io::Result<()> {
        if self.cycles.is_some() || self.child.is_some() || settling > Duration::from_secs(60) {
            return Err(io::Error::other("cycle export already configured, child active or invalid settling interval"));
        }
        let suffix = self.path().parent().and_then(Path::file_name)
            .and_then(|s| s.to_str()).ok_or_else(|| io::Error::other("invalid session identity"))?;
        let id = format!("iman-{}-{suffix}", std::process::id());
        self.cycles = Some(Box::new(crate::diagnostics::cycle::Session::new(id, writer, self.elapsed_ms())));
        self.settling = settling;
        #[cfg(feature = "io-monitor")]
        if let Some(m) = &mut self.monitor { m.begin_segment("session_start")?; }
        Ok(())
    }

    pub fn cycle_configured(&self) -> bool {
        #[cfg(feature = "diag-cycle")]
        return self.cycles.is_some();
        #[cfg(not(feature = "diag-cycle"))]
        false
    }

    pub fn disable_cycle_exports(&mut self, _error: &str) {
        #[cfg(feature = "diag-cycle")]
        if let Some(cycles) = &mut self.cycles { cycles.disable(_error); }
    }

    pub fn cycle_enabled(&self) -> bool {
        #[cfg(feature = "diag-cycle")]
        return self.cycles.as_ref().is_some_and(|s| s.error.is_none() && s.active.is_some());
        #[cfg(not(feature = "diag-cycle"))]
        false
    }

    pub fn settling_interval(&self) -> Duration {
        #[cfg(feature = "diag-cycle")]
        if self.cycle_enabled() { return self.settling; }
        Duration::ZERO
    }

    pub fn cycle_pending(&self) -> bool {
        #[cfg(feature = "diag-cycle")]
        return self.cycles.as_ref().is_some_and(|s| s.pending());
        #[cfg(not(feature = "diag-cycle"))]
        false
    }

    pub fn cycle_error(&self) -> Option<&str> {
        #[cfg(feature = "diag-cycle")]
        return self.cycles.as_ref().and_then(|s| s.error.as_deref());
        #[cfg(not(feature = "diag-cycle"))]
        None
    }

    pub fn cycle_status(&self) -> Value {
        #[cfg(feature = "diag-cycle")]
        return self.cycles.as_ref().map(|s| s.status()).unwrap_or(json!({"state":"disabled"}));
        #[cfg(not(feature = "diag-cycle"))]
        json!({"state":"disabled"})
    }

    #[cfg(feature = "diag-cycle")]
    pub fn cycle_segment(&mut self) -> Option<&mut crate::diagnostics::cycle::Segment> {
        self.cycles.as_mut().and_then(|s| s.active.as_mut())
    }

    /// Freeze inputs, then announce the write before dispatching its worker.
    /// The notification cannot alter the already owned collector snapshot.
    pub fn export_segment(
        &mut self,
        _error: Option<&str>,
        _before_write: impl FnOnce(),
    ) -> io::Result<bool> {
        #[cfg(feature = "diag-cycle")]
        {
            if !self.cycle_enabled() { return Ok(false); }
            if self.child.is_some() { return Err(io::Error::other("cannot export while a child is active")); }
            #[cfg(feature = "io-monitor")]
            if let Some(m) = &mut self.monitor { m.boundary("diagnostic_export"); }
            let ms = self.elapsed_ms();
            let Some(mut bundle) = self.cycles.as_mut().and_then(|s| s.freeze(ms, _error)) else { return Ok(false); };
            #[cfg(feature = "io-monitor")]
            {
                bundle.json("observation.json", &self.monitor.as_ref().map(|m| m.report()))?;
                bundle.json("cycle-io.json", &self.monitor.as_ref().and_then(|m| m.segment_report()))?;
                bundle.metadata["io_context"] = match self.io_context.as_ref().map(|m| m.report_json()).transpose() {
                    Ok(Some(text)) => serde_json::from_str(&text).map_err(io::Error::other)?,
                    Ok(None) => json!({"state":"disabled"}),
                    Err(error) => json!({"state":"failed", "error":error.to_string()}),
                };
                bundle.metadata["observation_error"] = json!(self.monitor_error);
                bundle.metadata["io_state"] = json!(match self.monitor.as_ref().map(|m| m.report()) {
                    Some(r) if r.error.is_some() => "failed",
                    Some(r) if r.finished => "stopped",
                    Some(_) => "observing",
                    None if self.monitor_requested => "unavailable",
                    None => "disabled",
                });
                bundle.metadata["io_scope"] = json!({"observation.json":"session", "cycle-io.json":"segment", "clock":"iman_session"});
            }
            #[cfg(not(feature = "io-monitor"))]
            { bundle.metadata["io_state"] = json!("not_compiled"); }
            #[cfg(any(feature = "diag-memory", feature = "diag-klog", feature = "diag-uart", feature = "diag-display", feature = "diag-input"))]
            for (name, data) in self.diagnostics.snapshot() { bundle.json(&name, &data)?; }
            #[cfg(feature = "diag-input")]
            {
                bundle.json("diag_input_test.json", &self.input_test.report())?;
                bundle.files.extend(self.input_test.proposal_files().clone());
            }
            let log = self.launch_log.as_ref()
                .ok_or_else(|| io::Error::other("no attached launch log"))
                .and_then(crate::runtime::startup::RamLog::snapshot);
            bundle.metadata["launch_log"] = match &log {
                Ok(bytes) => json!({"bytes":bytes.len(),"possibly_truncated":bytes.len() as u64 == crate::runtime::startup::LOG_LIMIT}),
                Err(error) => json!({"state":"unavailable","error":error.to_string()}),
            };
            bundle.files.insert("launch.log".into(), log.unwrap_or_default());
            // This new interval measures the writer without resetting master counters.
            #[cfg(feature = "io-monitor")]
            if let Some(m) = &mut self.monitor { m.begin_segment("diagnostic_export")?; }
            self.phase = "diagnostic_export".into();
            _before_write();
            self.cycles.as_mut().unwrap().start(bundle, ms)?;
            return Ok(true);
        }
        #[cfg(not(feature = "diag-cycle"))]
        Ok(false)
    }

    /// Start a fresh segment only after a successful write and child cleanup.
    pub fn next_segment(&mut self) {
        #[cfg(feature = "diag-cycle")]
        {
            let Some(cycles) = &self.cycles else { return; };
            if cycles.pending() { return; }
            if cycles.error.is_none() && cycles.saved > 0 {
                if let Some(log) = &self.launch_log {
                    if let Err(error) = log.reset() {
                        self.cycles.as_mut().unwrap().disable(&format!("log rotation failed: {error}"));
                        return;
                    }
                }
            }
            let ms = self.elapsed_ms();
            #[cfg(feature = "diag-input")]
            let previous_sequence = self.cycles.as_ref().and_then(|s| s.active.as_ref()).map(|s| s.sequence);
            self.cycles.as_mut().unwrap().begin_next(ms);
            #[cfg(feature = "diag-input")]
            if self.cycle_enabled()
                && previous_sequence != self.cycles.as_ref().and_then(|s| s.active.as_ref()).map(|s| s.sequence)
            { self.input_test.next_segment(); }
            #[cfg(feature = "io-monitor")]
            if self.cycle_enabled() {
                if let Some(m) = &mut self.monitor {
                    m.boundary("starting_gui");
                    if let Some(receipt) = &mut self.cycles.as_mut().unwrap().previous_export {
                        receipt["end_ms"] = json!(ms);
                        receipt["io"] = m.segment_report().map(|r| json!({
                            "baseline_ms":r.baseline_ms,"end_ms":r.elapsed_ms,
                            "baseline":r.baseline,"latest":r.latest,"totals":r.totals,"error":r.error
                        })).unwrap_or(Value::Null);
                    }
                    if let Err(error) = m.begin_segment("starting_gui") {
                        self.monitor_error = Some(error.to_string());
                    }
                }
            }
        }
    }

    pub fn finish(&mut self, error: Option<String>, cancelled: bool) -> io::Result<()> {
        // Fallback for failure before a display/session runner was constructed.
        if !cancelled && self.cycle_enabled() {
            if let Err(error) = self.export_segment(error.as_deref(), || {}) {
                #[cfg(feature = "diag-cycle")]
                if let Some(cycles) = &mut self.cycles { cycles.disable(&error.to_string()); }
                crate::session::diagnostic(format_args!("iman: export failed: {error}"));
            }
        }
        while self.cycle_pending() {
            self.poll("diagnostic_export");
            std::thread::sleep(Duration::from_millis(20));
        }
        self.stop_diagnostics();
        let failed = error.is_some() || cancelled || self.cycle_error().is_some();
        if let Some(id) = self.exit_operation {
            self.operations.insert(id, if failed {json!({"status":"error","error":{"code":"session_failed","message":"Session finalization failed"}})} else {json!({"status":"done","result":{"session_finalized":true}})});
        }
        if let Some(error) = self.cycle_error() {
            return Err(io::Error::other(format!("diagnostic export failed: {error}")));
        }
        Ok(())
    }
}
