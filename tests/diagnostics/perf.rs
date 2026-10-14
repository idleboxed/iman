// SPDX-License-Identifier: BSD-3-Clause
//! Synthetic proc/sys data; no device performance assertions or dependency execution.
#![cfg(feature = "diag-perf")]
use iman::diagnostics::perf::{
    parse_stat, Frequency, Monitor, Snapshot, Source, Task, CPU_LIMIT, HISTORY_LIMIT, REPORT_LIMIT,
    THREAD_LIMIT,
};
use std::{fs, io, path::Path};

fn stat(id: u32, name: &str, ticks: u64, start: u64) -> String {
    let mut fields = vec!["0".to_owned(); 40];
    fields[0] = "S".into();
    fields[11] = ticks.to_string();
    fields[12] = "3".into();
    fields[19] = start.to_string();
    format!("{id} ({name}) {}\n", fields.join(" "))
}
fn task(id: u32, ticks: u64) -> Task {
    Task {
        id,
        start_ticks: u64::from(id) * 10,
        name: format!("thread {id}"),
        cpu_ticks: ticks,
    }
}
fn snapshot(ticks: u64) -> Snapshot {
    Snapshot {
        process: task(41, ticks),
        threads: vec![task(41, ticks), task(42, ticks / 2)],
        frequencies: vec![Frequency {
            cpu: 0,
            scaling_khz: Some(1_200_000),
            hardware_khz: None,
        }],
        threads_limited: false,
        cpus_limited: false,
        read_errors: 0,
    }
}
fn write(path: &Path, value: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, value).unwrap();
}

#[test]
fn stat_accepts_parentheses_spaces_newlines_and_rejects_bad_counters() {
    let got = parse_stat(&stat(41, "a) b ( c\nx", 27, 900)).unwrap();
    assert_eq!(got.id, 41);
    assert_eq!(got.cpu_ticks, 30);
    assert_eq!(got.start_ticks, 900);
    assert_eq!(got.name, "a) b ( c?x");
    for sample_item in [
        String::new(),
        stat(0, "x", 1, 2),
        stat(41, "x", u64::MAX, 1),
        stat(41, "x", 1, 2).replacen(" S ", " BAD ", 1),
        "x".repeat(8193),
    ] {
        assert!(parse_stat(&sample_item).is_err());
    }
}

#[test]
fn source_reads_bounded_snapshots_and_marks_unavailable_frequencies_not_zero() {
    let root = tempfile::tempdir().unwrap();
    let proc = root.path().join("proc");
    let cpus = root.path().join("cpus");
    let source = Source::new(proc.clone(), cpus.clone());
    assert!(!proc.exists()); // constructor does not acquire anything
    write(&proc.join("41/stat"), &stat(41, "process", 10, 100));
    write(&proc.join("41/task/41/stat"), &stat(41, "main", 7, 100));
    write(&proc.join("41/task/42/stat"), &stat(42, "video", 3, 101));
    write(&proc.join("41/task/43/stat"), "malformed");
    write(&cpus.join("cpu0/cpufreq/scaling_cur_freq"), "1200000\n");
    write(&cpus.join("cpu0/cpufreq/cpuinfo_cur_freq"), "bad");
    let sample = source.sample(41).unwrap();
    assert_eq!(sample.process.cpu_ticks, 13);
    assert_eq!(sample.threads.len(), 2);
    assert_eq!(sample.threads[0].cpu_ticks, 10);
    assert_eq!(sample.frequencies[0].scaling_khz, Some(1_200_000));
    assert_eq!(sample.frequencies[0].hardware_khz, None);
    assert_eq!(sample.read_errors, 2);
    write(&proc.join("41/stat"), &"a".repeat(8193));
    assert!(source.sample(41).is_err());
    assert!(source.sample(0).is_err());
}

#[test]
fn source_exposes_thread_and_cpu_caps_and_missing_process_is_an_error() {
    let root = tempfile::tempdir().unwrap();
    let proc = root.path().join("proc");
    let cpus = root.path().join("cpus");
    let source = Source::new(proc.clone(), cpus.clone());
    assert!(source.sample(41).is_err());
    write(&proc.join("41/stat"), &stat(41, "p", 1, 1));
    for count in 1..=THREAD_LIMIT + 3 {
        write(
            &proc.join(format!("41/task/{count}/stat")),
            &stat(count as u32, "t", 1, 1),
        );
    }
    for count in 0..CPU_LIMIT + 3 {
        fs::create_dir_all(cpus.join(format!("cpu{count}"))).unwrap();
    }
    let sample = source.sample(41).unwrap();
    assert_eq!(sample.threads.len(), THREAD_LIMIT);
    assert!(sample.threads_limited);
    assert_eq!(sample.frequencies.len(), CPU_LIMIT);
    assert!(sample.cpus_limited);
    assert_eq!(
        fs::read_to_string(proc.join("41/stat")).unwrap(),
        stat(41, "p", 1, 1)
    );
}

