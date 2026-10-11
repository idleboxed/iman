// SPDX-License-Identifier: BSD-3-Clause
#![cfg(feature = "diag-input")]
use iman::input_test::inventory::{snapshot, SNAPSHOT_LIMIT};
use std::fs;

#[test]
fn hub_and_two_receivers_are_visible_without_serials_or_device_access() {
    let root = tempfile::tempdir().unwrap();
    for directory in ["input", "sys", "usb"] { fs::create_dir(root.path().join(directory)).unwrap(); }
    for (name, vendor, product) in [("1-1", "214b", "7250"),
        ("1-1.2", "2563", "0555"), ("1-1.3", "2563", "0555")] {
        let usb = root.path().join("usb").join(name);
        fs::create_dir(&usb).unwrap();
        for (field, value) in [("idVendor", vendor), ("idProduct", product), ("serial", "must-not-be-read")] {
            fs::write(usb.join(field), value).unwrap();
        }
    }

    let report = snapshot(&root.path().join("input"), &root.path().join("sys"), &root.path().join("usb"), &[]);

    assert_eq!(report["usb"]["devices"].as_array().unwrap().len(), 3);
    assert_eq!(report["usb"]["devices"][0]["idVendor"], "214b");
    assert_eq!(report["input"]["events_seen"], 0);
    assert_eq!(report["atomic"], false);
    assert_eq!(report["device_open_verified"], false);
    assert!(!report.to_string().contains("must-not-be-read"));
    assert!(serde_json::to_vec(&report).unwrap().len() <= SNAPSHOT_LIMIT);
}

#[test]
fn missing_roots_preserve_both_errors_and_long_nodes_cannot_exhaust_report_budget() {
    let root = tempfile::tempdir().unwrap();
    let missing = snapshot(&root.path().join("input"), &root.path().join("sys"), &root.path().join("usb"), &[]);
    assert!(missing["input"]["error"].is_string());
    assert!(missing["usb"]["error"].is_string());
    for directory in ["input", "sys", "usb"] { fs::create_dir(root.path().join(directory)).unwrap(); }
    for index in 0..20 {
        fs::write(root.path().join(format!("input/event{index}")), []).unwrap();
        let usb = root.path().join("usb").join(format!("{index}-{}", "z".repeat(120)));
        fs::create_dir(&usb).unwrap();
        for field in ["idVendor", "idProduct", "bDeviceClass", "speed", "devpath", "busnum", "devnum", "maxchild"] {
            fs::write(usb.join(field), "\\".repeat(512)).unwrap();
        }
    }

    let report = snapshot(&root.path().join("input"), &root.path().join("sys"), &root.path().join("usb"), &[]);

    assert!(serde_json::to_vec(&report).unwrap().len() <= SNAPSHOT_LIMIT);
    assert_eq!(report["input"]["devices_omitted"], 4);
    assert_eq!(report["usb"]["truncated"], true);
    assert!(report["input"]["budget_omitted"].as_u64().unwrap() + report["usb"]["budget_omitted"].as_u64().unwrap() > 0);
}
