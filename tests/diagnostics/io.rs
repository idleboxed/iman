// SPDX-License-Identifier: BSD-3-Clause
//! Synthetic diskstats snapshots, never physical card measurements.
#![cfg(feature = "io-monitor")]
use iman::diagnostics::io::{self as diag_io, RamReport, Recorder, HISTORY_LIMIT, REPORT_LIMIT};
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::Path,
};

fn row(name: &str, writes: u64, sectors: u64, ms: u64) -> String {
    format!("179 0 {name} 10 0 80 4 {writes} 0 {sectors} {ms} 0 30 40\n")
}

fn new_recorder() -> Recorder {
    Recorder::new(vec!["mmcblk0".into()]).unwrap()
}

fn sysfs_fixture(partition: u32) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let disk = root.path().join("devices/platform/mmc/block/mmcblk0");
    let part = disk.join(format!("mmcblk0p{partition}"));
    let block = root.path().join("dev/block");
    fs::create_dir_all(&part).unwrap();
    fs::create_dir_all(&block).unwrap();
    fs::write(disk.join("dev"), "179:0\n").unwrap();
    fs::write(part.join("dev"), format!("179:{partition}\n")).unwrap();
    fs::write(part.join("partition"), format!("{partition}\n")).unwrap();
    symlink(&disk, block.join("179:0")).unwrap();
    symlink(&part, block.join(format!("179:{partition}"))).unwrap();
    root
}

#[test]
fn tmpfs_application_root_never_selects_an_unrelated_block_device() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    assert!(diag_io::filesystem_devices(root.path()).is_err());
}

#[test]
fn filesystem_discovery_refuses_a_symlink_root() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    symlink("/dev/shm", root.path().join("link")).unwrap();
    assert!(diag_io::filesystem_devices(&root.path().join("link")).is_err());
}

#[test]
fn mounted_device_resolves_its_actual_partition_and_parent_without_requiring_p5() {
    for partition in [1, 5, 12] {
        let sysfs = sysfs_fixture(partition);

        let names = diag_io::partition_devices(sysfs.path(), libc::makedev(179, partition)).unwrap();

        assert_eq!(names, ["mmcblk0".to_owned(), format!("mmcblk0p{partition}")]);
        if partition != 5 {
            assert!(!sysfs.path().join("dev/block/179:5").exists());
        }
    }
}

#[test]
fn mounted_device_does_not_fall_back_to_another_partition_or_whole_disk() {
    let sysfs = sysfs_fixture(1);

    for device in [libc::makedev(179, 5), libc::makedev(179, 0), libc::makedev(8, 1)] {
        assert!(diag_io::partition_devices(sysfs.path(), device).is_err());
    }
}

#[test]
fn mounted_device_refuses_mismatched_or_malformed_sysfs_attributes() {
    for (attribute, value) in [
        ("mmcblk0p1/dev", "179:5\n"),
        ("mmcblk0p1/dev", "179:1"),
        ("mmcblk0p1/partition", "0\n"),
        ("mmcblk0p1/partition", "invalid\n"),
        ("dev", "../179:0\n"),
        ("dev", "179:4294967296\n"),
        ("dev", "179:1\n"),
        ("partition", "1\n"),
    ] {
        let sysfs = sysfs_fixture(1);
        fs::write(sysfs.path().join("devices/platform/mmc/block/mmcblk0").join(attribute), value).unwrap();

        assert!(diag_io::partition_devices(sysfs.path(), libc::makedev(179, 1)).is_err(), "{attribute}: {value}");
    }
    let sysfs = sysfs_fixture(1);
    fs::write(sysfs.path().join("devices/platform/mmc/block/mmcblk0/dev"), "1".repeat(65) + "\n").unwrap();

    assert!(diag_io::partition_devices(sysfs.path(), libc::makedev(179, 1)).unwrap_err()
        .to_string().contains("oversized sysfs attribute"));
}

