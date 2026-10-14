// SPDX-License-Identifier: BSD-3-Clause
#![cfg(feature = "diag-input")]
use iman::diagnostics::input::{Monitor, Sample, REPORT_LIMIT};
use imanlib::input_metrics::{Channel, CHANNEL_LIMIT, CONTROL_LIMIT};

#[test]
fn owner_input_reports_preserve_lost_queue_evidence_and_unknown_hardware_status() {
    let mut monitor = Monitor::default();
    monitor.begin_gui(3, 1);
    let mut data = Sample::default();
    data.evdev.event(0, 3, 0);
    data.evdev.event(3, 17, -1);
    data.evdev.event(3, 17, 1);
    data.evdev.errors = 2;
    monitor.submit_gui(3, 5, data).unwrap();
    monitor.end_gui();
    monitor.stop();

    let report_item = &monitor.report().history[0].data;
    assert_eq!(report_item.evdev.syn_dropped, 1);
    assert_eq!((report_item.evdev.controls[0].min, report_item.evdev.controls[0].max), (-1, 1));
    assert_eq!(report_item.evdev.errors, 2);
    assert!(!report_item.evdev_connected);
    assert!(report_item.evdev_identity.is_none());
    assert!(monitor.report_json().unwrap().len() <= REPORT_LIMIT);
}

#[test]
fn excessive_or_malformed_input_details_are_rejected_without_losing_the_expected_report() {
    let mut monitor = Monitor::default();
    monitor.begin_gui(3, 1);
    let sample = Sample {
        channels: vec![
            Channel {
                generation: 1,
                report_id: 1,
                ready: false,
                received: false,
                owner: false
            };
            CHANNEL_LIMIT + 1
        ],
        ..Default::default()
    };
    assert!(monitor.submit_gui(3, 2, sample).is_err());
    let mut valid = Sample::default();
    for index in 0..20 {
        valid.evdev.event(3, index, 0);
    }
    assert_eq!(valid.evdev.controls.len(), CONTROL_LIMIT);
    assert_eq!(valid.evdev.controls_omitted, 12);
    monitor.submit_gui(3, 3, valid).unwrap();
    assert_eq!(monitor.report().received, 1);
    assert_eq!(monitor.report().rejected, 1);
}
