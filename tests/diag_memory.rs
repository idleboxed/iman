// SPDX-License-Identifier: BSD-3-Clause
//! Synthetic procfs only; RSS here is fixture data, not a hardware measurement.
#![cfg(feature = "diag-memory")]
use iman::diagnostics::memory::{
    parse_status, read_process_with, Monitor, Source, HISTORY_LIMIT, INTERVAL_MS, REPORT_LIMIT,
    STATUS_LIMIT,
};
use std::{cell::Cell, fs, io, path::Path};

fn stat(pid: u32, start: u64) -> String {
    let mut fields = vec!["0".to_owned(); 20];
    fields[0] = "S".into();
    fields[19] = start.to_string();
    format!("{pid} (a) b\nx) {}", fields.join(" "))
}
fn status(pid: u32, rss: u64) -> String {
    format!("Name: app\nPid: {pid}\nVmRSS: {rss} kB\nVmSize: 8192 kB\n")
}
fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}
fn fixture() -> (tempfile::TempDir, Source) {
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir.path().join("meminfo"),
        "MemTotal: 4096 kB\nMemAvailable: 2048 kB\n",
    );
    for pid in [41, 42] {
        write(&dir.path().join(format!("{pid}/stat")), &stat(pid, 100));
        write(
            &dir.path().join(format!("{pid}/status")),
            &status(pid, u64::from(pid)),
        );
    }
    let source = Source::new(dir.path().into());
    (dir, source)
}

#[test]
fn status_keeps_real_zero_and_rejects_unknown_duplicate_invalid_units_and_overflow() {
    assert_eq!(parse_status(&status(41, 0), 41).unwrap(), (0, 8192 * 1024));
    for s in [
        String::new(),
        status(42, 1),
        "Pid: 41\nVmSize: 1 kB\n".into(),
        status(41, 1) + "VmRSS: 2 kB\n",
        status(41, 1) + "Pid: 41\n",
        status(41, 1).replace("kB", "KB"),
        status(41, 1).replace("1 kB", "-1 kB"),
        status(41, u64::MAX),
        status(41, 1).replace("kB", "kB extra"),
        "x".repeat(STATUS_LIMIT + 1),
    ] {
        assert!(parse_status(&s, 41).is_err(), "{s}");
    }
    assert!(parse_status(&status(0, 0), 0).is_err());
}

