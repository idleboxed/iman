// SPDX-License-Identifier: BSD-3-Clause
//! Synthetic profiles, memory bytes and display adapters in tmpfs; no physical device access.
#![cfg(feature = "diag-memdump")]
use std::{cell::{Cell, RefCell}, fs, io, os::unix::fs::symlink, path::Path, rc::Rc};

use iman::{
    diagnostics::{export::Bundle, memdump::{self, Monitor, State,
        profile::{self, Identity, LinuxPlatform, Platform, Profile, ReadPlan}, reader::{Capture, LinuxReader, Reader}}},
    display::Lease,
    runtime::{bundle::Root, paths::MEMORY_PROFILES_DIR, startup::RamLog},
    session::{self, control::Control, GameEnvironment},
};
use serde_json::{json, Value};

#[path = "common/timing.rs"]
mod timing;

fn identity() -> Identity {
    Identity { architecture: std::env::consts::ARCH.into(), model: Ok("Synthetic board, not hardware".to_owned()).into(),
        compatible: vec!["synthetic,board".into(), "synthetic,soc".into()], kernel: Ok("Synthetic kernel".to_owned()).into() }
}

fn profile_value() -> Value {
    json!({
        "schema":1, "description":"Synthetic read fixture, not hardware",
        "match":{"compatible":"synthetic,board"},
        "read":{"enabled":true,"address":"0x1000","length":4,"unit":4}
    })
}

fn write_profile(root: &Path, filename: &str, value: &Value) {
    let directory = root.join(MEMORY_PROFILES_DIR);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(filename), serde_json::to_vec(value).unwrap()).unwrap();
}

#[derive(Clone)]
struct SyntheticPlatform { value: Rc<RefCell<Identity>>, calls: Rc<Cell<usize>> }
impl SyntheticPlatform {
    fn new() -> Self { Self { value: Rc::new(RefCell::new(identity())), calls: Rc::new(Cell::new(0)) } }
}
impl Platform for SyntheticPlatform {
    fn identify(&self) -> io::Result<Identity> {
        self.calls.set(self.calls.get() + 1);
        Ok(self.value.borrow().clone())
    }
}

#[derive(Clone)]
struct SyntheticReader {
    calls: Rc<Cell<usize>>,
    events: Rc<RefCell<Vec<String>>>,
    bytes: Vec<u8>,
    error: Option<String>,
    fail: bool,
}
impl Default for SyntheticReader {
    fn default() -> Self {
        Self { calls: Rc::new(Cell::new(0)), events: Rc::new(RefCell::new(Vec::new())),
            bytes: vec![0x11, 0x22, 0x33, 0x44], error: None, fail: false }
    }
}
impl Reader for SyntheticReader {
    fn read(&self, plan: &ReadPlan) -> io::Result<Capture> {
        assert_eq!(plan.address, 0x1000);
        self.calls.set(self.calls.get() + 1);
        self.events.borrow_mut().push("read".into());
        if self.fail { return Err(io::Error::new(io::ErrorKind::PermissionDenied, "synthetic policy refusal")); }
        Ok(Capture { bytes: self.bytes.clone(), error: self.error.clone(), mapping: None })
    }
}

#[test]
fn candidate_profile_is_external_disabled_and_limited_to_first_word() {
    let profile = Profile::parse(include_bytes!("../memory-profiles/rk3032-dongle.json")).unwrap();

    assert!(!profile.read.enabled);
    assert_eq!(profile.plan().unwrap(), ReadPlan { address: 0x10100000, length: 4, unit: 4 });
    assert_eq!(profile.matcher.compatible, "rockchip,rk3032-dongle");
    assert!(profile.description.contains("Unconfirmed"));
}

