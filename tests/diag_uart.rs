// SPDX-License-Identifier: BSD-3-Clause
//! Synthetic Linux roots only. No tty, /dev/mem or register access.
#![cfg(feature = "diag-uart")]
use iman::diagnostics::uart::{Monitor, Source, DEPTH_LIMIT, ENTRY_LIMIT, INPUT_LIMIT, RECORD_LIMIT, REPORT_LIMIT, TEXT_BUDGET};
use std::{cell::Cell, fs, io, os::unix::fs::symlink, path::Path};

fn write(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn link(root: &Path, path: &str, target: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    symlink(target, path).unwrap();
}

fn fixture() -> (tempfile::TempDir, Source) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (path, text) in [
        ("proc/version", "Linux synthetic 4.4\n"),
        ("proc/cmdline", "console=ttyS0,115200\n"),
        ("proc/consoles", ""),
        ("proc/tty/drivers", "serial /dev/ttyS 4 64-95 serial\npty_slave /dev/pts 136 0-1 pty:slave\n"),
        ("proc/self/status", "Name:\tsynthetic\nUid:\t0 0 0 0\nCapEff:\t0000000000020000\nNoNewPrivs:\t0\nVmRSS:\t999 kB\n"),
        ("sys/kernel/debug/clk/clk_summary", "MUST NOT READ: hardware callbacks\n"),
        ("sys/kernel/debug/pinctrl/20008000.pinctrl/pins", "MUST NOT READ: hardware callbacks\n"),
        ("sys/kernel/debug/pinctrl/20008000.pinctrl/pinconf-pins", "MUST NOT READ: hardware callbacks\n"),
        ("sys/kernel/debug/pinctrl/20008000.pinctrl/pinmux-pins", "Pinmux settings per pin\npin 15 (gpio0-15): other\npin 16 (gpio0-16): (MUX UNCLAIMED) (GPIO UNCLAIMED)\npin 17 (gpio0-17): other-owner\n"),
        ("proc/tty/driver/serial", "MUST NOT READ: powers up UART\n"),
        ("dev/mem", "MUST NOT OPEN\n"),
    ] { write(root, path, text); }
    for (field, text) in [("clk_rate", "24000000\n"), ("clk_enable_count", "0\n"),
        ("clk_prepare_count", "0\n"), ("clk_parent", "xin24m\n")]
    {
        write(root, &format!("sys/kernel/debug/clk/xin24m/sclk_uart0/{field}"), text);
    }
    link(root, "sys/bus/platform/devices/20060000.serial/driver", "../../drivers/dw-apb-uart");
    link(root, "sys/class/tty/ttyS0/device", "../../../devices/platform/20060000.serial");
    let source = Source::new(root.into());
    (dir, source)
}

#[test]
fn construction_and_disabled_monitor_do_not_touch_sources() {
    let dir = tempfile::tempdir().unwrap();
    let absent = dir.path().join("not-created");
    let _source = Source::new(absent.clone());
    let mut monitor = Monitor::default();
    assert_eq!(monitor.report().samples, 0);
    assert!(!absent.exists());
    monitor.stop();
    monitor.poll(0, "gui_diagnostics", || panic!("stopped collector must not read"));
    assert_eq!(monitor.report().samples, 0);
    assert_eq!(Source::new("relative".into()).capture().unwrap_err().kind(), io::ErrorKind::Other);
}

