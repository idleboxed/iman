// SPDX-License-Identifier: BSD-3-Clause
#![cfg(feature = "diag-klog")]
use iman::diagnostics::klog::{
    capture_with, Monitor, HISTORY_LIMIT, INTERVAL_MS, LIMIT, RECORDS, REPORT_LIMIT, USB_EARLY_RECORDS, USB_RECORDS,
};
use std::io;

#[test]
fn snapshots_are_bounded_unfiltered_escaped_and_keep_the_latest_records() {
    let snapshot = capture_with(|buffer| {
        assert_eq!(buffer.len(), LIMIT);
        let text = format!(
            "unrelated line\n{}",
            (0..20)
                .map(|n| format!("[{}] VOP \x1bWIN0_EMPTY {}\n", n, "x".repeat(1000)))
                .collect::<String>()
        );
        let size = text.len().min(buffer.len());
        buffer[..size].copy_from_slice(&text.as_bytes()[..size]);
        Ok(size)
    })
    .unwrap();

    assert!(snapshot.buffer_full);
    assert_eq!(snapshot.records.len(), RECORDS);
    assert!(snapshot.matching_lines > RECORDS);
    for line in snapshot.records {
        assert!(line.len() <= 240);
        assert!(!line.contains('\x1b'));
    }
}

#[test]
fn inaccessible_kernel_log_is_an_error_not_an_empty_success() {
    let error = capture_with(|_| Err(io::Error::from_raw_os_error(libc::EPERM))).unwrap_err();

    assert_eq!(error.raw_os_error(), Some(libc::EPERM));
    assert!(capture_with(|_| Ok(LIMIT + 1)).is_err());
}

#[test]
fn a_real_empty_snapshot_is_distinct_from_an_error() {
    let snapshot = capture_with(|_| Ok(0)).unwrap();

    assert!(!snapshot.buffer_full);
    assert_eq!(snapshot.bytes_read, 0);
    assert!(snapshot.records.is_empty());
}

#[test]
fn monitor_throttles_recovers_and_does_not_relabel_old_data_as_a_fresh_success() {
    let mut monitor = Monitor::default();
    monitor.poll(0, "gui", || capture_with(|_| Ok(0)));
    monitor.poll(1, "gui", || panic!("early read"));
    monitor.poll(4999, "gui", || panic!("early read"));
    monitor.poll(5000, "game", || {
        Err(io::Error::from_raw_os_error(libc::EPERM))
    });
    assert_eq!(monitor.report().samples, 2);
    assert!(monitor.report().history.back().unwrap().snapshot.is_none());
    assert!(monitor.report().history.back().unwrap().error.is_some());
    monitor.poll(100_000, "game", || capture_with(|_| Ok(0)));
    monitor.poll(100_001, "game", || panic!("catch-up read"));
    assert_eq!(monitor.report().samples, 3);
    assert_eq!(monitor.report().read_errors, 1);
    monitor.stop();
    monitor.poll(200_000, "game", || panic!("stopped read"));
}

#[test]
fn report_is_bounded_with_escaped_records_and_preserves_truncation_and_error_counts() {
    let mut monitor = Monitor::default();
    for n in 0..20 {
        monitor.poll(n * INTERVAL_MS, "gui", || {
            capture_with(|buffer| {
                let text = (0..8)
                    .map(|i| format!("drm {i} {}\n", "\"\\".repeat(130)))
                    .collect::<String>();
                buffer[..text.len()].copy_from_slice(text.as_bytes());
                Ok(text.len())
            })
        });
    }
    assert_eq!(monitor.report().history.len(), HISTORY_LIMIT);
    let text = monitor.report_json().unwrap();
    assert!(text.len() <= REPORT_LIMIT);
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["samples"], 20);
    assert_eq!(value["read_errors"], 0);
    assert!(value["history_dropped"].as_u64().unwrap() >= 16);
    assert_eq!(monitor.report().history.len(), HISTORY_LIMIT);
    assert_eq!(value["history"][0]["snapshot"]["matching_lines"], 8);
}

#[test]
fn invalid_clock_phase_and_reader_bounds_are_explicit_not_empty_successes() {
    let mut m = Monitor::default();
    m.poll(10, "gui", || {
        let mut s = capture_with(|_| Ok(0)).unwrap();
        s.records = vec!["x".repeat(241)];
        Ok(s)
    });
    assert_eq!(m.report().read_errors, 1);
    m.poll(9, "gui", || panic!("regressing clock read"));
    assert!(m.report().stopped && m.report().error.is_some());
    let mut m = Monitor::default();
    m.poll(0, "x\ny", || panic!("invalid phase read"));
    assert!(m.report().stopped && m.report().error.is_some());
}

