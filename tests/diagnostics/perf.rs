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
    for s in [
        String::new(),
        stat(0, "x", 1, 2),
        stat(41, "x", u64::MAX, 1),
        stat(41, "x", 1, 2).replacen(" S ", " BAD ", 1),
        "x".repeat(8193),
    ] {
        assert!(parse_stat(&s).is_err());
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
    for n in 1..=THREAD_LIMIT + 3 {
        write(
            &proc.join(format!("41/task/{n}/stat")),
            &stat(n as u32, "t", 1, 1),
        );
    }
    for n in 0..CPU_LIMIT + 3 {
        fs::create_dir_all(cpus.join(format!("cpu{n}"))).unwrap();
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
    let mut m = Monitor::new(41, 100).unwrap();
    m.poll(0, || Ok(snapshot(10)));
    m.poll(100, || panic!("not due"));
    m.poll(2000, || Ok(snapshot(210)));
    let r = m.report();
    assert_eq!(r.samples, 2);
    assert_eq!(r.threads[0].cpu_ticks, 200);
    assert_eq!(r.threads[0].observed_ms, 2000);
    assert_eq!(r.threads[0].max_busy_permille, 1000);
    assert_eq!(r.threads[1].max_busy_permille, 500);
    assert_eq!(r.history[1].process_ticks, Some(200));
    assert_eq!(r.history[1].thread_ticks, vec![Some(200), Some(100)]);
    m.poll(12000, || Ok(snapshot(310)));
    m.poll(12001, || panic!("no catch-up"));
    assert_eq!(m.report().samples, 3);
    let json = m.finish_json(true).unwrap();
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
    let mut m = Monitor::new(41, 100).unwrap();
    m.poll(0, || Ok(snapshot(10)));
    let mut s = snapshot(20);
    s.threads.remove(1);
    m.poll(2000, || Ok(s));
    m.poll(4000, || Ok(snapshot(30)));
    assert_eq!(m.report().history[2].thread_ticks[1], None);
    let mut s = snapshot(40);
    s.threads[1].cpu_ticks = 1;
    m.poll(6000, || Ok(s));
    assert_eq!(m.report().history[3].thread_ticks[1], None);
    assert_eq!(m.report().read_errors, 1);
    let mut s = snapshot(50);
    s.threads[1].start_ticks = 900;
    m.poll(8000, || Ok(s));
    assert_eq!(m.report().threads.len(), 3);
    assert_eq!(m.report().history[4].thread_ticks[2], None);
}

#[test]
fn process_reuse_regression_or_reader_failure_stops_only_the_diagnostic() {
    for kind in 0..3 {
        let mut m = Monitor::new(41, 100).unwrap();
        m.poll(0, || Ok(snapshot(10)));
        m.poll(2000, || {
            if kind == 0 {
                return Err(io::Error::new(io::ErrorKind::NotFound, "process gone"));
            }
            let mut s = snapshot(if kind == 1 { 1 } else { 30 });
            if kind == 2 {
                s.process.start_ticks += 1;
            }
            Ok(s)
        });
        assert!(m.report().error.is_some());
        m.poll(4000, || panic!("stopped"));
        let r: serde_json::Value = serde_json::from_str(&m.finish_json(false).unwrap()).unwrap();
        assert_eq!(r["finished"], false);
        assert_eq!(r["samples"], 1);
    }
}

#[test]
fn history_and_serialized_report_are_bounded_but_aggregates_survive() {
    let mut m = Monitor::new(41, 100).unwrap();
    for n in 0..100u64 {
        let mut s = snapshot(1000 * n);
        s.threads = (0..THREAD_LIMIT)
            .map(|i| task(41 + i as u32, 100 * n))
            .collect();
        s.frequencies = (0..CPU_LIMIT)
            .map(|i| Frequency {
                cpu: i as u32,
                scaling_khz: Some(1_234_567),
                hardware_khz: Some(1_234_567),
            })
            .collect();
        m.poll(n * 2000, || Ok(s));
    }
    assert_eq!(m.report().history.len(), HISTORY_LIMIT);
    assert_eq!(m.report().threads[0].cpu_ticks, 9900);
    let json = m.finish_json(true).unwrap();
    assert!(json.len() <= REPORT_LIMIT);
    let r: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(r["samples"], 100);
    assert!(r["history_dropped"].as_u64().unwrap() > 76);
    assert_eq!(r["threads"][0]["cpu_ticks"], 9900);
}

#[test]
fn malformed_snapshots_and_lifetime_thread_limit_are_explicit() {
    let mut m = Monitor::new(41, 100).unwrap();
    let mut s = snapshot(0);
    s.threads.push(task(41, 0));
    m.poll(0, || Ok(s));
    assert!(m.report().error.is_some());
    let mut m = Monitor::new(41, 100).unwrap();
    for n in 0..20u64 {
        let mut s = snapshot(n);
        s.threads[1].start_ticks = n;
        m.poll(n * 2000, || Ok(s));
    }
    assert_eq!(m.report().threads.len(), THREAD_LIMIT);
    assert!(m.report().threads_limited);
}

#[test]
fn late_worker_columns_are_null_in_older_samples_and_names_can_change() {
    let mut m = Monitor::new(41, 100).unwrap();
    let mut first = snapshot(0);
    first.threads.truncate(1);
    m.poll(0, || Ok(first));
    let mut second = snapshot(100);
    second.threads[0].name = "renamed".into();
    m.poll(2000, || Ok(second));
    assert_eq!(m.report().threads[0].name, "renamed");
    assert_eq!(m.report().history[0].thread_ticks, vec![None, None]);
    assert_eq!(m.report().history[1].thread_ticks, vec![Some(100), None]);
}