#[test]
fn invalid_profile_shapes_and_requests_fail_before_any_reader() {
    let variants = [
        ("schema", json!(2)), ("description", json!("")),
        ("description", json!("bad\nlabel")), ("unknown", json!(true)),
    ];
    for (field, value) in variants {
        let mut data = profile_value();
        data[field] = value;

        assert!(Profile::parse(&serde_json::to_vec(&data).unwrap()).is_err(), "accepted {field}");
    }
    for (field, value) in [("length", json!(0)), ("length", json!(3)), ("length", json!(16388)),
        ("unit", json!(0)), ("unit", json!(8)), ("unit", json!(3)), ("enabled", json!("true")),
        ("address", json!("0x1001")), ("address", json!("4096")), ("address", json!(4096)),
        ("address", json!("0x")), ("address", json!("0xgg")), ("address", json!("0x10000000000000000")),
        ("address", json!("0xfffffffffffffffc")), ("address", json!("0x7ffffffffffffffc")),
        ("unknown", json!(true))]
    {
        let mut data = profile_value();
        data["read"][field] = value;

        assert!(Profile::parse(&serde_json::to_vec(&data).unwrap()).is_err(), "accepted read.{field}");
    }
    for (field, value) in [("compatible", json!("")), ("compatible", json!("bad\nlabel")),
        ("compatible", json!(["synthetic,board"])), ("unknown", json!(true))]
    {
        let mut data = profile_value();
        data["match"][field] = value;

        assert!(Profile::parse(&serde_json::to_vec(&data).unwrap()).is_err(), "accepted match.{field}");
    }
}

#[test]
fn profile_accepts_aligned_widths_and_lengths_up_to_the_fixed_limit() {
    for (address, length, unit) in [(0x1001, 1, 1), (0x1002, 2, 2), (0x1004, 8, 4),
        (0x1000, profile::DUMP_LIMIT, 4), (0x7ffffffffffffffe, 1, 1)]
    {
        let mut data = profile_value();
        data["read"]["address"] = json!(format!("0x{address:x}"));
        data["read"]["length"] = json!(length);
        data["read"]["unit"] = json!(unit);

        let profile = Profile::parse(&serde_json::to_vec(&data).unwrap()).unwrap();

        assert_eq!(profile.plan().unwrap(), ReadPlan { address, length, unit });
    }
}

#[test]
fn platform_requires_compatible_but_reports_missing_or_invalid_model_and_kernel() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let device_tree = root.path().join("sys/firmware/devicetree/base");
    fs::create_dir_all(&device_tree).unwrap();
    fs::create_dir(root.path().join("proc")).unwrap();
    fs::write(device_tree.join("model"), b"Synthetic board, not hardware\0").unwrap();
    fs::write(device_tree.join("compatible"), b"synthetic,board\0synthetic,soc\0").unwrap();
    fs::write(root.path().join("proc/version"), b"Synthetic kernel\n").unwrap();
    let source = LinuxPlatform::new(root.path().into());

    assert_eq!(serde_json::to_value(source.identify().unwrap()).unwrap(), serde_json::to_value(identity()).unwrap());
    fs::remove_file(device_tree.join("model")).unwrap();
    fs::remove_file(root.path().join("proc/version")).unwrap();
    let missing = source.identify().unwrap();
    assert!(missing.model.value.is_none() && missing.model.error.is_some());
    assert!(missing.kernel.value.is_none() && missing.kernel.error.is_some());
    assert!(Profile::parse(&serde_json::to_vec(&profile_value()).unwrap()).unwrap().matcher.matches(&missing));
    fs::write(root.path().join("proc/version"), b"invalid\nlabel").unwrap();
    symlink(root.path().join("proc/version"), device_tree.join("model")).unwrap();
    let invalid = source.identify().unwrap();
    assert!(invalid.model.error.unwrap().contains("regular file"));
    assert!(invalid.kernel.error.unwrap().contains("invalid platform identity label"));
    fs::remove_file(device_tree.join("compatible")).unwrap();
    assert!(source.identify().is_err());
    for bytes in [b"".as_slice(), b"invalid\nlabel\0"] {
        fs::write(device_tree.join("compatible"), bytes).unwrap();
        assert!(source.identify().unwrap_err().to_string().contains("platform compatible"));
    }
    fs::remove_file(device_tree.join("compatible")).unwrap();
    symlink("model", device_tree.join("compatible")).unwrap();
    assert!(source.identify().unwrap_err().to_string().contains("regular file"));
    assert!(LinuxPlatform::new("relative".into()).identify().is_err());
}

