// SPDX-License-Identifier: BSD-3-Clause
#![cfg(feature = "diag-display")]
use iman::diagnostics::display::{report_json, Clock, Measurements, Monitor, Sample, HISTORY_LIMIT, REPORT_LIMIT};
use imanlib::display_metrics::{Backend, KmsState, Statistics, Summary};
use std::time::Duration;

fn gui() -> Sample {
    Sample {
        backend: Backend::Headless,
        render: Some(Statistics::default()),
        kms: None,
        present: None,
        idle_check: None,
        probe: None,
        cleanup_ok: Some(true),
        operation_ok: Some(true),
    }
}

#[test]
fn requested_gui_reports_have_manager_times_and_reject_duplicates_wrong_pid_and_invalid_data() {
    let mut m = Monitor::default();
    assert!(m.submit_gui(5, 1, gui()).is_err());
    m.begin_gui(5, 10);
    assert!(m.submit_gui(6, 20, gui()).is_err());
    let mut invalid = gui();
    invalid.backend = Backend::Drm;
    assert!(m.submit_gui(5, 20, invalid).is_err());
    assert!(m.submit_gui(5, 9, gui()).is_err());

    m.submit_gui(5, 20, gui()).unwrap();
    assert!(m.submit_gui(5, 21, gui()).is_err());
    m.end_gui();

    let r = m.report();
    assert_eq!(r.received, 1);
    assert_eq!(r.rejected, 5);
    assert_eq!(r.missing, 0);
    assert_eq!((r.history[0].begin_ms, r.history[0].end_ms), (10, 20));
}

#[test]
fn missing_reports_are_explicit_and_stopping_does_not_accept_late_data() {
    let mut m = Monitor::default();
    m.begin_gui(1, 1);
    m.end_gui();
    m.begin_gui(2, 2);
    m.stop();
    m.stop();

    assert_eq!(m.report().missing, 2);
    assert!(m.report().stopped);
    assert!(m.submit_gui(2, 3, gui()).is_err());
    assert!(m.record(2, 2, 3, "gui", gui()).is_err());
}

#[test]
fn history_and_json_are_bounded_without_changing_live_totals() {
    let mut m = Monitor::default();
    for i in 1..=24 {
        let mut s = gui();
        s.render.as_mut().unwrap().render_us_sum = u64::MAX.into();
        m.record(1, i - 1, i, "gui", s).unwrap();
    }
    assert_eq!(m.report().history.len(), HISTORY_LIMIT);
    assert_eq!(m.report().history_dropped, 16);
    let text = m.report_json().unwrap();
    assert!(text.len() <= REPORT_LIMIT);
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["received"], 24);
    assert_eq!(json["clock"], "iman_session");
    assert_eq!(m.report().history.len(), HISTORY_LIMIT);
    assert!(m.record(1, 0, 1, "gui", gui()).is_err());
    assert!(m.record(1, 24, 25, "invalid phase", gui()).is_err());
}

#[test]
fn active_probe_measurements_have_an_explicit_separate_clock_and_partial_status() {
    let mut m = Monitor::with_clock(Clock::ProbeSession);
    let s = Sample {
        backend: Backend::KmsProbe,
        probe: Some(Summary::default()),
        render: None,
        operation_ok: Some(false),
        cleanup_ok: None,
        ..gui()
    };
    m.record(7, 10, 30, "copy", s).unwrap();
    m.stop();

    let json: serde_json::Value = serde_json::from_str(&m.report_json().unwrap()).unwrap();
    assert_eq!(json["clock"], "probe_session");
    assert_eq!(json["history"][0]["data"]["operation_ok"], false);
    assert!(json["history"][0]["data"]["cleanup_ok"].is_null());
}

fn kms() -> KmsState {
    KmsState {
        connector: 1, encoder: 2, crtc: 3, width: 1280, height: 720,
        refresh_hz: 60, completed_flips: 5, master_acquired: false,
    }
}

