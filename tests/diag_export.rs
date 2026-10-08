// SPDX-License-Identifier: BSD-3-Clause
//! Export only to private tmpfs. No SD, mount operation or host-disk syncfs.
#![cfg(feature = "diag-cycle")]
use iman::diagnostics::export::{self as diag_export, Bundle};
#[cfg(feature = "io-monitor")]
use iman::diagnostics::io::Recorder;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    os::unix::fs::symlink,
};

fn bundle() -> Bundle {
    let mut bundle = Bundle::new(serde_json::json!({"observation_error":"synthetic device missing"}));
    bundle.json("observation.json", &serde_json::Value::Null).unwrap();
    bundle.files.insert("launch.log".into(), b"synthetic fixture, not hardware\n".to_vec());
    bundle
}

#[cfg(feature = "io-monitor")]
#[test]
fn exports_final_counts_log_and_hashes_then_refuses_existing_run_without_overwrite() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let card = File::open(root.path()).unwrap();
    let mut recorder = Recorder::new(vec!["mmcblk0".into()]).unwrap();
    recorder
        .sample(0, "gui", "179 0 mmcblk0 0 0 0 0 1 0 2 3 0 0 0\n")
        .unwrap();
    recorder
        .sample(1000, "session_end", "179 0 mmcblk0 0 0 0 0 3 0 6 7 0 0 0\n")
        .unwrap();
    recorder.stop(None);
    let mut bundle = bundle();
    bundle.json("observation.json", recorder.report()).unwrap();
    bundle.metadata["observation_error"] = serde_json::Value::Null;

    let relative = diag_export::write_bundle(&card, "fixture-run", &bundle).unwrap();

    let manifest_path = root.path().join(relative);
    let manifest_bytes = fs::read(&manifest_path).unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
    for name in ["observation.json", "launch.log"] {
        let bytes = fs::read(manifest_path.parent().unwrap().join(name)).unwrap();
        assert_eq!(manifest["files"][name]["bytes"], bytes.len());
        assert_eq!(
            manifest["files"][name]["sha256"],
            format!("{:x}", Sha256::digest(&bytes))
        );
    }
    let observation: serde_json::Value = serde_json::from_slice(
        &fs::read(manifest_path.parent().unwrap().join("observation.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(observation["totals"][0]["bytes"], 2048);
    assert!(observation["finished"].as_bool().unwrap());
    assert_eq!(manifest["format"], "IMAN-CYCLE-1");
    assert!(diag_export::write_bundle(&card, "fixture-run", &bundle).is_err());
    assert_eq!(fs::read(manifest_path).unwrap(), manifest_bytes);
}

#[test]
fn oversized_or_invalid_input_refuses_before_any_creation() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let card = File::open(root.path()).unwrap();
    let mut bundle = bundle();
    for name in ["", "../escape", "/absolute", "run/child"] {
        assert!(diag_export::write_bundle(&card, name, &bundle).is_err());
    }
    let huge = "x".repeat(diag_export::FILE_LIMIT + 1);
    bundle.files.insert("launch.log".into(), huge.into_bytes());
    assert!(diag_export::write_bundle(&card, "huge", &bundle)
        .unwrap_err()
        .to_string()
        .contains("size limit"));
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn symlink_or_file_in_place_of_export_directory_refuses_without_touching_target() {
    for link in [false, true] {
        let root = tempfile::tempdir_in("/dev/shm").unwrap();
        let other = tempfile::tempdir_in("/dev/shm").unwrap();
        let target = root.path().join("iman-reports");
        if link {
            symlink(other.path(), &target).unwrap();
        } else {
            fs::write(&target, b"do not change").unwrap();
        }

        assert!(
            diag_export::write_bundle(&File::open(root.path()).unwrap(), "run", &bundle()).is_err()
        );

        assert_eq!(fs::read_dir(other.path()).unwrap().count(), 0);
        if !link {
            assert_eq!(fs::read(target).unwrap(), b"do not change");
        }
    }
}

#[test]
fn unavailable_monitor_can_export_explicit_error_without_fake_zero_measurements() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();

    let path = diag_export::write_bundle(
        &File::open(root.path()).unwrap(),
        "failed-observation",
        &bundle(),
    )
    .unwrap();

    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(root.path().join(&path)).unwrap()).unwrap();
    assert_eq!(manifest["metadata"]["observation_error"], "synthetic device missing");
    assert_eq!(
        fs::read(
            root.path()
                .join(path)
                .parent()
                .unwrap()
                .join("observation.json")
        )
        .unwrap(),
        b"null\n"
    );
}