#[test]
fn reader_checks_identity_around_status_and_propagates_disappearance() {
    let mut reads = 0;
    let error = read_process_with(41, |name| {
        reads += 1;
        Ok(if name == "status" {
            status(41, 4)
        } else {
            stat(41, if reads == 1 { 100 } else { 200 })
        })
    })
    .unwrap_err();
    assert!(error.to_string().contains("identity changed"));
    assert_eq!(reads, 3);
    assert!(read_process_with(0, |_| panic!("invalid pid must not read")).is_err());
    assert!(read_process_with(41, |_| Ok(stat(42, 100))).is_err());
    assert_eq!(
        read_process_with(41, |_| Err(io::Error::from(io::ErrorKind::NotFound)))
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
}

#[test]
fn source_is_explicit_read_only_bounded_and_keeps_independent_failures_unknown() {
    let missing = tempfile::tempdir().unwrap();
    let root = missing.path().join("never-created");
    let source = Source::new(root.clone());
    assert!(!root.exists());
    assert!(source.sample(0, None).is_err());
    assert!(source.sample(41, Some(41)).is_err());
    assert!(!root.exists());

    let (dir, source) = fixture();
    let before = fs::read(dir.path().join("41/status")).unwrap();
    let sample = source.sample(41, Some(42)).unwrap();
    assert_eq!(sample.system.value.unwrap().available_bytes, 2048 * 1024);
    assert_eq!(sample.manager.reading.value.unwrap().rss_bytes, 41 * 1024);
    assert_eq!(
        sample.child.unwrap().reading.value.unwrap().rss_bytes,
        42 * 1024
    );
    assert_eq!(fs::read(dir.path().join("41/status")).unwrap(), before);

    fs::remove_file(dir.path().join("meminfo")).unwrap();
    write(&dir.path().join("42/status"), &"x".repeat(STATUS_LIMIT + 1));
    let sample = source.sample(41, Some(42)).unwrap();
    assert!(sample.system.value.is_none() && sample.system.error.is_some());
    assert!(sample.manager.reading.value.is_some());
    assert!(sample.child.unwrap().reading.error.is_some());
    write(&dir.path().join("41/status"), "\u{fffd}");
    assert!(source
        .sample(41, None)
        .unwrap()
        .manager
        .reading
        .error
        .is_some());
}

#[test]
fn monitor_has_no_catchup_burst_and_recovers_without_reusing_stale_samples() {
    let (_dir, source) = fixture();
    let mut m = Monitor::default();
    let reads = Cell::new(0);
    for (ms, expected) in [
        (0, 1),
        (1, 1),
        (1999, 1),
        (2000, 2),
        (20_000, 3),
        (20_001, 3),
    ] {
        m.poll(ms, "gui", || {
            reads.set(reads.get() + 1);
            source.sample(41, None)
        });
        assert_eq!(reads.get(), expected);
    }
    m.poll(22_000, "game", || Err(io::Error::other("lost procfs")));
    let sample = m.report().history.back().unwrap();
    assert!(sample.reading.value.is_none());
    assert_eq!(sample.reading.error.as_deref(), Some("lost procfs"));
    m.poll(24_000, "game", || source.sample(41, Some(42)));
    assert!(m.report().history.back().unwrap().reading.value.is_some());
    assert_eq!(m.report().read_errors, 1);
    m.stop();
    m.poll(26_000, "game", || panic!("stopped monitor reads"));
    assert!(m.report().stopped);
}

#[test]
fn child_pid_reuse_stays_separate_in_history_and_peaks_survive_trimming() {
    let (dir, source) = fixture();
    let mut m = Monitor::default();
    m.poll(0, "gui", || source.sample(41, Some(42)));
    write(&dir.path().join("42/stat"), &stat(42, 200));
    write(&dir.path().join("42/status"), &status(42, 500));
    m.poll(INTERVAL_MS, "game", || source.sample(41, Some(42)));
    let starts: Vec<_> = m
        .report()
        .history
        .iter()
        .map(|s| {
            s.reading
                .value
                .as_ref()
                .unwrap()
                .child
                .as_ref()
                .unwrap()
                .reading
                .value
                .as_ref()
                .unwrap()
                .start_ticks
        })
        .collect();
    assert_eq!(starts, [100, 200]);
    for n in 2..80 {
        m.poll(n * INTERVAL_MS, "game", || {
            Err(io::Error::other("\"\n".repeat(5000)))
        });
    }
    assert_eq!(m.report().history.len(), HISTORY_LIMIT);
    assert_eq!(m.report().child_rss_max_bytes, Some(500 * 1024));
    let text = m.report_json().unwrap();
    assert!(text.len() <= REPORT_LIMIT);
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["samples"], 80);
    assert_eq!(v["available_min_bytes"], 2048 * 1024);
    assert!(v["history_dropped"].as_u64().unwrap() >= 64);
    assert_eq!(m.report().history.len(), HISTORY_LIMIT);
}

#[test]
fn invalid_phase_or_regressing_clock_stops_only_the_collector_without_reading() {
    for (ms, phase) in [(9, "gui"), (10, "bad\nphase"), (10, "")] {
        let mut m = Monitor::default();
        m.poll(10, "gui", || Err(io::Error::other("not available")));
        m.poll(ms, phase, || panic!("invalid observation must not read"));
        assert!(m.report().stopped);
        assert!(m.report().error.is_some());
    }
}

#[test]
fn serialized_history_is_trimmed_without_losing_peaks_or_live_history() {
    use iman::diagnostics::memory::{ProcessSample, Snapshot};
    let (_dir, source) = fixture();
    let mut m = Monitor::default();
    m.poll(0, "gui", || source.sample(41, Some(42)));
    for n in 1..=16 {
        m.poll(n * INTERVAL_MS, "game", || {
            let error = || io::Error::other("Я".repeat(120));
            Ok(Snapshot {
                system: Err(error()).into(),
                manager: ProcessSample {
                    pid: 41,
                    reading: Err(error()).into(),
                },
                child: Some(ProcessSample {
                    pid: 42,
                    reading: Err(error()).into(),
                }),
            })
        });
    }
    let text = m.report_json().unwrap();
    assert!(text.len() <= REPORT_LIMIT);
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["samples"], 17);
    assert_eq!(v["read_errors"], 48);
    assert!(v["history_dropped"].as_u64().unwrap() > 1);
    assert_eq!(v["manager_rss_max_bytes"], 41 * 1024);
    assert_eq!(m.report().history.len(), HISTORY_LIMIT);
}