#[test]
fn snapshot_keeps_software_state_separate_from_hardware_readiness() {
    let (dir, source) = fixture();
    let marker = fs::read(dir.path().join("dev/mem")).unwrap();
    let snapshot = source.capture().unwrap();
    let text = |path: &str| &snapshot.files[path].value.as_ref().unwrap().text;

    assert_eq!(text("/proc/consoles"), "");
    assert!(text("/proc/tty/drivers").contains("serial"));
    assert!(!text("/proc/tty/drivers").contains("pty_slave"));
    assert!(text("/proc/self/status").contains("CapEff:"));
    assert!(!text("/proc/self/status").contains("VmRSS"));
    assert_eq!(text("/sys/kernel/debug/clk/xin24m/sclk_uart0/clk_rate"), "24000000\n");
    assert_eq!(text("/sys/kernel/debug/clk/xin24m/sclk_uart0/clk_enable_count"), "0\n");
    let pins = text("/sys/kernel/debug/pinctrl/20008000.pinctrl/pinmux-pins");
    assert!(pins.contains("pin 16 (") && pins.contains("pin 17 ("));
    assert!(!pins.contains("pin 15 ("));
    assert_eq!(snapshot.scans["platform_serial_devices"].matches, 1);
    assert!(text("/sys/class/tty/ttyS0/device").contains("20060000.serial"));
    let node = snapshot.dev_mem.value.unwrap();
    assert_eq!(node.kind, "regular_file");
    assert_eq!(node.major, None);
    assert!(!node.access_tested);
    assert_eq!(fs::read(dir.path().join("dev/mem")).unwrap(), marker);
    let json = serde_json::to_string(&snapshot.files).unwrap();
    assert!(!json.contains("MUST NOT"));
    assert!(!json.contains("clk_summary") && !json.contains("pinconf-pins"));
    assert!(!snapshot.files.contains_key("/proc/tty/driver/serial"));
}

#[test]
fn absent_sources_and_partial_clock_data_remain_unknown_not_zero() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot = Source::new(dir.path().into()).capture().unwrap();
    assert!(snapshot.dev_mem.value.is_none());
    assert!(snapshot.files["/proc/consoles"].error.is_some());
    assert!(snapshot.scans["uart0_clock_tree"].errors.len() == 1);
    assert!(snapshot.read_errors() >= 6);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);

    let (dir, source) = fixture();
    fs::remove_file(dir.path().join("sys/kernel/debug/clk/xin24m/sclk_uart0/clk_rate")).unwrap();
    let snapshot = source.capture().unwrap();
    let rate = &snapshot.files["/sys/kernel/debug/clk/xin24m/sclk_uart0/clk_rate"];
    assert!(rate.value.is_none() && rate.error.is_some());
    assert!(snapshot.files["/sys/kernel/debug/clk/xin24m/sclk_uart0/clk_parent"].value.is_some());
}

#[test]
fn symlinks_and_fifos_are_not_opened_as_text_and_mem_is_metadata_only() {
    let (dir, source) = fixture();
    fs::remove_file(dir.path().join("proc/version")).unwrap();
    symlink("../dev/mem", dir.path().join("proc/version")).unwrap();
    for name in ["proc/consoles", "dev/mem"] {
        let path = dir.path().join(name);
        fs::remove_file(&path).unwrap();
        let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: fixture-only path, no descriptors or hardware resources involved.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    }
    let snapshot = source.capture().unwrap();
    for name in ["/proc/version", "/proc/consoles"] {
        assert!(snapshot.files[name].error.as_ref().unwrap().contains("not a regular file"));
    }
    assert_eq!(snapshot.dev_mem.value.unwrap().kind, "other");
}

#[test]
fn input_text_retention_and_json_sizes_are_bounded_with_explicit_loss() {
    let (dir, source) = fixture();
    write(dir.path(), "proc/version", &"x".repeat(INPUT_LIMIT + 1));
    write(dir.path(), "proc/cmdline", &"я\u{1}".repeat(2500));
    let snapshot = source.capture().unwrap();
    assert!(snapshot.files["/proc/version"].error.as_ref().unwrap().contains("limit"));
    let text = snapshot.files["/proc/cmdline"].value.as_ref().unwrap();
    assert!(text.truncated);
    assert!(text.text.contains('?') && !text.text.contains('\u{1}'));
    assert!(snapshot.text_bytes <= TEXT_BUDGET);
    let mut monitor = Monitor::default();
    monitor.poll(8, "gui_diagnostics", || Ok(snapshot));
    assert!(monitor.report_json().unwrap().len() <= REPORT_LIMIT);
}

