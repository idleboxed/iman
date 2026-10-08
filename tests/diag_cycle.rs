// SPDX-License-Identifier: BSD-3-Clause
//! Synthetic immutable bundles and injected writers, no hardware or persistent I/O.
#![cfg(feature = "diag-cycle")]
use iman::{diagnostics::cycle::{Session, Writer, PHASE_HISTORY_LIMIT, SESSION_BYTE_LIMIT, SESSION_EXPORT_LIMIT}, diagnostics::export::Bundle};
use serde_json::json;
use std::{io, sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}}, time::{Duration, Instant}};

fn freeze(session: &mut Session, ms: u64) -> Bundle {
    let mut bundle = session.freeze(ms, None).unwrap();
    bundle.json("observation.json", &json!(null)).unwrap();
    bundle.files.insert("launch.log".into(), b"synthetic segment\n".to_vec());
    bundle
}

fn wait(session: &mut Session, ms: u64) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while session.pending() {
        session.poll(ms);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn consecutive_snapshots_are_independent_and_carry_the_previous_export_receipt() {
    let records = Arc::new(Mutex::new(Vec::new()));
    let output = records.clone();
    let writer: Writer = Arc::new(move |run, bundle| {
        output.lock().unwrap().push((run.to_owned(), bundle.metadata.clone()));
        Ok(run.into())
    });
    let mut session = Session::new("synthetic".into(), writer, 0);
    session.phase(0, "gui");
    session.phase(12, "game");
    let bundle = freeze(&mut session, 20);
    assert!(session.active.is_none());

    session.start(bundle, 20).unwrap();
    session.begin_next(21); // A pending writer must not permit another active segment.
    assert!(session.active.is_none());
    wait(&mut session, 30);
    session.begin_next(30);
    let bundle = freeze(&mut session, 40);
    session.start(bundle, 40).unwrap();
    wait(&mut session, 50);

    let records = records.lock().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].0, "synthetic-cycle-000001");
    assert_eq!(records[1].0, "synthetic-cycle-000002");
    assert_eq!(records[0].1["segment"]["phases_ms"]["gui"], 12);
    assert_eq!(records[0].1["segment"]["phases_ms"]["game"], 8);
    assert_eq!(records[1].1["segment"]["begin_ms"], 30);
    assert_eq!(records[1].1["previous_export"]["status"], "synchronized");
    assert_eq!(session.saved, 2);
    assert!(session.failed_snapshot().is_none());
    assert!(session.active.is_none());
}

#[test]
fn failed_writer_retains_one_snapshot_and_never_retries_or_accumulates_segments() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let writer: Writer = Arc::new(move |_, _| { count.fetch_add(1, Ordering::SeqCst); Err(io::Error::other("synthetic sync failure")) });
    let mut session = Session::new("failure".into(), writer, 0);
    let mut bundle = freeze(&mut session, 5);
    bundle.json("diag_input_test.json", &json!({"owner":"iman"})).unwrap();
    bundle.json("pads-proposal/summary.json", &json!({"applied":false})).unwrap();

    session.start(bundle, 5).unwrap();
    wait(&mut session, 10);
    for i in 0..100 { session.begin_next(20 + i); session.phase(20 + i, "game"); }

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(session.saved, 0);
    assert!(session.active.is_none());
    assert!(session.freeze(200, None).is_none());
    assert_eq!(session.status()["skipped_segments"], 100);
    assert_eq!(session.failed_snapshot().unwrap().files["launch.log"], b"synthetic segment\n");
    assert!(session.failed_snapshot().unwrap().files.contains_key("pads-proposal/summary.json"));
    assert!(session.error.as_deref().unwrap().contains("sync failure"));
}

#[test]
fn phase_history_eviction_preserves_durations_and_a_fixed_vocabulary() {
    let mut session = Session::new("phases".into(), Arc::new(|_, _| unreachable!()), 0);
    for i in 0..200 { session.phase(i, if i % 2 == 0 {"gui"} else {"game"}); }
    let bundle = freeze(&mut session, 200);

    let segment = &bundle.metadata["segment"];
    assert_eq!(segment["boundaries"].as_array().unwrap().len(), PHASE_HISTORY_LIMIT);
    assert_eq!(segment["boundaries_dropped"], 200 - PHASE_HISTORY_LIMIT);
    assert_eq!(segment["phases_ms"]["gui"], 100);
    assert_eq!(segment["phases_ms"]["game"], 100);
}

#[test]
fn budget_and_payload_refusals_happen_before_the_writer() {
    for kind in ["bytes", "count", "payload"] {
        let mut session = Session::new("budget".into(), Arc::new(|_, _| panic!("writer must not run")), 0);
        let mut bundle = freeze(&mut session, 1);
        match kind {
            "bytes" => session.exported_bytes = SESSION_BYTE_LIMIT,
            "count" => session.saved = SESSION_EXPORT_LIMIT,
            _ => { bundle.files.insert("launch.log".into(), vec![0; 65537]); }
        }

        assert!(session.start(bundle, 1).is_err());
        assert!(session.failed_snapshot().is_some());
        assert!(session.error.is_some());
        assert!(!session.pending());
    }
}

#[test]
fn dropping_session_waits_for_an_accepted_write() {
    let (entered, running) = std::sync::mpsc::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let gate = Mutex::new(gate);
    let writer: Writer = Arc::new(move |_, _| {
        entered.send(()).unwrap();
        gate.lock().unwrap().recv().unwrap();
        Ok("synthetic".into())
    });
    let mut session = Session::new("drop".into(), writer, 0);
    let bundle = freeze(&mut session, 1);
    session.start(bundle, 1).unwrap();
    running.recv_timeout(Duration::from_secs(3)).unwrap();
    let (done, finished) = std::sync::mpsc::channel();
    let dropper = std::thread::spawn(move || { drop(session); done.send(()).unwrap(); });

    assert!(finished.recv_timeout(Duration::from_millis(20)).is_err());
    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(3)).unwrap();
    dropper.join().unwrap();
}

#[cfg(feature = "diag-perf")]
#[test]
fn absent_game_cpu_is_explicit_and_failed_launch_identity_is_retained() {
    let mut session = Session::new("launch".into(), Arc::new(|_, _| unreachable!()), 0);
    session.active.as_mut().unwrap().game = Some(json!({"request":{"hash":"synthetic"}}));
    let mut bundle = session.freeze(5, Some("preflight failed")).unwrap();
    bundle.json("observation.json", &json!(null)).unwrap();
    bundle.files.insert("launch.log".into(), Vec::new());

    assert_eq!(bundle.metadata["session_error"], "preflight failed");
    let cpu: serde_json::Value = serde_json::from_slice(&bundle.files["diag_perf.json"]).unwrap();
    assert_eq!(cpu["state"], "unavailable");
    assert!(bundle.manifest().is_ok());
}

#[test]
fn input_journal_cannot_bypass_the_combined_export_budget() {
    let mut session = Session::new("full".into(), Arc::new(|_, _| panic!("over-budget writer must not run")), 0);
    let mut bundle = freeze(&mut session, 1);
    for name in ["launch.log", "diag_memory.json", "diag_display.json", "diag_input.json"] {
        bundle.files.insert(name.into(), vec![b' '; 64 * 1024]);
    }
    bundle.files.insert("diag_input_test.json".into(), vec![b' '; 48 * 1024]);

    let error = session.start(bundle, 2).unwrap_err();

    assert!(error.to_string().contains("size limit"));
    assert!(session.failed_snapshot().unwrap().files.contains_key("diag_input_test.json"));
    assert_eq!(session.saved, 0);
    assert!(!session.pending());
}