#[test]
fn mounted_device_refuses_escaped_sysfs_and_attribute_symlinks() {
    let sysfs = sysfs_fixture(1);
    let foreign = sysfs_fixture(1);
    let link = sysfs.path().join("dev/block/179:1");
    fs::remove_file(&link).unwrap();
    symlink(foreign.path().join("devices/platform/mmc/block/mmcblk0/mmcblk0p1"), &link).unwrap();

    assert!(diag_io::partition_devices(sysfs.path(), libc::makedev(179, 1)).unwrap_err()
        .to_string().contains("sysfs identity mismatch"));

    let sysfs = sysfs_fixture(1);
    let attribute = sysfs.path().join("devices/platform/mmc/block/mmcblk0/mmcblk0p1/dev");
    fs::rename(&attribute, attribute.with_extension("original")).unwrap();
    symlink(attribute.with_extension("original"), &attribute).unwrap();

    assert!(diag_io::partition_devices(sysfs.path(), libc::makedev(179, 1)).is_err());
}

#[test]
fn failed_device_discovery_is_reported_without_telemetry_or_session_failure() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let mut control = iman::session::control::Control::bind(root.path(), root.path()).unwrap();
    let before = fs::read_dir(root.path()).unwrap().count();

    control.enable_monitor_with(|| Err(std::io::Error::other("synthetic mounted SD missing")));

    assert_eq!(control.diagnostics_status(), ["I/O: unavailable (synthetic mounted SD missing)"]);
    assert!(control.observation_report().is_none());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), before);
    control.finish(None, true).unwrap();
}

#[test]
fn accepts_old_and_extended_counters_in_requested_device_order() {
    let names = diag_io::device_names("mmcblk0,mmcblk0p5").unwrap();
    let text = format!(
        "{}{}",
        row("mmcblk0p5", 2, 4, 6),
        row("mmcblk0", 3, 5, 7).trim_end().to_owned() + " 0 0 0 0 0 0\n"
    );

    let stats = diag_io::parse_diskstats(&text, &names).unwrap();

    assert_eq!(stats[0].name, "mmcblk0");
    assert_eq!(stats[0].write_ios, 3);
    assert_eq!(stats[0].write_sectors, 5);
    assert_eq!(stats[0].write_ms, 7);
    assert_eq!(stats[1].name, "mmcblk0p5");
}

#[test]
fn rejects_bad_names_missing_duplicate_short_and_invalid_counters() {
    for names in ["", "../sda", "sda,sda", "a,b,c", "a/b", "a b", "a,"] {
        assert!(diag_io::device_names(names).is_err(), "{names}");
    }
    let names = vec!["mmcblk0".into()];
    let good = row("mmcblk0", 10, 20, 30);
    for text in [
        String::new(),
        format!("{good}{good}"),
        "179 0 mmcblk0 0".into(),
        good.replace(" 20 ", " -1 "),
        good.replace(" 30 ", " nonsense "),
        "x".repeat(diag_io::INPUT_LIMIT + 1),
    ] {
        assert!(diag_io::parse_diskstats(&text, &names).is_err());
    }
}

#[test]
fn baseline_is_not_charged_to_session_and_idle_is_zero() {
    let mut recorder = new_recorder();
    recorder
        .sample(0, "session_start", &row("mmcblk0", 100, 1000, 200))
        .unwrap();
    recorder
        .sample(1000, "gui", &row("mmcblk0", 100, 1000, 200))
        .unwrap();

    let report = recorder.report();
    assert_eq!(report.baseline[0].write_sectors, 1000);
    assert_eq!(report.totals[0].bytes, 0);
    assert_eq!(report.history[1].interval_ms, 1000);
    assert_eq!(report.history[1].writes[0].ios, 0);
}