#[test]
fn malformed_utf8_is_reported_without_losing_other_sources() {
    let (dir, source) = fixture();
    fs::write(dir.path().join("proc/cmdline"), [0xff]).unwrap();
    let snapshot = source.capture().unwrap();
    assert!(snapshot.files["/proc/cmdline"].error.is_some());
    assert!(snapshot.files["/proc/version"].value.is_some());
}

#[test]
fn discovery_does_not_follow_directory_symlinks_and_stops_at_depth_limit() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "sclk_uart0/clk_rate", "MUST NOT READ");
    let root = "sys/kernel/debug/clk";
    link(dir.path(), &format!("{root}/external"), outside.path().to_str().unwrap());
    let nested = format!("{root}/{}sclk_uart0", "node/".repeat(DEPTH_LIMIT + 1));
    write(dir.path(), &format!("{nested}/clk_rate"), "MUST NOT READ");
    let snapshot = Source::new(dir.path().into()).capture().unwrap();
    assert!(snapshot.scans["uart0_clock_tree"].truncated);
    assert_eq!(snapshot.scans["uart0_clock_tree"].matches, 0);
    assert!(!serde_json::to_string(&snapshot).unwrap().contains("MUST NOT READ"));
}

#[test]
fn directory_entry_budget_is_not_interpreted_as_proven_absence() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sys/class/tty");
    fs::create_dir_all(&path).unwrap();
    for index in 0..ENTRY_LIMIT + 1 { fs::write(path.join(format!("unrelated{index}")), []).unwrap(); }
    let snapshot = Source::new(dir.path().into()).capture().unwrap();
    let scan = &snapshot.scans["tty_devices"];
    assert!(scan.truncated);
    assert_eq!(scan.entries, ENTRY_LIMIT);
    assert_eq!(scan.matches, 0);
}

#[test]
fn observation_count_budget_prevents_an_unbounded_clock_report() {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..30 {
        write(dir.path(), &format!("sys/kernel/debug/clk/p{index}/sclk_uart0/clk_rate"), "1\n");
    }
    let snapshot = Source::new(dir.path().into()).capture().unwrap();
    assert!(snapshot.records_truncated);
    assert!(snapshot.scans["uart0_clock_tree"].truncated);
    assert!(snapshot.files.len() <= RECORD_LIMIT);
    let mut monitor = Monitor::default();
    monitor.poll(0, "gui_diagnostics", || Ok(snapshot));
    assert!(monitor.report_json().unwrap().len() <= REPORT_LIMIT);
}

#[test]
fn ordinary_session_phases_never_read_uart_sources_or_claim_a_measurement() {
    let mut monitor = Monitor::default();
    let before = monitor.report_json().unwrap();

    for phase in ["initializing", "gui_start", "gui", "gui_exit", "game_start", "game",
        "game_cleanup", "game_exit", "post_game", "input_test", "display_reclaim",
        "gui_error", "session_end", "diagnostic_export", "unknown"]
    {
        monitor.poll(100, phase, || panic!("ordinary phase must not read UART sources"));
        assert_eq!(monitor.report_json().unwrap(), before);
    }

    assert_eq!(monitor.report().samples, 0);
    assert!(monitor.report().snapshot.is_none());
    assert!(monitor.report().ms.is_none());
    assert!(monitor.report().error.is_none());
}

#[test]
fn monitor_captures_once_and_never_retries_a_failed_snapshot() {
    let (_dir, source) = fixture();
    let mut monitor = Monitor::default();
    let calls = Cell::new(0);
    monitor.poll(17, "gui_diagnostics", || { calls.set(calls.get() + 1); source.capture() });
    let first = monitor.report_json().unwrap();
    monitor.poll(3000, "gui_diagnostics", || panic!("must not sample twice"));
    assert_eq!(monitor.report_json().unwrap(), first);
    assert_eq!(calls.get(), 1);
    assert_eq!(monitor.report().ms, Some(17));
    monitor.stop();
    assert!(monitor.report().stopped);

    let mut failed = Monitor::default();
    failed.poll(0, "gui_diagnostics", || Err(io::Error::from(io::ErrorKind::PermissionDenied)));
    failed.poll(1, "gui_diagnostics", || panic!("failed read must not retry"));
    assert_eq!(failed.report().samples, 1);
    assert!(failed.report().error.is_some() && failed.report().snapshot.is_none());
}