fn snapshot(text: &str) -> iman::diagnostics::klog::Snapshot {
    capture_with(|buffer| { buffer[..text.len()].copy_from_slice(text.as_bytes()); Ok(text.len()) }).unwrap()
}

#[test]
fn all_subsystems_and_unlabelled_lines_are_preserved_with_optional_kernel_metadata() {
    let s = snapshot("<3>[    1.000042] filesystem error\n<6>[2.3] usb attached\nordinary information\n<4>[not-a-time] memory warning\n");

    assert_eq!(s.matching_lines, 4);
    assert_eq!(s.records_dropped, 0);
    assert_eq!(s.record_info[0].priority, Some(3));
    assert_eq!(s.record_info[0].kernel_us, Some(1_000_042));
    assert_eq!(s.record_info[1].kernel_us, Some(2_300_000));
    assert_eq!(s.record_info[2].priority, None);
    assert_eq!(s.record_info[2].kernel_us, None);
    assert_eq!(s.record_info[3].kernel_us, None);
    assert_eq!(s.records[2], "ordinary information");
}

#[test]
fn overlapping_snapshots_are_marked_without_claiming_new_error_counts_or_continuity() {
    let mut m = Monitor::default();
    m.poll(0, "gui", || Ok(snapshot("<6>[1.0] a\n<3>[2.0] b\n")));
    m.poll(5000, "gui", || Ok(snapshot("<3>[2.0] b\n<5>[3.0] c\n")));
    assert_eq!(m.report().history[0].comparison, "initial_inventory");
    assert_eq!(m.report().history[0].matching_suffix_prefix_records, None);
    assert_eq!(m.report().history[1].matching_suffix_prefix_records, Some(1));
    assert_eq!(m.report().history[1].comparison, "content_overlap_only");
    m.poll(10000, "game", || Err(io::Error::other("synthetic read failure")));
    m.poll(15000, "game", || Ok(snapshot("<5>[3.0] c\n")));
    assert_eq!(m.report().history.back().unwrap().comparison, "after_read_error");
    assert_eq!(m.report().history.back().unwrap().matching_suffix_prefix_records, None);
    assert_eq!(m.report().unseen_records, "unknown");
}

#[test]
fn overlapping_truncated_text_is_not_mistaken_for_identical_raw_records() {
    let mut m = Monitor::default();
    m.poll(0, "gui", || Ok(snapshot(&("x".repeat(500) + "first\n"))));
    m.poll(5000, "gui", || Ok(snapshot(&("x".repeat(500) + "second\n"))));

    let last = m.report().history.back().unwrap();
    assert_eq!(last.matching_suffix_prefix_records, Some(0));
    assert!(last.snapshot.as_ref().unwrap().record_info[0].text_truncated);
    let value: serde_json::Value = serde_json::from_str(&m.report_json().unwrap()).unwrap();
    for sample in value["history"].as_array().unwrap() {
        assert_eq!(sample["snapshot"]["records"].as_array().unwrap().len(), sample["snapshot"]["record_info"].as_array().unwrap().len());
    }
}

#[test]
fn invalid_utf8_records_remain_distinguishable_and_are_marked() {
    let mut monitor = Monitor::default();
    for (ms, byte) in [(0, 0xff), (5000, 0xfe)] {
        monitor.poll(ms, "gui", || capture_with(|buffer| {
            buffer[0] = byte;
            Ok(1)
        }));
    }
    let last = monitor.report().history.back().unwrap();
    assert_eq!(last.matching_suffix_prefix_records, Some(0));
    assert!(last.snapshot.as_ref().unwrap().record_info[0].invalid_utf8);
}