#[test]
fn selection_is_external_exact_unique_and_refuses_any_malformed_profile() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let held = Root::open(root.path()).unwrap();
    assert!(profile::select(&held, &identity()).unwrap().is_none());
    write_profile(root.path(), "selected.json", &profile_value());
    let mut other = profile_value();
    other["match"]["compatible"] = json!("synthetic,other-board");
    write_profile(root.path(), "other.json", &other);

    let selected = profile::select(&held, &identity()).unwrap().unwrap();
    assert_eq!(selected.filename, "selected.json");
    assert_eq!(selected.sha256.len(), 64);
    for compatible in ["synthetic,unknown", "synthetic,board-rev2", "synthetic,boar"] {
        let mut unknown = identity();
        unknown.compatible = vec![compatible.into()];
        assert!(profile::select(&held, &unknown).unwrap().is_none(), "{compatible}");
    }
    let mut report_only = identity();
    report_only.architecture = "different architecture".into();
    report_only.model = Ok("Different board description".to_owned()).into();
    report_only.kernel = Err(io::Error::other("synthetic kernel unavailable")).into();
    assert_eq!(profile::select(&held, &report_only).unwrap().unwrap().filename, "selected.json");
    write_profile(root.path(), "duplicate.json", &profile_value());
    assert!(profile::select(&held, &identity()).unwrap_err().to_string().contains("multiple"));
    fs::remove_file(root.path().join(MEMORY_PROFILES_DIR).join("duplicate.json")).unwrap();
    fs::write(root.path().join(MEMORY_PROFILES_DIR).join("broken.json"), b"{").unwrap();
    assert!(profile::select(&held, &identity()).is_err());
}

#[test]
fn profile_files_refuse_symlinks_hardlinks_and_special_nodes_without_opening_devices() {
    for kind in ["symlink", "hardlink", "directory", "fifo"] {
        let root = tempfile::tempdir_in("/dev/shm").unwrap();
        write_profile(root.path(), "valid.json", &profile_value());
        let path = root.path().join(MEMORY_PROFILES_DIR).join("invalid.json");
        match kind {
            "symlink" => symlink("valid.json", &path).unwrap(),
            "hardlink" => fs::hard_link(root.path().join(MEMORY_PROFILES_DIR).join("valid.json"), &path).unwrap(),
            "directory" => fs::create_dir(&path).unwrap(),
            "fifo" => {
                use std::os::unix::ffi::OsStrExt;
                let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            }
            _ => unreachable!(),
        }

        let error = profile::select(&Root::open(root.path()).unwrap(), &identity()).unwrap_err();

        assert!(error.to_string().contains("regular single-link"), "{kind}: {error}");
    }
}

#[test]
fn profile_registry_limits_and_symlinked_parent_fail_closed() {
    for mode in ["oversized", "count", "entries", "parent_symlink"] {
        let root = tempfile::tempdir_in("/dev/shm").unwrap();
        write_profile(root.path(), "fixture.json", &profile_value());
        let directory = root.path().join(MEMORY_PROFILES_DIR);
        match mode {
            "oversized" => fs::write(directory.join("fixture.json"), vec![b' '; profile::PROFILE_LIMIT + 1]).unwrap(),
            "count" => {
                for index in 0..profile::PROFILE_COUNT_LIMIT {
                    write_profile(root.path(), &format!("profile-{index}.json"), &profile_value());
                }
            }
            "entries" => {
                for index in 0..64 { fs::write(directory.join(format!("ignored-{index}.txt")), b"").unwrap(); }
            }
            "parent_symlink" => {
                let moved = root.path().join("profiles");
                fs::rename(&directory, &moved).unwrap();
                symlink(moved, &directory).unwrap();
            }
            _ => unreachable!(),
        }

        assert!(profile::select(&Root::open(root.path()).unwrap(), &identity()).is_err(), "{mode}");
    }
}