#[test]
fn collectors_register_export_without_adding_rpc_commands_or_sampling_on_status() {
    let (_dir, source) = fixture();
    let mut collectors = iman::diagnostics::collectors::Collectors::with_uart_source(source);
    collectors.enable_defaults();
    assert!(iman::diagnostics::cycle::compiled_modules().contains(&"diag_uart"));
    assert!(!iman::diagnostics::collectors::Collectors::capabilities().iter().any(|s| s.starts_with("diag_uart.")));
    assert_eq!(collectors.snapshot()["diag_uart.json"]["data"]["samples"], 0);
    assert!(collectors.status_without_gui().iter().any(|s| s == "UART: waiting for diagnostic screen"));
    assert_eq!(collectors.snapshot()["diag_uart.json"]["data"]["samples"], 0);
    collectors.stop_and_emit();
    assert_eq!(collectors.snapshot()["diag_uart.json"]["data"]["stopped"], true);
}

#[cfg(not(any(feature = "diag-memory", feature = "diag-klog")))]
#[test]
fn uart_only_poll_survives_periodic_collector_early_return_and_exports_to_normal_bundle() {
    let (dir, source) = fixture();
    let mut collectors = iman::diagnostics::collectors::Collectors::with_uart_source(source);
    collectors.enable_defaults();
    for phase in ["initializing", "gui", "game", "input_test", "diagnostic_export"] {
        collectors.poll(phase, 1, None);
    }
    let waiting = collectors.snapshot()["diag_uart.json"].clone();
    assert_eq!(waiting["data"]["samples"], 0);
    assert!(waiting["data"]["snapshot"].is_null());
    assert!(waiting["data"]["ms"].is_null());
    collectors.poll("gui_diagnostics", 1, None);
    let first = collectors.snapshot()["diag_uart.json"].clone();
    write(dir.path(), "proc/version", "changed after snapshot");
    collectors.enable_defaults();
    collectors.poll("gui", 1, None);
    collectors.poll("gui_diagnostics", 1, None);
    assert_eq!(collectors.snapshot()["diag_uart.json"], first);
    assert_eq!(first["data"]["samples"], 1);
    assert_eq!(first["data"]["phase"], "gui_diagnostics");
    assert_eq!(first["data"]["policy"], "diagnostic_screen_once_per_session_software_state_only");
    assert_eq!(first["clock"], "iman_session");
    let mut bundle = iman::diagnostics::export::Bundle::new(serde_json::json!({}));
    bundle.files.insert("launch.log".into(), Vec::new());
    bundle.json("diag_uart.json", &first).unwrap();
    assert!(bundle.files["diag_uart.json"].len() < iman::diagnostics::export::FILE_LIMIT);
    assert!(bundle.manifest().is_ok());
    let export = tempfile::tempdir().unwrap();
    let root = fs::File::open(export.path()).unwrap();
    let output = iman::diagnostics::export::write_bundle(&root, "uart-fixture", &bundle).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(export.path().join(output.parent().unwrap()).join("diag_uart.json")).unwrap()).unwrap();
    assert_eq!(saved, first);
    collectors.stop_and_emit();
    assert!(collectors.status_without_gui().is_empty());
}

#[test]
fn manager_diagnostics_never_register_the_box2_active_uart_probe() {
    assert!(!iman::diagnostics::cycle::compiled_modules().contains(&"diag_uart_ping"));
    let mut collectors = iman::diagnostics::collectors::Collectors::default();
    collectors.enable_defaults();
    assert!(!collectors.snapshot().contains_key("diag_uart_ping.json"));
}