#[test]
fn early_usb_messages_survive_tail_eviction_empty_reads_and_later_read_errors() {
    let boot = "<6>[0.123456] usb 1-1: new high-speed USB device\n<3>[0.5] hub 1-1:1.0: unable to enumerate\n";
    let unrelated = (0..20).map(|n| format!("[2.0] filesystem message {n}\n")).collect::<String>();
    let mut monitor = Monitor::default();
    monitor.poll(7, "gui", || Ok(snapshot(&(boot.to_owned() + &unrelated))));
    for n in 1..=6 {
        monitor.poll(7 + n * INTERVAL_MS, "input_test", || Ok(snapshot(&unrelated)));
    }
    monitor.poll(7 + 7 * INTERVAL_MS, "gui", || Ok(snapshot("")));
    monitor.poll(7 + 8 * INTERVAL_MS, "gui", || Err(io::Error::other("synthetic read failure")));

    let report = monitor.report();
    assert_eq!(report.usb.records.len(), 2);
    assert_eq!(report.usb.records[0].observed_ms, 7);
    assert_eq!(report.usb.records[0].info.kernel_us, Some(123456));
    assert_eq!(report.usb.records[1].info.priority, Some(3));
    assert_eq!(report.usb.last_matching_sample_ms, Some(7));
    assert_eq!(report.usb.last_successful_sample_ms, Some(7 + 7 * INTERVAL_MS));
    assert_eq!(report.usb.matching_line_observations, 2);
    assert!(report.history.iter().filter_map(|s| s.snapshot.as_ref())
        .flat_map(|s| &s.records).all(|s| !s.contains("usb") && !s.contains("hub")));
    let value: serde_json::Value = serde_json::from_str(&monitor.report_json().unwrap()).unwrap();
    assert_eq!(value["version"], 3);
    assert_eq!(value["selection"], "all");
    assert_eq!(value["usb"]["records"].as_array().unwrap().len(), 2);
    assert!(value["history"].as_array().unwrap().last().unwrap()["error"].is_string());
}

#[test]
fn usb_archive_selects_host_and_hid_tokens_without_filtering_the_generic_tail() {
    let lines = ["USB disconnect", "usbcore: registered driver", "hub 1-1:1.0 port disabled",
        "DWC_OTG controller", "dwc2 controller", "ehci-platform controller", "ohci_hcd controller",
        "xhci-hcd controller", "uhci_hcd controller", "HID-generic device", "hidraw0 input", "usbhid driver"];
    let mut monitor = Monitor::default();
    monitor.poll(0, "gui", || Ok(snapshot(&(lines.join("\n") + "\nordinary message\nfilesystem warning\n"))));

    assert_eq!(monitor.report().usb.records.len(), lines.len());
    for (record, expected) in monitor.report().usb.records.iter().zip(lines) {
        assert_eq!(record.text, expected);
    }
    let tail = &monitor.report().history[0].snapshot.as_ref().unwrap().records;
    assert_eq!(tail.last().unwrap(), "filesystem warning");
    assert_eq!(tail.len(), RECORDS);
}

#[test]
fn usb_capture_keeps_first_and_latest_records_and_reports_omissions_on_every_read() {
    let text = (0..80).map(|n| format!("usb message {n}\n")).collect::<String>();
    let mut monitor = Monitor::default();
    for n in 0..3 {
        monitor.poll(n * INTERVAL_MS, "gui", || Ok(snapshot(&text)));
    }

    let usb = &monitor.report().usb;
    assert_eq!(usb.records.len(), USB_RECORDS);
    let expected = (0..USB_EARLY_RECORDS).chain(64..80).map(|n| format!("usb message {n}")).collect::<Vec<_>>();
    assert_eq!(usb.records.iter().map(|r| r.text.clone()).collect::<Vec<_>>(), expected);
    assert_eq!(usb.matching_line_observations, 240);
    assert_eq!(usb.capture_dropped_observations, 3 * (80 - USB_RECORDS) as u64);
    assert_eq!(usb.retention_evictions, 0);
    assert!(usb.records.iter().all(|r| r.observed_ms == 0));
    assert_eq!(usb.last_matching_sample_ms, Some(2 * INTERVAL_MS));
}