#[test]
fn preparation_and_snapshot_never_read_memory_and_disabled_profile_has_explicit_status() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let platform = SyntheticPlatform::new();
    let reader = SyntheticReader::default();
    let mut monitor = Monitor::new(platform.clone(), reader.clone());
    assert_eq!(platform.calls.get(), 0);
    assert_eq!(reader.calls.get(), 0);
    let mut value = profile_value();
    value["read"]["enabled"] = json!(false);
    write_profile(root.path(), "fixture.json", &value);

    monitor.prepare(&Root::open(root.path()).unwrap());
    monitor.prepare(&Root::open(root.path()).unwrap());
    monitor.capture_once(root.path(), 0);

    assert_eq!(platform.calls.get(), 1);
    assert_eq!(reader.calls.get(), 0);
    assert_eq!(monitor.report().state, State::Skipped);
    assert!(monitor.status().contains("disabled in selected profile"));
    assert!(!root.path().join("iman-memory-dump.attempt").exists());
    let first = serde_json::to_value(monitor.report()).unwrap();
    assert_eq!(first, serde_json::to_value(monitor.report()).unwrap());
}

#[test]
fn changed_platform_profile_or_ambiguous_selection_prevents_read_without_reselection() {
    for change in ["platform", "profile", "duplicate", "root"] {
        let root = tempfile::tempdir_in("/dev/shm").unwrap();
        write_profile(root.path(), "fixture.json", &profile_value());
        let held = Root::open(root.path()).unwrap();
        let platform = SyntheticPlatform::new();
        let reader = SyntheticReader::default();
        let mut monitor = Monitor::new(platform.clone(), reader.clone());
        monitor.prepare(&held);
        match change {
            "platform" => platform.value.borrow_mut().compatible = vec!["synthetic,other-board".into()],
            "profile" => { let mut value = profile_value(); value["description"] = json!("Changed");
                write_profile(root.path(), "fixture.json", &value); }
            "duplicate" => write_profile(root.path(), "duplicate.json", &profile_value()),
            "root" => { fs::rename(root.path().join("system"), root.path().join("old-system")).unwrap(); }
            _ => unreachable!(),
        }

        monitor.validate(&held);
        monitor.capture_once(root.path(), 0);

        assert_eq!(monitor.report().state, State::Skipped, "{change}");
        assert_eq!(reader.calls.get(), 0);
        assert!(!root.path().join("iman-memory-dump.attempt").exists());
    }
}

#[test]
fn report_only_identity_changes_are_updated_without_preventing_the_attempt() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    write_profile(root.path(), "fixture.json", &profile_value());
    let held = Root::open(root.path()).unwrap();
    let platform = SyntheticPlatform::new();
    let reader = SyntheticReader::default();
    let mut monitor = Monitor::new(platform.clone(), reader.clone());
    monitor.prepare(&held);
    let mut changed = identity();
    changed.architecture = "different architecture".into();
    changed.model = Ok("Updated board description".to_owned()).into();
    changed.kernel = Err(io::Error::other("synthetic kernel unavailable")).into();
    changed.compatible.reverse();
    *platform.value.borrow_mut() = changed.clone();

    monitor.validate(&held);
    monitor.capture_once(root.path(), 0);

    assert_eq!(monitor.report().state, State::Captured);
    assert_eq!(reader.calls.get(), 1);
    assert_eq!(serde_json::to_value(&monitor.report().identity).unwrap(), serde_json::to_value(changed).unwrap());
}