#[test]
fn manager_measurements_reuse_common_statistics_without_inventing_idle_or_probe_data() {
    let mut measurements = Measurements::default();
    measurements.render(Duration::from_micros(7), false);
    measurements.render(Duration::from_micros(3), true);
    measurements.present(Duration::from_micros(11), true);
    measurements.present(Duration::from_micros(13), false);

    assert_eq!(measurements.status(Some(&kms())),
        "Display (iman): 2 renders, 2 presents, 5 completed flips; failed");
    let (_, sample) = measurements.finish(kms(), None);

    assert!(sample.validate());
    let render = sample.render.unwrap();
    assert_eq!((render.requests, render.full, render.telemetry), (2, 1, 1));
    assert_eq!((render.render_us_sum, render.render_us_max), (10, 7));
    assert_eq!((render.full_cost.count, render.full_cost.sum_us), (1, 7));
    assert_eq!((render.telemetry_only_cost.count, render.telemetry_only_cost.sum_us), (1, 3));
    let present = sample.present.unwrap();
    assert_eq!((present.count, present.sum_us, present.max_us), (2, 24, 13));
    assert_eq!(sample.operation_ok, Some(false));
    assert_eq!(sample.cleanup_ok, None);
    assert!(sample.idle_check.is_none() && sample.probe.is_none());
    assert_eq!(sample.kms.unwrap().completed_flips, 5);
}

#[test]
fn desktop_owner_reports_real_window_cost_without_fabricated_kms_state() {
    let mut measurements = Measurements::default();
    measurements.render(Duration::from_micros(3), false);
    measurements.present(Duration::from_micros(7), true);
    assert!(measurements.status(None).contains("KMS unavailable (desktop)"));

    let (_, sample) = measurements.finish_desktop(Some(true));

    assert!(sample.validate());
    assert_eq!(sample.backend, Backend::Desktop);
    assert!(sample.kms.is_none());
    assert_eq!(sample.render.unwrap().render_us_sum, 3);
    assert_eq!(sample.present.unwrap().sum_us, 7);
    assert_eq!(sample.cleanup_ok, Some(true));
}

#[test]
fn ownership_failure_and_cleanup_result_are_separate_from_present_timings() {
    let mut measurements = Measurements::default();
    measurements.failed();

    let (_, sample) = measurements.finish(kms(), Some(false));

    assert_eq!(sample.present.unwrap().count, 0);
    assert_eq!(sample.operation_ok, Some(false));
    assert_eq!(sample.cleanup_ok, Some(false));
}

#[test]
fn sourced_histories_share_the_json_budget_but_never_merge_counters_or_mutate_live_reports() {
    let mut child = Monitor::default();
    let mut manager = Monitor::default();
    for i in 1..=16 {
        child.record(42, i - 1, i, "gui", gui()).unwrap();
        let mut m = Measurements::default();
        m.render(Duration::from_micros(123), false);
        manager.record(7, i - 1, i, "gui_diagnostics", m.finish(kms(), None).1).unwrap();
    }

    let text = report_json(&child, &manager).unwrap();
    let report: serde_json::Value = serde_json::from_str(&text).unwrap();

    assert!(text.len() <= REPORT_LIMIT);
    assert_eq!(report["source"], "igui");
    assert_eq!(report["manager"]["source"], "iman");
    assert_eq!(report["received"], 16);
    assert_eq!(report["manager"]["received"], 16);
    for (source, pid) in [(&report, 42), (&report["manager"], 7)] {
        assert!(source["history"].as_array().unwrap().iter().all(|entry| entry["pid"] == pid));
        assert_eq!(source["clock"], "iman_session");
        assert_eq!(source["history"].as_array().unwrap().len() as u64
            + source["history_dropped"].as_u64().unwrap(), 16);
    }
    assert_eq!(child.report().history.len(), HISTORY_LIMIT);
    assert_eq!(manager.report().history.len(), HISTORY_LIMIT);
    assert_eq!(child.report().history_dropped, 8);
    assert_eq!(manager.report().history_dropped, 8);
}

#[test]
fn manager_reports_do_not_consume_expected_gui_submission_or_bypass_stop() {
    let mut collectors = iman::diagnostics::collectors::Collectors::default();
    let sample = || Measurements::default().finish(kms(), None).1;
    assert!(collectors.submit_manager_display("initializing", Duration::ZERO, sample()).is_err());
    collectors.enable_defaults();
    collectors.begin_gui(42);

    collectors.submit_manager_display("starting_gui", Duration::ZERO, sample()).unwrap();
    collectors.submit_display(42, gui()).unwrap();
    let report = collectors.command("diag_display.report").unwrap();

    assert_eq!(report["received"], 1);
    assert_eq!(report["missing"], 0);
    assert_eq!(report["manager"]["received"], 1);
    assert_eq!(report["manager"]["history"][0]["pid"], std::process::id());
    assert_eq!(report["manager"]["history"][0]["phase"], "starting_gui");
    collectors.stop_and_emit();
    assert!(collectors.submit_manager_display("gui_error", Duration::ZERO, sample()).is_err());
    assert!(collectors.command("diag_display.start").is_err());
}