#[test]
fn usb_archive_retains_early_records_and_deduplicates_the_previous_successful_capture() {
    let first = (0..32).map(|n| format!("usb message {n}\n")).collect::<String>();
    let second = (32..64).map(|n| format!("usb message {n}\n")).collect::<String>();
    let mut monitor = Monitor::default();
    monitor.poll(0, "gui", || Ok(snapshot(&first)));
    monitor.poll(INTERVAL_MS, "game", || Ok(snapshot(&second)));
    monitor.poll(2 * INTERVAL_MS, "game", || Err(io::Error::other("synthetic read failure")));
    monitor.poll(3 * INTERVAL_MS, "game", || Ok(snapshot(&second)));

    let usb = &monitor.report().usb;
    let expected = (0..USB_EARLY_RECORDS).chain(48..64).map(|n| format!("usb message {n}")).collect::<Vec<_>>();
    assert_eq!(usb.records.iter().map(|r| r.text.clone()).collect::<Vec<_>>(), expected);
    assert_eq!(usb.retention_evictions, 32);
    assert_eq!(usb.capture_dropped_observations, 0);
    assert!(usb.records.iter().take(USB_EARLY_RECORDS).all(|r| r.observed_ms == 0));
    assert!(usb.records.iter().skip(USB_EARLY_RECORDS).all(|r| r.observed_ms == INTERVAL_MS));
    assert_eq!(usb.deduplication, "raw_content_not_event_identity");
}

#[test]
fn usb_deduplication_uses_full_raw_bytes_not_truncated_or_lossy_text() {
    let mut monitor = Monitor::default();
    for (n, byte) in [0xff, 0xfe].into_iter().enumerate() {
        monitor.poll(n as u64 * INTERVAL_MS, "gui", || capture_with(|buffer| {
            let mut bytes = b"usb ".to_vec();
            bytes.extend_from_slice("x".repeat(500).as_bytes());
            bytes.push(byte);
            buffer[..bytes.len()].copy_from_slice(&bytes);
            Ok(bytes.len())
        }));
    }

    let records = &monitor.report().usb.records;
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].text, records[1].text);
    assert!(records.iter().all(|r| r.info.text_truncated && r.info.invalid_utf8));
    assert_eq!(records[1].observed_ms, INTERVAL_MS);
}

#[test]
fn usb_archive_marks_partial_first_line_without_labeling_every_message_partial() {
    let mut monitor = Monitor::default();
    monitor.poll(0, "gui", || capture_with(|buffer| {
        let prefix = b"usb possibly partial\nusb complete\n";
        buffer.fill(b'x');
        buffer[..prefix.len()].copy_from_slice(prefix);
        Ok(LIMIT)
    }));

    let records = &monitor.report().usb.records;
    assert_eq!(records.len(), 2);
    assert!(records[0].first_line_maybe_partial);
    assert!(!records[1].first_line_maybe_partial);
}

#[test]
fn worst_case_usb_text_fits_report_export_and_ipc_without_losing_early_evidence() {
    let mut monitor = Monitor::default();
    for n in 0..8 {
        let text = (0..USB_RECORDS).map(|i| {
            format!("<191>[18446744073709.000000] usb {} {}\n", n * USB_RECORDS + i, "\"\\".repeat(130))
        }).collect::<String>();
        assert!(text.len() < LIMIT);
        monitor.poll(n as u64 * INTERVAL_MS, "input_test", || Ok(snapshot(&text)));
    }
    let before = serde_json::to_value(monitor.report()).unwrap();
    let text = monitor.report_json().unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();

    assert!(text.len() <= REPORT_LIMIT);
    assert_eq!(value["usb"]["records"].as_array().unwrap().len(), USB_RECORDS);
    assert_eq!(value["usb"]["records"][0]["observed_ms"], 0);
    assert_eq!(value["usb"]["records"][USB_RECORDS - 1]["observed_ms"], 7 * INTERVAL_MS);
    assert_eq!(value["usb"], before["usb"]);
    assert!(!text.contains("fingerprint"));
    assert!(value["history_dropped"].as_u64().unwrap() > before["history_dropped"].as_u64().unwrap());
    assert_eq!(serde_json::to_value(monitor.report()).unwrap(), before);

    let mut bundle = iman::diagnostics::export::Bundle::new(serde_json::json!({"synthetic":true}));
    bundle.files.insert("launch.log".into(), b"synthetic\n".to_vec());
    bundle.json("diag_klog.json", &serde_json::json!({"scope":"collector_epoch", "clock":"iman_session", "data":value})).unwrap();
    assert!(bundle.files["diag_klog.json"].len() <= iman::diagnostics::export::FILE_LIMIT);
    assert!(bundle.byte_len().unwrap() <= iman::diagnostics::export::BUNDLE_LIMIT);
    assert!(bundle.files["diag_klog.json"].len() < imanlib::protocol::RESPONSE_LIMIT);

    value.as_object_mut().unwrap().remove("usb");
    assert!(serde_json::to_vec(&value).unwrap().len() <= 4096);
}