#[test]
fn clock_units_and_per_thread_deltas_do_not_sum_the_process_twice() {
    let mut monitor = Monitor::new(41, 100).unwrap();
    monitor.poll(0, || Ok(snapshot(10)));
    monitor.poll(100, || panic!("not due"));
    monitor.poll(2000, || Ok(snapshot(210)));
    let report_item = monitor.report();
    assert_eq!(report_item.samples, 2);
    assert_eq!(report_item.threads[0].cpu_ticks, 200);
    assert_eq!(report_item.threads[0].observed_ms, 2000);
    assert_eq!(report_item.threads[0].max_busy_permille, 1000);
    assert_eq!(report_item.threads[1].max_busy_permille, 500);
    assert_eq!(report_item.history[1].process_ticks, Some(200));
    assert_eq!(report_item.history[1].thread_ticks, vec![Some(200), Some(100)]);
    monitor.poll(12000, || Ok(snapshot(310)));
    monitor.poll(12001, || panic!("no catch-up"));
    assert_eq!(monitor.report().samples, 3);
    let json = monitor.finish_json(true).unwrap();
    assert!(
        serde_json::from_str::<serde_json::Value>(&json).unwrap()["finished"]
            .as_bool()
            .unwrap()
    );
    assert!(Monitor::new(0, 100).is_err());
    assert!(Monitor::new(41, 0).is_err());
}

#[test]
fn disappearing_reused_and_regressing_threads_do_not_fabricate_usage() {
    let mut monitor = Monitor::new(41, 100).unwrap();
    monitor.poll(0, || Ok(snapshot(10)));
    let mut sample_item = snapshot(20);
    sample_item.threads.remove(1);
    monitor.poll(2000, || Ok(sample_item));
    monitor.poll(4000, || Ok(snapshot(30)));
    assert_eq!(monitor.report().history[2].thread_ticks[1], None);
    let mut sample_item = snapshot(40);
    sample_item.threads[1].cpu_ticks = 1;
    monitor.poll(6000, || Ok(sample_item));
    assert_eq!(monitor.report().history[3].thread_ticks[1], None);
    assert_eq!(monitor.report().read_errors, 1);
    let mut sample_item = snapshot(50);
    sample_item.threads[1].start_ticks = 900;
    monitor.poll(8000, || Ok(sample_item));
    assert_eq!(monitor.report().threads.len(), 3);
    assert_eq!(monitor.report().history[4].thread_ticks[2], None);
}

#[test]
fn process_reuse_regression_or_reader_failure_stops_only_the_diagnostic() {
    for kind in 0..3 {
        let mut monitor = Monitor::new(41, 100).unwrap();
        monitor.poll(0, || Ok(snapshot(10)));
        monitor.poll(2000, || {
            if kind == 0 {
                return Err(io::Error::new(io::ErrorKind::NotFound, "process gone"));
            }
            let mut sample_item = snapshot(if kind == 1 { 1 } else { 30 });
            if kind == 2 {
                sample_item.process.start_ticks += 1;
            }
            Ok(sample_item)
        });
        assert!(monitor.report().error.is_some());
        monitor.poll(4000, || panic!("stopped"));
        let report_item: serde_json::Value = serde_json::from_str(&monitor.finish_json(false).unwrap()).unwrap();
        assert_eq!(report_item["finished"], false);
        assert_eq!(report_item["samples"], 1);
    }
}

#[test]
fn history_and_serialized_report_are_bounded_but_aggregates_survive() {
    let mut monitor = Monitor::new(41, 100).unwrap();
    for count in 0..100u64 {
        let mut sample_item = snapshot(1000 * count);
        sample_item.threads = (0..THREAD_LIMIT)
            .map(|index| task(41 + index as u32, 100 * count))
            .collect();
        sample_item.frequencies = (0..CPU_LIMIT)
            .map(|index| Frequency {
                cpu: index as u32,
                scaling_khz: Some(1_234_567),
                hardware_khz: Some(1_234_567),
            })
            .collect();
        monitor.poll(count * 2000, || Ok(sample_item));
    }
    assert_eq!(monitor.report().history.len(), HISTORY_LIMIT);
    assert_eq!(monitor.report().threads[0].cpu_ticks, 9900);
    let json = monitor.finish_json(true).unwrap();
    assert!(json.len() <= REPORT_LIMIT);
    let report_item: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(report_item["samples"], 100);
    assert!(report_item["history_dropped"].as_u64().unwrap() > 76);
    assert_eq!(report_item["threads"][0]["cpu_ticks"], 9900);
}

#[test]
fn malformed_snapshots_and_lifetime_thread_limit_are_explicit() {
    let mut monitor = Monitor::new(41, 100).unwrap();
    let mut sample_item = snapshot(0);
    sample_item.threads.push(task(41, 0));
    monitor.poll(0, || Ok(sample_item));
    assert!(monitor.report().error.is_some());
    let mut monitor = Monitor::new(41, 100).unwrap();
    for count in 0..20u64 {
        let mut sample_item = snapshot(count);
        sample_item.threads[1].start_ticks = count;
        monitor.poll(count * 2000, || Ok(sample_item));
    }
    assert_eq!(monitor.report().threads.len(), THREAD_LIMIT);
    assert!(monitor.report().threads_limited);
}

#[test]
fn late_worker_columns_are_null_in_older_samples_and_names_can_change() {
    let mut monitor = Monitor::new(41, 100).unwrap();
    let mut first = snapshot(0);
    first.threads.truncate(1);
    monitor.poll(0, || Ok(first));
    let mut second = snapshot(100);
    second.threads[0].name = "renamed".into();
    monitor.poll(2000, || Ok(second));
    assert_eq!(monitor.report().threads[0].name, "renamed");
    assert_eq!(monitor.report().history[0].thread_ticks, vec![None, None]);
    assert_eq!(monitor.report().history[1].thread_ticks, vec![Some(100), None]);
}
