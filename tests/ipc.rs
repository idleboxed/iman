// SPDX-License-Identifier: BSD-3-Clause
//! Private tmpfs/local socket fixtures; no SD or GUI devices.
use iman::session::control::Control;
use imanlib::{
    client::Client,
    igui::GuiState,
    protocol::{Outcome, Response},
};
use serde_json::{json, Value};
use std::{
    io::Write,
    os::unix::net::UnixStream,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

fn exchange(control: &mut Control, client: &mut Client, command: &str, params: Value) -> Response {
    let id = client.send(command, params).unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        control.poll("gui");
        if let Some(r) = client.poll().unwrap() {
            assert_eq!(r.id, id);
            return r;
        }
        assert!(Instant::now() < until, "RPC timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn value(r: Response) -> Value {
    match r.outcome {
        Outcome::Done { result } => result,
        other => panic!("{other:?}"),
    }
}
fn error(r: Response, code: &str) {
    match r.outcome {
        Outcome::Error { error } => assert_eq!(error.code, code),
        other => panic!("{other:?}"),
    }
}
fn fixture() -> (tempfile::TempDir, Control) {
    let dir = tempfile::tempdir_in("/dev/shm").unwrap();
    let control = Control::bind(dir.path(), dir.path()).unwrap();
    (dir, control)
}
#[test]
fn namespaced_dispatch_permissions_and_peer_disconnect_are_not_exit_intent() {
    let (_dir, mut control) = fixture();
    control.begin_gui(std::process::id(), GuiState::default(), None);
    let mut gui = Client::connect(control.path()).unwrap();
    let mut other = Client::connect(control.path()).unwrap();
    value(exchange(&mut control, &mut gui, "igui.bootstrap", json!({})));
    error(
        exchange(&mut control, &mut other, "igui.closed", json!({})),
        "forbidden",
    );
    error(
        exchange(&mut control, &mut other, "igui.bootstrap", json!({})),
        "forbidden",
    );
    assert_eq!(value(exchange(&mut control, &mut other, "core.status", json!({"extra":1})))["phase"], "gui");
    error(exchange(&mut control, &mut other, "core.operation_status", json!({"extra":true})), "invalid_params");
    error(exchange(&mut control, &mut other, "core.operation_status", json!({"operation_id":1,"extra":true})), "operation_unknown");
    error(
        exchange(&mut control, &mut other, "core.execute", json!({})),
        "unknown_command",
    );
    error(
        exchange(&mut control, &mut other, "io_monitor.report", json!({})),
        "module_unavailable",
    );
    drop(other);
    control.poll("gui");
    control.gui_health().unwrap();
    drop(gui);
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        control.poll("gui");
        if let Err(error) = control.gui_health() {
            assert!(error.to_string().contains("without exit intent"));
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
}

#[test]
fn collector_reports_are_discoverable_but_runtime_switches_are_removed() {
    let (_dir, mut control) = fixture();
    let mut client = Client::connect(control.path()).unwrap();
    let capabilities = value(exchange(
        &mut control,
        &mut client,
        "core.capabilities",
        json!({}),
    ));
    assert!(!capabilities.as_object().unwrap().contains_key("version"));
    for (name, compiled) in [
        ("diag_memory", cfg!(feature = "diag-memory")),
        ("diag_klog", cfg!(feature = "diag-klog")),
        ("diag_display", cfg!(feature = "diag-display")),
        ("diag_input", cfg!(feature = "diag-input")),
    ] {
        let report = format!("{name}.report");
        assert_eq!(
            capabilities["commands"]
                .as_array()
                .unwrap()
                .contains(&json!(report)),
            compiled
        );
        let response = exchange(&mut control, &mut client, &report, json!({"extra":true}));
        if compiled {
            assert_eq!(value(response), Value::Null);
            error(
                exchange(
                    &mut control,
                    &mut client,
                    &format!("{name}.unknown"),
                    json!({}),
                ),
                "unknown_command",
            );
            error(
                exchange(
                    &mut control,
                    &mut client,
                    &format!("{name}.start"),
                    json!({"pid":1}),
                ),
                "unknown_command",
            );
            error(exchange(
                &mut control, &mut client, &format!("{name}.stop"), json!({}),
            ), "unknown_command");
        } else {
            error(response, "module_unavailable");
        }
    }
}
#[test]
fn unregistered_pid_cannot_bootstrap_and_socket_is_removed_on_drop() {
    let (dir, mut control) = fixture();
    let socket = control.path().to_owned();
    control.begin_gui(u32::MAX, GuiState::default(), None);
    let mut client = Client::connect(&socket).unwrap();
    error(
        exchange(&mut control, &mut client, "igui.bootstrap", json!({})),
        "forbidden",
    );
    drop(client);
    drop(control);
    assert!(!socket.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
#[test]
fn core_exit_operation_completes_only_after_waitpid_and_finalization() {
    let (_dir, mut control) = fixture();
    control.begin_gui(std::process::id(), GuiState::default(), None);
    let mut gui = Client::connect(control.path()).unwrap();
    value(exchange(
        &mut control,
        &mut gui,
        "igui.bootstrap",
        json!({}),
    ));
    let mut client = Client::connect(control.path()).unwrap();
    let op = match exchange(&mut control, &mut client, "core.exit", json!({})).outcome {
        Outcome::Accepted { operation_id } => operation_id,
        other => panic!("{other:?}"),
    };
    for _ in 0..4 {
        control.poll("gui");
        gui.poll().unwrap();
    }
    assert!(gui.close_requested());
    assert_eq!(
        value(exchange(
            &mut control,
            &mut client,
            "core.operation_status",
            json!({"operation_id":op})
        ))["status"],
        "pending"
    );
    value(exchange(&mut control, &mut gui, "igui.closed", json!({})));
    assert_eq!(
        value(exchange(
            &mut control,
            &mut client,
            "core.operation_status",
            json!({"operation_id":op})
        ))["status"],
        "pending"
    );
    control
        .end_gui(Command::new("/bin/true").status().unwrap())
        .unwrap();
    control.finish(None, false).unwrap();
    assert_eq!(
        value(exchange(
            &mut control,
            &mut client,
            "core.operation_status",
            json!({"operation_id":op})
        ))["status"],
        "done"
    );
}
#[test]
fn partial_or_oversized_client_does_not_block_another_client() {
    let (_dir, mut control) = fixture();
    let mut slow = UnixStream::connect(control.path()).unwrap();
    slow.write_all(&[0, 0]).unwrap();
    let mut invalid = UnixStream::connect(control.path()).unwrap();
    invalid.write_all(&u32::MAX.to_be_bytes()).unwrap();
    let mut healthy = Client::connect(control.path()).unwrap();
    assert!(value(exchange(
        &mut control,
        &mut healthy,
        "core.capabilities",
        json!({})
    ))["commands"]
        .as_array()
        .unwrap()
        .contains(&json!("core.status")));
}
#[test]
fn duplicate_request_is_not_reexecuted_without_protocol_version_metadata() {
    use imanlib::{
        protocol::{Message, Request, RESPONSE_LIMIT},
        transport::Channel,
    };
    let (_dir, mut control) = fixture();
    let mut peer =
        Channel::new(UnixStream::connect(control.path()).unwrap(), RESPONSE_LIMIT).unwrap();
    for command in ["core.capabilities", "core.exit"] {
        let r = Request::new(1, command, json!({"extra":true}));
        peer.queue(&Message::Request(r), 16384).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            peer.flush().unwrap();
            control.poll("gui");
            if let Some(Message::Response(r)) = peer.receive().unwrap() {
                if command == "core.capabilities" {
                    assert!(!value(r).as_object().unwrap().contains_key("version"));
                } else {
                    error(r, "duplicate_id");
                }
                break;
            }
            assert!(Instant::now() < deadline);
        }
    }
}
#[test]
fn disk_endpoint_parent_is_refused_without_creation() {
    let root = tempfile::tempdir().unwrap();
    if iman::session::runtime_directory(root.path()).is_err() {
        assert!(Control::bind(Path::new("/"), root.path()).is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
#[cfg(feature = "io-monitor")]
#[test]
fn unavailable_observer_reports_error_and_never_fabricates_zero_counts() {
    let (_dir, mut control) = fixture();
    control.enable_monitor(vec!["iman_missing_device_fixture".into()]);
    let mut client = Client::connect(control.path()).unwrap();
    let report = value(exchange(
        &mut control,
        &mut client,
        "io_monitor.report",
        json!({"extra":true}),
    ));
    assert!(report["observation"].is_null());
    assert!(report["observation_error"]
        .as_str()
        .unwrap()
        .contains("device missing"));
    error(exchange(&mut control, &mut client, "io_monitor.stop", json!({})), "unknown_command");
    let reply = exchange(&mut control, &mut client, "io_monitor.export", json!({}));
    assert!(matches!(reply.outcome, Outcome::Error { .. }));
}

#[cfg(feature = "diag-cycle")]
#[test]
fn automatic_export_refuses_replaced_root_and_manual_export_is_removed() {
    // Both identities stay owned by one TempDir, including after an assertion panic.
    let parent = tempfile::tempdir_in("/dev/shm").unwrap();
    let root = parent.path().join("selected");
    std::fs::create_dir(&root).unwrap();
    let mut control = Control::bind(&root, &root).unwrap();
    control.start_diagnostics().unwrap();
    #[cfg(feature = "io-monitor")]
    control.enable_monitor(vec!["iman_missing_device_fixture".into()]);
    let mut client = Client::connect(control.path()).unwrap();
    error(exchange(&mut control, &mut client, "io_monitor.export", json!({})),
        if cfg!(feature = "io-monitor") { "unknown_command" } else { "module_unavailable" });
    let caps = value(exchange(&mut control, &mut client, "core.capabilities", json!({})));
    assert!(!caps["commands"].as_array().unwrap().contains(&json!("io_monitor.export")));
    assert!(caps["commands"].as_array().unwrap().contains(&json!("diag_cycle.status")));

    let original = parent.path().join("original");
    std::fs::rename(&root, &original).unwrap();
    std::fs::create_dir(&root).unwrap();
    assert!(control.export_segment(None, || {}).unwrap());
    let deadline = Instant::now() + Duration::from_secs(3);
    while control.cycle_pending() {
        control.poll("diagnostic_export");
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }

    let status = value(exchange(&mut control, &mut client, "diag_cycle.status", json!({})));
    assert_eq!(status["state"], "failed");
    assert!(status["error"].as_str().unwrap().contains("root identity changed"));
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    assert_eq!(status["failed_snapshot_retained"], true);
    assert!(!control.export_segment(None, || {}).unwrap());
    for (name, compiled) in [
        ("diag_memory", cfg!(feature = "diag-memory")),
        ("diag_klog", cfg!(feature = "diag-klog")),
        ("diag_display", cfg!(feature = "diag-display")),
        ("diag_input", cfg!(feature = "diag-input")),
    ] {
        if compiled {
            let report = value(exchange(&mut control, &mut client, &format!("{name}.report"), json!({})));
            assert_eq!(report["stopped"], false);
        }
    }
    assert!(control.finish(None, false).is_err());
    assert!(!root.join("iman-reports").exists());
}

#[test]
fn gui_control_is_forwarded_and_acknowledged_by_gui_not_the_shell() {
    let (_dir, mut control) = fixture();
    control.begin_gui(std::process::id(), GuiState::default(), None);
    let mut gui = Client::connect(control.path()).unwrap();
    value(exchange(
        &mut control,
        &mut gui,
        "igui.bootstrap",
        json!({}),
    ));
    let mut caller = Client::connect(control.path()).unwrap();
    for command in ["igui.status", "igui.input", "igui.screenshot", "igui.close"] {
        let params = if command == "igui.input" {
            json!({"action":"down"})
        } else {
            json!({})
        };
        let id = caller.send(command, params.clone()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let request = loop {
            control.poll("gui");
            assert!(caller.poll().unwrap().is_none());
            gui.poll().unwrap();
            if let Some(request) = gui.take_request() {
                break request;
            }
            assert!(Instant::now() < deadline);
        };
        assert_eq!(request.command, command);
        assert_eq!(request.params, params);
        gui.reply(Response::done(request.id, json!({"from":"gui"})))
            .unwrap();
        loop {
            control.poll("gui");
            gui.poll().unwrap();
            if let Some(response) = caller.poll().unwrap() {
                assert_eq!(response.id, id);
                assert_eq!(value(response)["from"], "gui");
                break;
            }
            assert!(Instant::now() < deadline);
        }
    }
    value(exchange(&mut control, &mut gui, "igui.closed", json!({})));
    error(
        exchange(
            &mut control,
            &mut caller,
            "igui.input",
            json!({"action":"b"}),
        ),
        "unavailable",
    );
}

#[test]
fn box2_capabilities_allow_games_and_refuse_only_missing_components() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let mut control = Control::bind(root.path(), root.path()).unwrap();
    control.begin_gui(std::process::id(), GuiState::default(), None);
    let mut client = Client::connect(control.path()).unwrap();

    value(exchange(
        &mut control,
        &mut client,
        "igui.bootstrap",
        json!({}),
    ));
    let capabilities = value(exchange(
        &mut control,
        &mut client,
        "core.capabilities",
        json!({}),
    ));

    for command in ["igui.prepare_launch", "igui.handoff"] {
        assert!(capabilities["commands"]
            .as_array()
            .unwrap()
            .contains(&json!(command)));
    }
    error(
        exchange(
            &mut control,
            &mut client,
            "igui.prepare_launch",
            json!({
                "hash":"ab".repeat(32),"platform":"NES","image":"NES/ab/missing.nes"
            }),
        ),
        "launch_refused",
    );
}

#[test]
#[cfg(all(feature = "diag-display", feature = "diag-input"))]
fn owner_reports_require_bootstrapped_connection_and_cannot_spoof_pid_or_repeat() {
    use imanlib::{
        display_metrics::{Backend, Sample},
        input_metrics,
    };
    let (_dir, mut control) = fixture();
    let mut gui = Client::connect(control.path()).unwrap();
    let mut other = Client::connect(control.path()).unwrap();
    control.enable_diagnostics().unwrap();
    control.begin_gui(std::process::id(), GuiState::default(), None);
    let bootstrap = value(exchange(
        &mut control,
        &mut gui,
        "igui.bootstrap",
        json!({}),
    ));
    assert_eq!(bootstrap["diag_display"], true);
    assert_eq!(bootstrap["diag_input"], true);
    let display = serde_json::to_value(Sample {
        backend: Backend::Headless,
        render: None,
        kms: None,
        present: None,
        idle_check: None,
        probe: None,
        cleanup_ok: Some(true),
        operation_ok: Some(true),
    })
    .unwrap();
    let input = serde_json::to_value(input_metrics::Sample::default()).unwrap();
    for (name, payload) in [("diag_display", display), ("diag_input", input)] {
        let command = format!("{name}.submit");
        error(
            exchange(&mut control, &mut other, &command, payload.clone()),
            "forbidden",
        );
        let mut spoof = payload.clone();
        spoof["pid"] = json!(123);
        value(exchange(&mut control, &mut gui, &command, spoof));
        error(
            exchange(&mut control, &mut gui, &command, payload),
            "invalid_state",
        );
        let report = value(exchange(
            &mut control,
            &mut other,
            &format!("{name}.report"),
            json!({}),
        ));
        assert_eq!(report["received"], 1);
        assert_eq!(report["history"][0]["pid"], std::process::id());
    }
    value(exchange(&mut control, &mut gui, "igui.closed", json!({})));
    error(
        exchange(
            &mut control,
            &mut gui,
            "diag_input.submit",
            serde_json::to_value(input_metrics::Sample::default()).unwrap(),
        ),
        "forbidden",
    );
}

#[cfg(feature = "diag-input")]
#[test]
fn owner_input_reports_accept_current_edges_and_preserve_the_wire_shape() {
    use imanlib::input_metrics::{Sample, LOGICAL_EDGE_COUNT};

    let (_dir, mut control) = fixture();
    control.enable_diagnostics().unwrap();
    control.begin_gui(std::process::id(), GuiState::default(), None);
    let mut gui = Client::connect(control.path()).unwrap();
    value(exchange(&mut control, &mut gui, "igui.bootstrap", json!({})));
    let counts: Vec<u64> = (1..=LOGICAL_EDGE_COUNT as u64).collect();
    let mut payload = serde_json::to_value(Sample::default()).unwrap();
    payload["logical_edges"] = json!(counts);

    for invalid_length in [8, 9, 11] {
        let mut invalid = payload.clone();
        invalid["logical_edges"] = json!(vec![0; invalid_length]);
        error(exchange(&mut control, &mut gui, "diag_input.submit", invalid), "invalid_params");
    }
    let acknowledgement = value(exchange(&mut control, &mut gui, "diag_input.submit", payload));
    let report = value(exchange(&mut control, &mut gui, "diag_input.report", json!({})));

    assert_eq!(acknowledgement, json!({"received":true}));
    assert_eq!(report["received"], 1);
    assert_eq!(report["rejected"], 0);
    assert_eq!(report["history"][0]["data"]["logical_edges"], json!(counts));
    assert_eq!(report["history"][0]["data"]["logical_edges"].as_array().unwrap().len(), LOGICAL_EDGE_COUNT);
}

#[test]
fn interactive_input_test_is_selected_only_by_the_manager_build_feature() {
    let (_dir, mut control) = fixture();
    control.begin_gui(std::process::id(), GuiState::default(), None);
    let mut client = Client::connect(control.path()).unwrap();

    let bootstrap = value(exchange(&mut control, &mut client, "igui.bootstrap", json!({})));

    assert_eq!(bootstrap["input_test"], cfg!(feature = "diag-input"));
}

#[test]
fn input_test_handoff_is_private_feature_gated_and_consumed_only_after_reap() {
    use std::os::unix::process::ExitStatusExt;
    let (_dir, mut control) = fixture();
    control.begin_gui(std::process::id(), GuiState::default(), None);
    let mut gui = Client::connect(control.path()).unwrap();
    let mut outsider = Client::connect(control.path()).unwrap();
    value(exchange(&mut control, &mut gui, "igui.bootstrap", json!({})));
    error(exchange(&mut control, &mut outsider, "igui.input_test", json!({})), "forbidden");
    let response = exchange(&mut control, &mut gui, "igui.input_test", json!({"unexpected":1}));
    if cfg!(feature = "diag-input") {
        assert_eq!(value(response), json!({"handoff_received":true,"test_started":false}));
        assert!(control.input_test_requested());
        #[cfg(feature = "diag-input")]
        assert!(!control.take_input_test_request());
        error(exchange(&mut control, &mut gui, "igui.input_test", json!({})), "invalid_state");
        error(exchange(&mut control, &mut gui, "igui.prepare_launch", json!({})), "invalid_state");
        drop(gui);
        control.poll("gui");
        control.gui_health().unwrap();
        assert!(control.end_gui(std::process::ExitStatus::from_raw(0)).unwrap().is_none());
        #[cfg(feature = "diag-input")]
        { assert!(control.take_input_test_request()); assert!(!control.take_input_test_request()); }
    } else {
        error(response, "module_unavailable");
        assert!(!control.input_test_requested());
    }
}

#[cfg(feature = "diag-input")]
#[test]
fn failed_gui_cleanup_cancels_input_test_handoff() {
    use std::os::unix::process::ExitStatusExt;
    for status in [0, 9] {
        let (_dir, mut control) = fixture();
        control.begin_gui(std::process::id(), GuiState::default(), None);
        let mut gui = Client::connect(control.path()).unwrap();
        value(exchange(&mut control, &mut gui, "igui.bootstrap", json!({})));
        value(exchange(&mut control, &mut gui, "igui.input_test", json!({})));
        if status == 0 { control.gui_failed(); }
        else { assert!(control.end_gui(std::process::ExitStatus::from_raw(status)).is_err()); }
        assert!(!control.take_input_test_request());
    }
}

#[cfg(feature = "diag-cycle")]
#[test]
fn receipt_is_unavailable_while_writer_runs_or_after_export_failure() {
    use std::sync::{Arc, Mutex, mpsc};
    for fail in [false, true] {
        let (_dir, mut control) = fixture();
        let (release, wait) = mpsc::sync_channel(0);
        let wait = Mutex::new(wait);
        control.enable_cycle_exports(Arc::new(move |_, _| {
            wait.lock().unwrap().recv().map_err(std::io::Error::other)?;
            if fail { Err(std::io::Error::other("synthetic sync failure")) }
            else { Ok("/synthetic/actual-report.json".into()) }
        }), Duration::ZERO).unwrap();
        assert!(control.export_segment(None, || {}).unwrap());
        assert!(control.synchronized_report_path().is_none());
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while control.cycle_pending() {
            control.poll("diagnostic_export");
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        if fail { assert!(control.synchronized_report_path().is_none()); }
        else { assert_eq!(control.synchronized_report_path(), Some(Path::new("/synthetic/actual-report.json"))); }
    }
}

#[test]
fn interactive_input_report_is_gated_and_contains_only_manager_completion() {
    let (_dir, mut control) = fixture();
    let mut client = Client::connect(control.path()).unwrap();
    let response = exchange(&mut control, &mut client, "diag_input_test.report", json!({}));

    if cfg!(feature = "diag-input") {
        assert!(value(response).is_null());
    } else {
        error(response, "module_unavailable");
    }
    #[cfg(feature = "diag-input")]
    {
        let completed = json!({"format":"IMAN-INPUT-TEST-2","owner":"iman","reason":"completed"});
        control.record_input_test(completed.clone());

        let report = value(exchange(&mut control, &mut client, "diag_input_test.report", json!({})));

        assert_eq!(report, completed);
    }
}

#[cfg(feature = "diag-input")]
#[test]
fn interactive_report_uses_the_existing_guarded_segment_export_and_resets_after_success() {
    use std::{os::unix::process::ExitStatusExt, sync::{Arc, Mutex}};
    let (_dir, mut control) = fixture();
    let reports = Arc::new(Mutex::new(Vec::new()));
    let captured = reports.clone();
    control.enable_cycle_exports(Arc::new(move |_, bundle| {
        bundle.manifest()?;
        let report: Value = serde_json::from_slice(&bundle.files["diag_input_test.json"])?;
        captured.lock().unwrap().push(report);
        Ok("synthetic-report".into())
    }), Duration::ZERO).unwrap();
    control.begin_gui(std::process::id(), GuiState::default(), None);
    let mut gui = Client::connect(control.path()).unwrap();
    value(exchange(&mut control, &mut gui, "igui.bootstrap", json!({})));
    assert!(control.export_segment(None, || {}).is_err()); // No export with an active GUI.
    value(exchange(&mut control, &mut gui, "igui.closed", json!({})));
    control.end_gui(std::process::ExitStatus::from_raw(0)).unwrap();
    let completed = json!({"format":"IMAN-INPUT-TEST-2","owner":"iman","reason":"completed"});
    control.record_input_test(completed.clone());

    assert!(control.export_segment(None, || {}).unwrap());
    let deadline = Instant::now() + Duration::from_secs(3);
    while control.cycle_pending() {
        control.poll("diagnostic_export");
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }

    assert!(control.cycle_error().is_none());
    assert_eq!(reports.lock().unwrap()[0], completed);
    control.next_segment();
    let report = value(exchange(&mut control, &mut gui, "diag_input_test.report", json!({})));
    assert!(report.is_null());
}

#[cfg(feature = "diag-input")]
#[test]
fn manager_candidates_are_frozen_with_the_report_and_failed_export_does_not_reset_them() {
    use std::sync::{Arc, Mutex};
    for fail in [false, true] {
        let (_dir, mut control) = fixture();
        let exports = Arc::new(Mutex::new(Vec::new()));
        let captured = exports.clone();
        control.enable_cycle_exports(Arc::new(move |_, bundle| {
            bundle.manifest()?;
            captured.lock().unwrap().push(bundle.files.clone());
            if fail { Err(std::io::Error::other("synthetic export failure")) }
            else { Ok("synthetic-candidates-report".into()) }
        }), Duration::ZERO).unwrap();
        let mut client = Client::connect(control.path()).unwrap();
        let files = std::collections::BTreeMap::from([
            ("pads-proposal/summary.json".into(), b"{\"applied\":false}\n".to_vec()),
            ("pads-proposal/1234-5678-0123456789abcdef.cfg".into(), b"# synthetic\n".to_vec()),
        ]);
        control.record_input_test_completion(iman::input_test::Completion { report: json!({"owner":"iman"}), files: files.clone() });

        assert!(control.export_segment(None, || {}).unwrap());
        let deadline = Instant::now() + Duration::from_secs(3);
        while control.cycle_pending() {
            control.poll("diagnostic_export");
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        control.next_segment();

        let exported = exports.lock().unwrap();
        for (name, bytes) in &files { assert_eq!(&exported[0][name], bytes); }
        assert_eq!(serde_json::from_slice::<Value>(&exported[0]["diag_input_test.json"]).unwrap()["owner"], "iman");
        let retained = value(exchange(&mut control, &mut client, "diag_input_test.report", json!({})));
        if fail {
            assert_eq!(retained["owner"], "iman");
            assert!(control.cycle_error().is_some());
            assert!(!control.export_segment(None, || {}).unwrap());
        } else { assert!(retained.is_null()); }
    }
}
