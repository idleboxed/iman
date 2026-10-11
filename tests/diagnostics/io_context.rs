// SPDX-License-Identifier: BSD-3-Clause
//! Synthetic procfs fixtures only, never block devices or host process counters.
#![cfg(feature = "io-monitor")]
use iman::diagnostics::io_context::{parse_mountinfo, read_process_with, Monitor, Source, HISTORY_LIMIT, INTERVAL_MS, REPORT_LIMIT};
use std::{cell::Cell, fs, io, os::unix::fs::MetadataExt, path::Path};

fn stat(pid: u32, start: u64) -> String {
    let mut fields = vec!["0".to_owned(); 20];
    fields[0] = "S".into();
    fields[19] = start.to_string();
    format!("{pid} (name) with spaces) {}", fields.join(" "))
}
fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}
fn fixture() -> (tempfile::TempDir, Source) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("card");
    fs::create_dir(&root).unwrap();
    let proc = dir.path().join("proc");
    let dev = fs::metadata(&root).unwrap().dev();
    write(&proc.join("self/mountinfo"), &format!("1 0 {}:{} / {} rw,noatime - exfat fixture rw\n",
        libc::major(dev), libc::minor(dev), root.display()));
    write(&proc.join("meminfo"), "Dirty: 2 kB\nWriteback: 0 kB\n");
    for name in ["dirty_background_bytes", "dirty_background_ratio", "dirty_bytes", "dirty_ratio",
        "dirty_expire_centisecs", "dirty_writeback_centisecs"] { write(&proc.join("sys/vm").join(name), "10\n"); }
    for pid in [41, 42] {
        write(&proc.join(format!("{pid}/stat")), &stat(pid, 100));
        write(&proc.join(format!("{pid}/io")), "rchar: 1024\nwchar: 2048\nsyscr: 1\nsyscw: 2\nread_bytes: 4096\nwrite_bytes: 0\ncancelled_write_bytes: 0\n");
    }
    let source = Source::new(proc, root);
    (dir, source)
}

#[test]
fn mount_context_selects_the_actual_containing_device_and_decodes_escapes() {
    let text = "1 0 8:1 / /card rw - exfat test rw\n2 1 8:1 / /card/a\\040b ro,noatime - exfat test ro\n3 0 9:1 / /card/a\\040b/nested rw - ext4 other rw\n";
    let m = parse_mountinfo(text, Path::new("/card/a b/game"), libc::makedev(8, 1)).unwrap();
    assert_eq!(m.mount_point, "/card/a b");
    assert_eq!(m.mount_options, "ro,noatime");
    assert_eq!(m.super_options, "ro");
    assert_eq!(m.filesystem, "exfat");
    assert!(parse_mountinfo(text, Path::new("/card-other"), libc::makedev(8, 1)).is_err());
    assert!(parse_mountinfo(text, Path::new("/card"), libc::makedev(99, 1)).is_err());
}

#[test]
fn malformed_oversized_and_ambiguous_mounts_are_unknown_not_guessed() {
    let good = "1 0 8:1 / /card rw - exfat test rw\n";
    for text in ["bad".into(), format!("{good}{good}"), good.replace("/card", "/card\\999"),
        good.replace("rw -", &("x".repeat(1025) + " -")), "x".repeat(1024 * 1024 + 1)] {
        assert!(parse_mountinfo(&text, Path::new("/card"), libc::makedev(8, 1)).is_err());
    }
}

#[test]
fn process_counters_preserve_real_zero_missing_fields_and_identity() {
    let value = read_process_with(41, |name| Ok(if name == "stat" {stat(41, 100)} else {"read_bytes: 0\nnew_field: 999\n".into()})).unwrap();
    assert_eq!(value.pid, 41);
    assert_eq!(value.start_ticks, 100);
    assert_eq!(value.counters["read_bytes"], Some(0));
    assert_eq!(value.counters["write_bytes"], None);
    assert_eq!(value.counters.len(), 7);
    for data in ["", "write_bytes: -1\n", "write_bytes: 1\nwrite_bytes: 2\n", "write_bytes: 1 byte\n"] {
        assert!(read_process_with(41, |name| Ok(if name == "stat" {stat(41, 100)} else {data.into()})).is_err());
    }
    assert!(read_process_with(41, |name| Ok(if name == "stat" {stat(41, 100)} else {"x".repeat(4097)})).is_err());
}