#[test]
fn successful_capture_is_once_only_and_ram_marker_blocks_a_second_session() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    write_profile(root.path(), "fixture.json", &profile_value());
    let held = Root::open(root.path()).unwrap();
    let reader = SyntheticReader::default();
    let mut monitor = Monitor::new(SyntheticPlatform::new(), reader.clone());
    monitor.prepare(&held);

    monitor.capture_once(root.path(), 42);
    monitor.capture_once(root.path(), 43);

    assert_eq!(reader.calls.get(), 1);
    assert_eq!(monitor.report().state, State::Captured);
    assert_eq!(monitor.report().attempts, 1);
    assert_eq!(monitor.report().ms, Some(42));
    assert!(monitor.report().complete);
    assert_eq!(monitor.bytes(), &[0x11, 0x22, 0x33, 0x44]);
    assert!(monitor.status().contains("in RAM"));
    let mut second = Monitor::new(SyntheticPlatform::new(), reader.clone());
    second.prepare(&held);
    second.capture_once(root.path(), 100);
    assert_eq!(second.report().state, State::Skipped);
    assert!(second.status().contains("attempt guard refused"));
    assert_eq!(reader.calls.get(), 1);
}

#[test]
fn failed_and_partial_readings_retain_truthful_counts_without_retry() {
    for mode in ["policy", "partial", "oversized", "incomplete"] {
        let root = tempfile::tempdir_in("/dev/shm").unwrap();
        let mut value = profile_value();
        value["read"]["unit"] = json!(2);
        write_profile(root.path(), "fixture.json", &value);
        let mut reader = SyntheticReader::default();
        match mode {
            "policy" => reader.fail = true,
            "partial" => { reader.bytes.truncate(2); reader.error = Some("synthetic external abort".into()); }
            "oversized" => reader.bytes.extend_from_slice(&[0, 0]),
            "incomplete" => reader.bytes.clear(),
            _ => unreachable!(),
        }
        let mut monitor = Monitor::new(SyntheticPlatform::new(), reader.clone());
        monitor.prepare(&Root::open(root.path()).unwrap());

        monitor.capture_once(root.path(), 0);
        monitor.capture_once(root.path(), 1);

        assert_eq!(monitor.report().state, State::Failed, "{mode}");
        assert!(!monitor.report().complete);
        assert_eq!(reader.calls.get(), 1);
        assert_eq!(monitor.report().captured_bytes, if mode == "partial" { 2 } else { 0 });
        assert!(monitor.report().reason.is_some());
    }
}

#[test]
fn native_reader_refuses_invalid_plans_and_non_devices_before_physical_access() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let path = root.path().join("not-memory");
    fs::write(&path, b"synthetic device placeholder").unwrap();
    let reader = LinuxReader::new(path.clone());
    let plan = ReadPlan { address: 0x1000, length: 4, unit: 4 };

    assert!(reader.read(&plan).err().unwrap().to_string().contains("character device 1:1"));
    let mut invalid = plan;
    invalid.address = u64::MAX;
    assert!(reader.read(&invalid).err().unwrap().to_string().contains("invalid physical-memory read plan"));
    fs::remove_file(&path).unwrap();
    symlink("missing", &path).unwrap();
    assert!(reader.read(&plan).err().unwrap().to_string().contains("character device 1:1"));
}

struct Screen { events: Rc<RefCell<Vec<String>>>, start_visible: bool, start_error: bool, reclaim_error: bool }
impl Lease for Screen {
    fn lend(&mut self) -> io::Result<()> { self.events.borrow_mut().push("lend".into()); Ok(()) }
    fn reclaim(&mut self) -> io::Result<()> {
        self.events.borrow_mut().push("reclaim".into());
        if self.reclaim_error { return Err(io::Error::other("synthetic reclaim failure")); }
        Ok(())
    }
    fn finish(&mut self) -> io::Result<()> { self.events.borrow_mut().push("finish".into()); Ok(()) }
    fn export_status(&mut self, message: &str, _failed: bool) -> io::Result<bool> {
        self.events.borrow_mut().push(message.into());
        if message.starts_with("Memory dump: reading") {
            if self.start_error { return Err(io::Error::other("synthetic presentation failure")); }
            return Ok(self.start_visible);
        }
        Ok(true)
    }
}