#[test]
fn burst_accounts_512_byte_sectors_and_actual_interval_without_summing_devices() {
    let mut recorder = Recorder::new(vec!["mmcblk0".into(), "mmcblk0p5".into()]).unwrap();
    recorder
        .sample(
            0,
            "session_start",
            &format!("{}{}", row("mmcblk0", 0, 0, 0), row("mmcblk0p5", 0, 0, 0)),
        )
        .unwrap();
    recorder
        .sample(
            1750,
            "gui",
            &format!(
                "{}{}",
                row("mmcblk0", 3, 16, 21),
                row("mmcblk0p5", 2, 8, 12)
            ),
        )
        .unwrap();

    let report = recorder.report();
    assert_eq!(report.totals.len(), 2);
    assert_eq!(report.totals[0].bytes, 8192);
    assert_eq!(report.totals[1].bytes, 4096);
    assert_eq!(report.totals[0].ios, 3);
    assert_eq!(report.totals[0].request_ms, 21);
    assert_eq!(report.history[1].interval_ms, 1750);
}

#[test]
fn reset_wrap_identity_change_overflow_and_time_reversal_do_not_commit() {
    let initial = row("mmcblk0", 5, 10, 20);
    for (time, text) in [
        (2000, row("mmcblk0", 4, 10, 20)),
        (2000, row("mmcblk0", 5, 9, 20)),
        (2000, row("mmcblk0", 5, 10, 19)),
        (2000, initial.replace("179 0", "8 0")),
        (2000, row("mmcblk0", 5, u64::MAX, 20)),
        (999, initial.clone()),
    ] {
        let mut recorder = new_recorder();
        recorder.sample(1000, "gui", &initial).unwrap();

        assert!(recorder.sample(time, "gui", &text).is_err());
        assert_eq!(recorder.report().samples_seen, 1);
        assert_eq!(recorder.report().elapsed_ms, 1000);
        assert_eq!(recorder.report().totals[0].bytes, 0);
    }
}

#[test]
fn history_and_serialized_report_are_bounded_but_totals_cover_whole_run() {
    let mut recorder = new_recorder();
    for n in 0..1000 {
        recorder
            .sample(n * 1000, "gui", &row("mmcblk0", n, n * 8, n * 3))
            .unwrap();
    }
    recorder.stop(None);

    let report = recorder.report();
    assert_eq!(report.history.len(), HISTORY_LIMIT);
    assert_eq!(report.samples_dropped, 1000 - HISTORY_LIMIT as u64);
    assert_eq!(report.totals[0].bytes, 999 * 8 * 512);
    assert_eq!(report.phases["gui"].elapsed_ms, 999000);
    assert_eq!(report.phases["gui"].writes[0].bytes, report.totals[0].bytes);
    assert!(report.finished);
    assert!(serde_json::to_vec(report).unwrap().len() < REPORT_LIMIT);
    assert!(recorder
        .sample(1000000, "gui", &row("mmcblk0", 1000, 8000, 3000))
        .is_err());
}

#[test]
fn diagnostic_failure_preserves_last_valid_counts_and_is_not_success() {
    let mut recorder = new_recorder();
    recorder.sample(0, "gui", &row("mmcblk0", 1, 2, 3)).unwrap();
    let error = recorder.sample(1000, "gui", "").unwrap_err();
    recorder.stop(Some(&error.to_string()));

    assert!(!recorder.report().finished);
    assert!(recorder
        .report()
        .error
        .as_ref()
        .unwrap()
        .contains("device missing"));
    assert_eq!(recorder.report().latest[0].write_ios, 1);
}

#[test]
fn missing_gui_diagnostics_and_error_phases_keep_recording_until_explicit_stop() {
    let mut recorder = new_recorder();
    for (index, phase) in ["session_start", "gui_start", "gui_diagnostics", "gui_error", "session_end"]
        .into_iter().enumerate()
    {
        let n = index as u64;
        recorder.sample(n * 1000, phase, &row("mmcblk0", n, n, n)).unwrap();
    }
    recorder.stop(None);

    let report = recorder.report();
    assert!(report.finished);
    assert!(report.error.is_none());
    assert_eq!(report.samples_seen, 5);
    assert_eq!(report.totals[0].bytes, 4 * 512);
    assert!(report.phases.contains_key("gui_diagnostics"));
    assert!(report.phases.contains_key("gui_error"));
}