#[test]
fn reused_disappearing_or_wrong_process_is_not_attributed_to_the_old_child() {
    let calls = Cell::new(0);
    let error = read_process_with(41, |name| {
        if name == "io" { return Ok("write_bytes: 1\n".into()); }
        calls.set(calls.get() + 1);
        Ok(stat(41, calls.get()))
    }).unwrap_err();
    assert!(error.to_string().contains("identity changed"));
    assert!(read_process_with(41, |name| Ok(if name == "stat" {stat(42, 100)} else {"write_bytes: 1\n".into()})).is_err());
    assert_eq!(read_process_with(41, |_| Err(io::Error::from_raw_os_error(libc::ENOENT))).unwrap_err().raw_os_error(), Some(libc::ENOENT));
    assert!(read_process_with(0, |_| panic!("invalid PID read")).is_err());
}

#[test]
fn construction_is_passive_and_polling_is_bounded_throttled_and_recovers() {
    let (dir, source) = fixture();
    let mut m = Monitor::new(source);
    assert!(m.report().environment.is_none());
    assert_eq!(m.report().samples, 0);
    m.poll(0, "gui", 41, Some(42));
    assert_eq!(m.report().read_errors, 0);
    assert_eq!(m.report().history[0].memory.value.as_ref().unwrap().dirty_bytes, Some(2048));
    fs::remove_file(dir.path().join("proc/42/io")).unwrap();
    m.poll(1, "game", 41, Some(42));
    assert_eq!(m.report().samples, 1);
    m.poll(INTERVAL_MS, "game", 41, Some(42));
    assert_eq!(m.report().read_errors, 1);
    assert!(m.report().history.back().unwrap().child.as_ref().unwrap().value.is_none());
    write(&dir.path().join("proc/42/io"), "write_bytes: 17\n");
    for n in 2..100 { m.poll(n * INTERVAL_MS, "game", 41, Some(42)); }
    assert_eq!(m.report().samples, 100);
    assert_eq!(m.report().read_errors, 1);
    assert_eq!(m.report().environment.as_ref().unwrap().ms, 0);
    assert_eq!(m.report().history.len(), HISTORY_LIMIT);
    assert_eq!(m.report().history_dropped, 100 - HISTORY_LIMIT as u64);
    assert!(m.report_json().unwrap().len() <= REPORT_LIMIT);
    m.stop(); m.poll(999_999, "game", 41, Some(42));
    assert_eq!(m.report().samples, 100);
}

#[test]
fn missing_optional_sources_do_not_stop_other_observation_or_become_zero() {
    let (dir, source) = fixture();
    fs::remove_file(dir.path().join("proc/sys/vm/dirty_bytes")).unwrap();
    write(&dir.path().join("proc/meminfo"), "MemTotal: 100 kB\n");
    fs::remove_file(dir.path().join("proc/self/mountinfo")).unwrap();
    let mut m = Monitor::new(source);
    m.poll(100, "gui", 41, None);

    let r = m.report();
    assert_eq!(r.read_errors, 2);
    assert!(!r.stopped);
    assert!(r.environment.as_ref().unwrap().mount.value.is_none());
    assert!(r.environment.as_ref().unwrap().writeback["dirty_bytes"].value.is_none());
    assert_eq!(r.history[0].memory.value.as_ref().unwrap().dirty_bytes, None);
    assert!(r.history[0].child.is_none());
    assert!(r.history[0].manager.value.is_some());
}

#[test]
fn invalid_targets_phase_or_clock_stop_only_the_context_collector() {
    for (ms, phase, pid, child) in [(9, "gui", 41, None), (11, "bad\nphase", 41, None),
        (11, "gui", 0, None), (11, "gui", 41, Some(41))] {
        let (_dir, source) = fixture();
        let mut m = Monitor::new(source);
        m.poll(10, "gui", 41, None);
        m.poll(ms, phase, pid, child);
        assert!(m.report().stopped);
        assert!(m.report().error.is_some());
        assert_eq!(m.report().samples, 1);
    }
}