#[test]
fn full_sized_launch_log_is_exported_separately_from_json() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let log = "x".repeat(diag_export::FILE_LIMIT);
    let mut data = bundle();
    data.files.insert("launch.log".into(), log.as_bytes().to_vec());

    let manifest =
        diag_export::write_bundle(&File::open(root.path()).unwrap(), "full-log", &data).unwrap();

    let directory = root.path().join(manifest).parent().unwrap().to_path_buf();
    assert_eq!(
        fs::read_to_string(directory.join("launch.log")).unwrap(),
        log
    );
    assert!(
        fs::metadata(directory.join("report.json")).unwrap().len() < diag_export::FILE_LIMIT as u64
    );
}

#[cfg(feature = "io-monitor")]
#[test]
fn owned_live_snapshot_exports_without_stopping_or_mutating_the_recorder() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let card = File::open(root.path()).unwrap();
    let mut recorder = Recorder::new(vec!["mmcblk0".into()]).unwrap();
    recorder.sample(0, "gui", "179 0 mmcblk0 0 0 0 0 1 0 2 3 0 0 0\n").unwrap();
    let mut data = bundle();
    data.json("observation.json", recorder.report()).unwrap();

    let path = diag_export::write_bundle(&card, "live-snapshot", &data).unwrap();
    recorder.sample(10, "game", "179 0 mmcblk0 0 0 0 0 2 0 4 6 0 0 0\n").unwrap();

    assert!(!recorder.report().finished);
    assert_eq!(recorder.report().totals[0].bytes, 1024);
    let report: serde_json::Value = serde_json::from_slice(&fs::read(root.path().join(path).parent().unwrap().join("observation.json")).unwrap()).unwrap();
    assert_eq!(report["totals"][0]["bytes"], 0);
}

#[test]
fn bundle_rejects_unknown_paths_and_combined_size_before_directory_creation() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let card = File::open(root.path()).unwrap();
    let mut data = bundle();
    data.files.insert("../escape".into(), Vec::new());
    assert!(diag_export::write_bundle(&card, "unsafe", &data).is_err());
    data.files.remove("../escape");
    for name in ["observation.json", "launch.log", "retroarch.log", "diag_memory.json"] {
        data.files.insert(name.into(), vec![b'x'; diag_export::FILE_LIMIT]);
    }
    assert!(diag_export::write_bundle(&card, "too-large", &data).is_err());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn independent_collector_bundle_needs_no_io_and_root_refuses_symlinks() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let other = tempfile::tempdir_in("/dev/shm").unwrap();
    let mut data = Bundle::new(serde_json::json!({"io_state":"not_compiled"}));
    data.files.insert("launch.log".into(), b"synthetic diagnostics".to_vec());
    data.json("diag_memory.json", &serde_json::json!({"synthetic":true})).unwrap();

    let path = diag_export::write_root(&iman::runtime::bundle::Root::open(root.path()).unwrap(), "memory-only", &data).unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();

    assert!(!path.parent().unwrap().join("observation.json").exists());
    assert_eq!(manifest["metadata"]["io_state"], "not_compiled");
    for (name, bytes) in &data.files {
        assert_eq!(manifest["files"][name]["sha256"], format!("{:x}", Sha256::digest(bytes)));
    }
    let link = root.path().join("link");
    symlink(other.path(), &link).unwrap();
    assert!(iman::runtime::bundle::Root::open(&link).is_err());
    assert_eq!(fs::read_dir(other.path()).unwrap().count(), 0);
}

