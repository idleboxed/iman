// SPDX-License-Identifier: BSD-3-Clause
#![cfg(any(
    feature = "diag-memory",
    feature = "diag-klog",
    feature = "diag-uart",
    feature = "diag-display",
    feature = "diag-input"
))]
use iman::diagnostics::collectors::Collectors;

#[test]
fn startup_enables_compiled_modules_and_runtime_start_stop_commands_are_absent() {
    let mut c = Collectors::default();
    c.enable_defaults();
    for name in Collectors::capabilities().into_iter()
        .filter_map(|command| command.strip_suffix(".report"))
    {
        let report = c.command(&format!("{name}.report")).unwrap();
        assert_eq!(report["stopped"], false);
        assert!(c.command(&format!("{name}.start")).is_err());
        assert!(c.command(&format!("{name}.stop")).is_err());
    }
    c.stop_and_emit();
    c.stop_and_emit();
    for command in Collectors::capabilities().into_iter().filter(|s| s.ends_with(".report")) {
        assert_eq!(c.command(command).unwrap()["stopped"], true);
    }
}

#[test]
fn standalone_status_never_claims_gui_measurements_or_samples_disabled_modules() {
    let mut c = Collectors::default();
    c.enable_defaults();
    let lines = c.status_without_gui();

    #[cfg(feature = "diag-memory")]
    assert!(lines.iter().any(|line| line == "RAM: 0 samples, 0 read errors"));
    #[cfg(feature = "diag-klog")]
    assert!(lines.iter().any(|line| line == "Kernel log: 0 samples, 0 read errors"));
    assert!(!lines.iter().any(|line| line.contains("IGUI") || line.contains("Input")));
    c.stop_and_emit();
    assert!(c.status_without_gui().is_empty());
}

#[test]
fn explicit_board_defaults_start_compiled_modules_but_do_not_sample_in_constructor() {
    let mut c = Collectors::default();
    c.enable_defaults();
    for command in Collectors::capabilities()
        .into_iter()
        .filter(|s| s.ends_with(".report"))
    {
        let report = c.command(command).unwrap();
        assert_eq!(
            report
                .get("samples")
                .or_else(|| report.get("received"))
                .unwrap(),
            0
        );
        assert_eq!(report["stopped"], false);
    }
    assert!(c.command("diag_unknown.start").is_err());
}

#[test]
fn structured_snapshot_keeps_existing_payloads_without_finalizing_collectors() {
    let mut collectors = iman::diagnostics::collectors::Collectors::default();
    collectors.enable_defaults();

    let first = collectors.snapshot();
    let second = collectors.snapshot();

    assert_eq!(first, second);
    for report in first.values() {
        assert_eq!(report["scope"], "collector_epoch");
        assert_eq!(report["clock"], "iman_session");
        assert_eq!(report["data"]["stopped"], false);
    }
    collectors.stop_and_emit();
    for report in collectors.snapshot().values() {
        assert_eq!(report["data"]["stopped"], true);
    }
}

#[cfg(feature = "diag-klog")]
#[test]
fn usb_archive_is_included_in_existing_ipc_and_structured_export_without_extra_commands() {
    let mut collectors = Collectors::default();
    collectors.enable_defaults();

    let ipc = collectors.command("diag_klog.report").unwrap();
    let reports = collectors.snapshot();

    assert_eq!(ipc["version"], 3);
    assert_eq!(ipc["usb"]["retention"], "first_16_latest_16");
    assert_eq!(ipc["usb"]["last_successful_sample_ms"], serde_json::Value::Null);
    assert_eq!(ipc["usb"]["records"], serde_json::json!([]));
    assert_eq!(reports["diag_klog.json"]["data"], ipc);
    assert!(!ipc["stopped"].as_bool().unwrap());
}
