// SPDX-License-Identifier: BSD-3-Clause
//! Read-only topology snapshots, separate from the raw-event journal budget.
use {
    imanlib::controls::Conflict,
    imanlib::evdev::linux::diagnostic_input_inventory,
    imanlib::gamepad::profile::Profile,
    imanlib::input_probe::linux::usb_inventory,
};
use serde_json::{json, Value};
use std::path::Path;

pub const SNAPSHOT_LIMIT: usize = 4 * 1024;

/// Only sysfs text and directory metadata: no event reads, USB transfers or writes.
pub fn snapshot(input: &Path, input_sysfs: &Path, usb_sysfs: &Path, profiles: &[Profile]) -> Value {
    snapshot_with_conflicts(input, input_sysfs, usb_sysfs, profiles, &[])
}

pub fn snapshot_with_conflicts(input: &Path, input_sysfs: &Path, usb_sysfs: &Path, profiles: &[Profile], conflicts: &[Conflict]) -> Value {
    let usb = usb_inventory(usb_sysfs).unwrap_or_else(|error| {
        json!({"devices":[], "error":error.to_string().chars().take(160).collect::<String>()})
    });
    let mut report = json!({"input":diagnostic_input_inventory(input, input_sysfs, profiles, conflicts), "usb":usb,
        "atomic":false, "device_open_verified":false});
    for field in ["input", "usb"] {
        report[field]["budget_omitted"] = json!(0);
    }
    // Preserve error/summary fields and report every omitted node. Two snapshots
    // fit in 8 KiB even for long escaped sysfs names, independently of raw events.
    while serde_json::to_vec(&report).unwrap().len() > SNAPSHOT_LIMIT {
        let field = ["input", "usb"].into_iter()
            .max_by_key(|field| report[*field]["devices"].as_array().unwrap().len()).unwrap();
        if report[field]["devices"].as_array_mut().unwrap().pop().is_none() { break; }
        report[field]["budget_omitted"] = json!(report[field]["budget_omitted"].as_u64().unwrap() + 1);
    }
    report
}