#[test]
fn replaced_session_root_cannot_redirect_export_to_another_directory() {
    let parent = tempfile::tempdir_in("/dev/shm").unwrap();
    let selected = parent.path().join("bundle");
    fs::create_dir(&selected).unwrap();
    let held = iman::runtime::bundle::Root::open(&selected).unwrap();
    fs::rename(&selected, parent.path().join("original")).unwrap();
    fs::create_dir(&selected).unwrap();

    let error = diag_export::write_root(&held, "refused", &bundle()).unwrap_err();

    assert!(error.to_string().contains("identity changed"));
    assert_eq!(fs::read_dir(&selected).unwrap().count(), 0);
    assert_eq!(fs::read_dir(parent.path().join("original")).unwrap().count(), 0);
}

#[test]
fn proposal_subdirectory_is_verified_hashed_and_included_in_the_same_manifest() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let mut data = bundle();
    data.json("diag_input_test.json", &serde_json::json!({"owner":"iman","applied":false})).unwrap();
    data.json("pads-proposal/summary.json", &serde_json::json!({"candidate_only":true})).unwrap();
    data.files.insert("pads-proposal/1234-5678-0123456789abcdef.cfg".into(), b"# synthetic candidate\n".to_vec());
    data.files.insert("pads-proposal/1234-5678-0123456789abcdef.json".into(), b"{}\n".to_vec());

    let relative = diag_export::write_bundle(&File::open(root.path()).unwrap(), "proposals", &data).unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(&fs::read(root.path().join(&relative)).unwrap()).unwrap();
    let directory = root.path().join(relative).parent().unwrap().to_path_buf();

    for (name, expected) in &data.files {
        let bytes = fs::read(directory.join(name)).unwrap();
        assert_eq!(bytes, *expected);
        assert_eq!(manifest["files"][name]["sha256"], format!("{:x}", Sha256::digest(bytes)));
    }
    assert_eq!(fs::read_dir(directory.join("pads-proposal")).unwrap().count(), 3);
    assert!(fs::read_dir(directory.join("pads-proposal")).unwrap().all(|entry|
        !entry.unwrap().file_name().to_string_lossy().ends_with(".partial")));
}

#[test]
fn proposal_paths_and_budgets_are_refused_before_any_directory_is_created() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let card = File::open(root.path()).unwrap();
    for path in ["pads-proposal/../launch.log", "pads-proposal/nested/1234-5678-0123456789abcdef.cfg",
        "/pads-proposal/summary.json", "pads-proposal/1234-5678.cfg", "pads-proposal/1234-5678-0123456789abcdef.txt",
        "pads-proposal/1234-5678-0123456789abcdeF.cfg", "pads-proposal/summary.json/child"] {
        let mut data = bundle();
        data.json("diag_input_test.json", &serde_json::json!({})).unwrap();
        data.json("pads-proposal/summary.json", &serde_json::json!({})).unwrap();
        data.files.insert(path.into(), vec![]);

        assert!(diag_export::write_bundle(&card, "refused", &data).is_err(), "accepted {path}");
    }
    let mut data = bundle();
    data.json("diag_input_test.json", &serde_json::json!({})).unwrap();
    data.files.insert("pads-proposal/summary.json".into(), vec![b' '; diag_export::PROPOSAL_LIMIT + 1]);
    assert!(diag_export::write_bundle(&card, "huge-proposals", &data).is_err());
    data.files.insert("pads-proposal/summary.json".into(), b"{}\n".to_vec());
    for index in 0..diag_export::PROPOSAL_FILES_LIMIT {
        data.files.insert(format!("pads-proposal/1234-5678-{index:016x}.cfg"), b"# candidate\n".to_vec());
    }
    assert!(diag_export::write_bundle(&card, "many-proposals", &data).is_err());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn export_without_candidates_does_not_create_an_empty_proposal_directory() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();

    let path = diag_export::write_bundle(&File::open(root.path()).unwrap(), "no-candidates", &bundle()).unwrap();

    assert!(!root.path().join(path).parent().unwrap().join("pads-proposal").exists());
    let mut data = bundle();
    data.json("pads-proposal/summary.json", &serde_json::json!({})).unwrap();
    assert!(data.manifest().is_err());
    data.json("diag_input_test.json", &serde_json::json!({})).unwrap();
    data.files.remove("pads-proposal/summary.json");
    data.files.insert("pads-proposal/1234-5678-0123456789abcdef.cfg".into(), vec![]);
    assert!(data.manifest().is_err());
}