#[test]
fn session_collects_own_phases_and_cleanup_but_disables_measurements_while_lent() {
    use iman::{session::control::Control, display::Lease, session::{self, GameEnvironment}};
    use imanlib::{client::Client, protocol::Outcome};
    use std::{cell::Cell, io, rc::Rc, time::Instant};

    struct Screen {
        enabled: Rc<Cell<bool>>,
        measurements: Option<Measurements>,
        cleanup: Option<bool>,
        fail_cleanup: bool,
    }
    impl Lease for Screen {
        fn observe_display(&mut self, enabled: bool) {
            self.enabled.set(enabled);
            if enabled {
                self.measurements.get_or_insert_with(Default::default);
            } else {
                self.measurements = None;
            }
        }
        fn take_diagnostics(&mut self) -> Option<(Duration, Sample)> {
            Some(std::mem::take(self.measurements.as_mut()?).finish(kms(), self.cleanup))
        }
        fn starting_gui(&mut self) -> io::Result<()> {
            if let Some(m) = &mut self.measurements {
                m.render(Duration::from_micros(2), false);
                m.present(Duration::from_micros(3), true);
            }
            Ok(())
        }
        fn lend(&mut self) -> io::Result<()> {
            assert!(!self.enabled.get());
            Ok(())
        }
        fn reclaim(&mut self) -> io::Result<()> { Ok(()) }
        fn finish(&mut self) -> io::Result<()> {
            self.cleanup = Some(!self.fail_cleanup);
            if self.fail_cleanup { Err(io::Error::other("synthetic cleanup failure")) } else { Ok(()) }
        }
    }

    for enabled in [false, true] {
        for fail_cleanup in [false, true] {
            let root = tempfile::tempdir_in("/dev/shm").unwrap();
            let mut control = Control::bind(root.path(), root.path()).unwrap();
            let mut client = Client::connect(control.path()).unwrap();
            let exchange = |control: &mut Control, client: &mut Client, command| {
                client.send(command, serde_json::json!({})).unwrap();
                let began = Instant::now();
                loop {
                    control.poll("session_start");
                    if let Some(response) = client.poll().unwrap() {
                        let Outcome::Done { result } = response.outcome else { panic!("RPC failed") };
                        break result;
                    }
                    assert!(began.elapsed() < Duration::from_secs(2));
                    std::thread::sleep(Duration::from_millis(1));
                }
            };
            if enabled {
                control.enable_diagnostics().unwrap();
            }
            let active = Rc::new(Cell::new(enabled));
            let mut screen = Screen {
                enabled: active.clone(),
                measurements: enabled.then(Measurements::default),
                cleanup: None,
                fail_cleanup,
            };

            let result = session::run_session_with_display(
                root.path(), GameEnvironment::Inherited, &mut control,
                &mut |_| {}, &mut screen, |_, _, _, _| {
                    assert!(!active.get(), "manager must not measure a child's frames");
                    Ok(None)
                },
            );
            let report = exchange(&mut control, &mut client, "diag_display.report");

            assert_eq!(result.is_err(), fail_cleanup);
            if !enabled {
                assert!(report.is_null());
                continue;
            }
            assert_eq!(report["received"], 0);
            assert_eq!(report["manager"]["source"], "iman");
            assert_eq!(report["manager"]["received"], 3);
            let history = report["manager"]["history"].as_array().unwrap();
            let phases: Vec<_> = history.iter().map(|entry| entry["phase"].as_str().unwrap()).collect();
            assert_eq!(phases, ["initializing", "starting_gui", "session_end"]);
            assert_eq!(history[1]["data"]["render"]["requests"], 1);
            assert_eq!(history[1]["data"]["present"]["count"], 1);
            assert_eq!(history[2]["data"]["cleanup_ok"], !fail_cleanup);
        }
    }
}