#[test]
fn normal_exit_presents_start_before_reading_then_exports_existing_capture_and_manifest() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    write_profile(root.path(), "fixture.json", &profile_value());
    let reader = SyntheticReader::default();
    let mut screen = Screen { events: reader.events.clone(), start_visible: true, start_error: false, reclaim_error: false };
    let mut control = Control::bind(root.path(), root.path()).unwrap();
    control.set_memory_dump_sources(SyntheticPlatform::new(), reader.clone()).unwrap();
    control.attach_launch_log(RamLog::temporary(root.path()).unwrap());
    control.start_diagnostics().unwrap();
    assert_eq!(reader.calls.get(), 0);

    session::run_session_with_timing(root.path(), GameEnvironment::Inherited, &mut control,
        &mut |_| {}, &mut screen, &mut timing::ManualTiming::default(), |_, _, _, _| Ok(None)).unwrap();

    assert_eq!(reader.calls.get(), 1);
    let events = reader.events.borrow();
    let begin = events.iter().position(|event| event.starts_with("Memory dump: reading")).unwrap();
    let read = events.iter().position(|event| event == "read").unwrap();
    let captured = events.iter().position(|event| event.starts_with("Memory dump: captured")).unwrap();
    let export = events.iter().position(|event| event == "Saving diagnostics...").unwrap();
    assert!(begin < read && read < captured && captured < export);
    assert!(events.iter().any(|event| event == "Diagnostics synchronized."));
    let receipt = control.synchronized_report_path().unwrap();
    let manifest: Value = serde_json::from_slice(&fs::read(receipt).unwrap()).unwrap();
    let directory = receipt.parent().unwrap();
    let report: Value = serde_json::from_slice(&fs::read(directory.join(memdump::REPORT_FILE)).unwrap()).unwrap();
    assert_eq!(report["data"]["state"], "captured");
    assert_eq!(report["data"]["complete"], true);
    assert_eq!(report["data"]["attempts"], 1);
    assert_eq!(report["scope"], "session");
    assert_eq!(report["data"]["method"], "provided_reader");
    assert!(report["data"]["mapping"].is_null());
    assert_eq!(report["data"]["storage"], "ram_not_export_confirmation");
    assert_eq!(fs::read(directory.join(memdump::DATA_FILE)).unwrap(), reader.bytes);
    assert_eq!(manifest["files"][memdump::DATA_FILE]["bytes"], 4);
    assert_eq!(manifest["files"][memdump::DATA_FILE]["sha256"], report["data"]["sha256"]);
}

#[test]
fn failures_and_unpresented_start_skip_reading_and_show_the_reason() {
    for mode in ["failed_gui", "cancelled", "invisible", "presentation_error", "missing_profile", "disabled", "reclaim_error"] {
        let root = tempfile::tempdir_in("/dev/shm").unwrap();
        if mode != "missing_profile" {
            let mut value = profile_value();
            value["read"]["enabled"] = json!(mode != "disabled");
            write_profile(root.path(), "fixture.json", &value);
        }
        let reader = SyntheticReader::default();
        let mut screen = Screen { events: reader.events.clone(), start_visible: mode != "invisible",
            start_error: mode == "presentation_error", reclaim_error: mode == "reclaim_error" };
        let mut control = Control::bind(root.path(), root.path()).unwrap();
        control.set_memory_dump_sources(SyntheticPlatform::new(), reader.clone()).unwrap();
        control.attach_launch_log(RamLog::temporary(root.path()).unwrap());
        control.start_diagnostics().unwrap();

        let result = session::run_session_with_timing(root.path(), GameEnvironment::Inherited, &mut control,
            &mut |_| {}, &mut screen, &mut timing::ManualTiming::default(), |_, _, _, _| {
                match mode {
                    "failed_gui" => Err(io::Error::other("synthetic GUI failure")),
                    "cancelled" => Err(io::Error::new(io::ErrorKind::Interrupted, "synthetic cancellation")),
                    _ => Ok(None),
                }
            });

        assert_eq!(reader.calls.get(), 0, "{mode}");
        assert_eq!(control.memory_dump_report().state, State::Skipped, "{mode}");
        assert!(control.memory_dump_report().reason.is_some());
        assert!(!root.path().join("iman-memory-dump.attempt").exists());
        assert_eq!(result.is_err(), matches!(mode, "failed_gui" | "cancelled" | "presentation_error" | "reclaim_error"));
        if !matches!(mode, "presentation_error" | "reclaim_error") {
            assert!(reader.events.borrow().iter().any(|event| event.starts_with("Memory dump: skipped")), "{mode}");
        }
    }
}