#[test]
fn requested_unavailable_io_has_status_instead_of_silently_skipping_diagnostics() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let mut control = iman::session::control::Control::bind(root.path(), root.path()).unwrap();
    assert!(control.diagnostics_status().is_empty());

    // Invalid input is refused before procfs or report writes, without a fixture device.
    control.enable_monitor(Vec::new());

    let status = control.diagnostics_status();
    assert_eq!(status.len(), 1);
    assert!(status[0].starts_with("I/O: unavailable (expected one or two distinct block device names"));
    assert!(control.observation_report().is_none());
}

#[test]
fn refuses_symlink_parent_without_creating_report() {
    let root = tempfile::tempdir().unwrap();
    symlink("/dev/shm", root.path().join("link")).unwrap();

    assert!(RamReport::create(&root.path().join("link")).is_err());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn non_tmpfs_parent_is_refused_before_report_creation() {
    let error = RamReport::create(Path::new("/proc")).err().unwrap();

    assert!(error.to_string().contains("I/O report requires tmpfs"));
}

#[test]
fn tmpfs_report_is_private_retained_bounded_and_rewrites_without_stale_tail() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let mut output = RamReport::create(root.path()).unwrap();
    let path = output.path().to_path_buf();
    let mut recorder = new_recorder();
    for n in 0..150 {
        recorder
            .sample(n * 1000, "gui", &row("mmcblk0", n, n, n))
            .unwrap();
        output.write(recorder.report()).unwrap();
    }
    output.write(new_recorder().report()).unwrap();
    drop(output);

    assert!(Path::new(&path).exists());
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let bytes = fs::read(path).unwrap();
    assert!(bytes.len() < REPORT_LIMIT);
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["samples_seen"], 0);
}

#[test]
fn cycle_and_export_intervals_share_exact_boundaries_and_sum_to_master() {
    let mut master = iman::diagnostics::io::Recorder::new(vec!["mmcblk0".into()]).unwrap();
    let row = |writes: u64| format!("179 0 mmcblk0 0 0 0 0 {writes} 0 {} {} 0 0 0\n", writes * 2, writes * 3);
    master.sample(0, "gui", &row(10)).unwrap();
    let mut cycle = master.fork_interval("gui").unwrap();
    for (ms, phase, writes) in [(10, "game", 12), (20, "post_game", 15), (30, "diagnostic_export", 17)] {
        master.sample(ms, phase, &row(writes)).unwrap();
        cycle.sample(ms, phase, &row(writes)).unwrap();
    }
    let mut export = master.fork_interval("diagnostic_export").unwrap();
    master.sample(40, "starting_gui", &row(20)).unwrap();
    export.sample(40, "starting_gui", &row(20)).unwrap();
    let mut tail = master.fork_interval("starting_gui").unwrap();
    master.sample(50, "session_end", &row(21)).unwrap();
    tail.sample(50, "session_end", &row(21)).unwrap();

    assert_eq!(master.report().totals[0].bytes,
        cycle.report().totals[0].bytes + export.report().totals[0].bytes + tail.report().totals[0].bytes);
    assert_eq!(export.report().baseline_ms, Some(30));
    assert_eq!(master.report().phases["diagnostic_export"].writes[0].bytes, export.report().totals[0].bytes);
    assert!(!master.report().finished);
    master.stop(Some("synthetic counter failure"));
    assert!(master.fork_interval("gui").is_err());
}