#[test]
fn unprepared_library_session_has_no_probe_effects_or_status_changes() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let reader = SyntheticReader::default();
    let platform = SyntheticPlatform::new();
    let mut screen = Screen { events: reader.events.clone(), start_visible: true, start_error: false, reclaim_error: false };
    let mut control = Control::bind(root.path(), root.path()).unwrap();
    control.set_memory_dump_sources(platform.clone(), reader.clone()).unwrap();

    session::run_session_with_timing(root.path(), GameEnvironment::Inherited, &mut control,
        &mut |_| {}, &mut screen, &mut timing::ManualTiming::default(), |_, _, _, _| Ok(None)).unwrap();

    assert_eq!(platform.calls.get(), 0);
    assert_eq!(reader.calls.get(), 0);
    assert_eq!(control.memory_dump_report().state, State::Unprepared);
    assert_eq!(&*reader.events.borrow(), &["lend", "reclaim", "finish"]);
}

#[test]
fn active_child_or_disabled_export_prevents_attempt_and_source_replacement() {
    for mode in ["child", "export_disabled"] {
        let root = tempfile::tempdir_in("/dev/shm").unwrap();
        write_profile(root.path(), "fixture.json", &profile_value());
        let reader = SyntheticReader::default();
        let mut control = Control::bind(root.path(), root.path()).unwrap();
        control.set_memory_dump_sources(SyntheticPlatform::new(), reader.clone()).unwrap();
        control.start_diagnostics().unwrap();
        if mode == "child" { control.set_game(Some(u32::MAX)); }
        else { control.disable_cycle_exports("synthetic export refusal"); }

        control.capture_memory_dump();

        assert_eq!(reader.calls.get(), 0);
        assert_eq!(control.memory_dump_report().state, State::Skipped);
        assert!(!root.path().join("iman-memory-dump.attempt").exists());
        assert!(control.set_memory_dump_sources(SyntheticPlatform::new(), reader.clone()).unwrap_err()
            .to_string().contains("before startup"));
        control.set_game(None);
    }
}

#[test]
fn ordinary_poll_and_export_never_trigger_active_memory_access() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    write_profile(root.path(), "fixture.json", &profile_value());
    let reader = SyntheticReader::default();
    let mut control = Control::bind(root.path(), root.path()).unwrap();
    control.set_memory_dump_sources(SyntheticPlatform::new(), reader.clone()).unwrap();
    control.attach_launch_log(RamLog::temporary(root.path()).unwrap());
    control.start_diagnostics().unwrap();

    for phase in ["gui", "game", "post_game", "gui_diagnostics", "session_end", "diagnostic_export"] {
        control.poll(phase);
    }
    control.finish(None, false).unwrap();

    assert_eq!(reader.calls.get(), 0);
    assert_eq!(control.memory_dump_report().state, State::Skipped);
    assert!(!root.path().join("iman-memory-dump.attempt").exists());
    let receipt = control.synchronized_report_path().unwrap();
    assert!(!receipt.parent().unwrap().join(memdump::DATA_FILE).exists());
}

#[test]
fn binary_export_requires_metadata_and_preserves_existing_file_and_bundle_limits() {
    let mut bundle = Bundle::new(json!({}));
    bundle.files.insert("launch.log".into(), Vec::new());
    bundle.files.insert(memdump::DATA_FILE.into(), vec![0; 4]);

    assert!(bundle.manifest().unwrap_err().to_string().contains("structured report"));
    bundle.json(memdump::REPORT_FILE, &json!({"synthetic":true})).unwrap();
    assert!(bundle.manifest().is_ok());
    bundle.files.insert(memdump::DATA_FILE.into(), vec![0; iman::diagnostics::export::FILE_LIMIT + 1]);
    assert!(bundle.manifest().unwrap_err().to_string().contains("size limit"));
}