#[test]
fn master_and_segment_use_one_poll_and_remain_live_across_immutable_export() {
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let source = calls.clone();
    let mut monitor = iman::diagnostics::io::Monitor::start_using(vec!["mmcblk0".into()], root.path(),
        std::time::Instant::now(), Box::new(move || {
            let n = source.fetch_add(1, Ordering::SeqCst);
            Ok(format!("179 0 mmcblk0 0 0 0 0 {n} 0 {n} {n} 0 0 0\n"))
        })).unwrap();
    monitor.begin_segment("session_start").unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    monitor.boundary("diagnostic_export");
    let frozen = serde_json::to_value(monitor.segment_report()).unwrap();
    monitor.begin_segment("diagnostic_export").unwrap();
    for _ in 0..5 { monitor.boundary("diagnostic_export"); }

    assert_eq!(calls.load(Ordering::SeqCst), 7);
    assert_eq!(frozen["totals"][0]["bytes"], 512);
    assert_eq!(monitor.report().totals[0].bytes, 6 * 512);
    assert_eq!(monitor.segment_report().unwrap().totals[0].bytes, 5 * 512);
    assert!(!monitor.report().finished);
    assert_eq!(monitor.report().samples_seen, 7);

    monitor.finish();
    assert!(monitor.report().finished);
    assert!(monitor.segment_report().unwrap().finished);
    let stopped_calls = calls.load(Ordering::SeqCst);
    monitor.boundary("gui");
    assert_eq!(calls.load(Ordering::SeqCst), stopped_calls);
}

#[test]
fn read_and_write_activity_survives_a_long_idle_tail_without_charging_baselines() {
    let mut recorder = new_recorder();
    let initial = row("mmcblk0", 100, 1000, 200);
    let reads = initial.replace(" 10 0 80 4 ", " 12 0 96 9 ");
    let both = reads.replace(" 100 0 1000 200 ", " 103 0 1008 204 ");
    recorder.sample(0, "gui", &initial).unwrap();
    recorder.sample(1000, "game", &reads).unwrap();
    recorder.sample(2000, "game", &both).unwrap();
    for n in 3..100 { recorder.sample(n * 1000, "game", &both).unwrap(); }

    let r = recorder.report();
    assert_eq!(r.read_totals[0].bytes, 16 * 512);
    assert_eq!(r.read_totals[0].ios, 2);
    assert_eq!(r.read_totals[0].request_ms, 5);
    assert_eq!(r.totals[0].bytes, 8 * 512);
    assert_eq!(r.activity.len(), 2);
    assert_eq!(r.activity[0].elapsed_ms, 1000);
    assert_eq!(r.phases["gui"].reads[0].bytes, 16 * 512);
    assert!(r.history.iter().all(|s| s.reads[0].bytes == 0 && s.writes[0].bytes == 0));
    let mut segment = recorder.fork_interval("post_game").unwrap();
    segment.sample(100_000, "post_game", &both).unwrap();
    assert_eq!(segment.report().read_totals[0].bytes, 0);
    assert!(segment.report().activity.is_empty());
}

#[test]
fn read_counter_regression_and_overflow_do_not_commit_a_partial_sample() {
    let initial = row("mmcblk0", 5, 10, 20);
    for readings in ["9 0 80 4", "10 0 79 4", "10 0 80 3", "10 0 18446744073709551615 4"] {
        let mut recorder = new_recorder();
        recorder.sample(0, "gui", &initial).unwrap();
        assert!(recorder.sample(1, "gui", &initial.replace("10 0 80 4", readings)).is_err());
        assert_eq!(recorder.report().samples_seen, 1);
        assert!(recorder.report().activity.is_empty());
    }
}

#[test]
fn activity_evictions_and_maximum_width_reports_remain_bounded() {
    let names = vec!["mmcblk0".into(), "mmcblk0p1".into()];
    let mut recorder = Recorder::new(names).unwrap();
    let row = |name, n: u64| format!("179 {} {name} {n} 0 {n} {n} {n} 0 {n} {n} 18446744073709551615 0 0\n", if name == "mmcblk0" {0} else {1});
    let base = 10_000_000_000_000_000u64;
    for n in 0..100 {
        let time = 10_000_000_000_000_000_000u64 + n;
        let counts = base + n * 100_000_000_000_000;
        recorder.sample(time, if n % 2 == 0 {"diagnostic_export"} else {"display_reclaim"},
            &(row("mmcblk0", counts) + &row("mmcblk0p1", counts))).unwrap();
    }
    let report = recorder.report();
    assert_eq!(report.activity.len(), 16);
    assert_eq!(report.activity_dropped, 83);
    assert!(serde_json::to_vec(report).unwrap().len() <= REPORT_LIMIT);
}
