// SPDX-License-Identifier: BSD-3-Clause
//! Real tmpfs/file operations, no board identity bypass or hardware access.
use iman::runtime::startup::RamLog;
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
};

#[test]
fn non_tmpfs_directory_is_refused_before_any_creation() {
    let parent = std::path::Path::new("/proc");

    let error = RamLog::create(parent, "iman-log-must-not-create").err().unwrap();

    assert!(error.to_string().contains("tmpfs"));
    assert!(!parent.join("iman-log-must-not-create").exists());
}

#[test]
fn independent_private_runs_refuse_overwrite() {
    let tmp = tempfile::tempdir_in("/dev/shm").unwrap();
    let cwd = std::env::current_dir().unwrap();
    let mut gui = RamLog::create(tmp.path(), "box2").unwrap();
    let mut probe = RamLog::create(tmp.path(), "box2-display").unwrap();

    gui.record("gui fixture\n").unwrap();
    probe.record("probe fixture\n").unwrap();
    drop((gui, probe));

    assert_eq!(std::env::current_dir().unwrap(), cwd);
    for (name, bytes) in [
        ("box2", b"gui fixture\n".as_slice()),
        ("box2-display", b"probe fixture\n"),
    ] {
        assert_eq!(
            RamLog::create(tmp.path(), name).err().unwrap().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        let path = tmp.path().join(name);
        assert_eq!(fs::read(path.join("igui.log")).unwrap(), bytes);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(path.join("igui.log"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn symlink_parent_or_existing_entry_never_redirects_or_overwrites() {
    let tmp = tempfile::tempdir_in("/dev/shm").unwrap();
    let parent = tmp.path().join("parent");
    fs::create_dir(&parent).unwrap();
    let link = tmp.path().join("link");
    symlink(&parent, &link).unwrap();
    fs::write(tmp.path().join("original"), b"unchanged").unwrap();
    symlink(tmp.path().join("original"), parent.join("run")).unwrap();

    assert!(RamLog::create(&link, "other").is_err());
    assert!(RamLog::create(&parent, "run").is_err());

    assert_eq!(fs::read_dir(&parent).unwrap().count(), 1);
    assert_eq!(fs::read(tmp.path().join("original")).unwrap(), b"unchanged");
}

#[test]
fn invalid_run_names_are_refused_without_side_effects() {
    let tmp = tempfile::tempdir_in("/dev/shm").unwrap();

    for name in [
        "",
        ".",
        "..",
        "../escape",
        "/absolute",
        "nested/run",
        "run/",
        "nul\0name",
    ] {
        assert!(
            RamLog::create(tmp.path(), name).is_err(),
            "accepted {name:?}"
        );
    }

    assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 0);
}

#[cfg(feature = "diag-cycle")]
#[test]
fn segment_log_rotation_keeps_shared_stdio_offsets_and_independent_snapshots() {
    const WORKER: &str = "IMAN_CYCLE_LOG_FIXTURE";
    if let Some(parent) = std::env::var_os(WORKER) {
        let parent = std::path::Path::new(&parent);
        iman::session::install_signal_handlers().unwrap();
        let mut log = RamLog::create(parent, "log").unwrap();
        unsafe { log.activate().unwrap(); }
        log.record("first segment\n").unwrap();
        let first = log.snapshot().unwrap();
        log.reset().unwrap();
        log.record("second segment\n").unwrap();
        let second = log.snapshot().unwrap();
        assert_eq!(first, b"first segment\n");
        assert_eq!(second, b"second segment\n");
        return;
    }
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "segment_log_rotation_keeps_shared_stdio_offsets_and_independent_snapshots", "--nocapture"])
        .env(WORKER, root.path()).output().unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(fs::read_to_string(root.path().join("log/igui.log")).unwrap().starts_with("second segment\n"));
}

#[cfg(feature = "diag-cycle")]
#[test]
fn segment_log_refuses_symlinks_and_unrelated_stdio_without_truncation() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let mut log = RamLog::create(root.path(), "log").unwrap();
    log.record("keep evidence").unwrap();
    let path = root.path().join("log");

    assert!(log.reset().is_err());
    assert_eq!(fs::read(path.join("igui.log")).unwrap(), b"keep evidence");
    fs::rename(path.join("igui.log"), path.join("original")).unwrap();
    symlink(path.join("original"), path.join("igui.log")).unwrap();
    assert!(log.snapshot().is_err());
    assert_eq!(fs::read(path.join("original")).unwrap(), b"keep evidence");
}

#[cfg(feature = "diag-cycle")]
#[test]
fn owned_log_refuses_a_regular_file_replacement_without_touching_either_file() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let mut log = RamLog::create(root.path(), "log").unwrap();
    log.record("original evidence").unwrap();
    let path = root.path().join("log/igui.log");
    let original = root.path().join("log/original");
    fs::rename(&path, &original).unwrap();
    fs::write(&path, b"unrelated replacement").unwrap();

    assert!(log.snapshot().is_err());
    assert!(log.reset().is_err());

    assert_eq!(fs::read(&original).unwrap(), b"original evidence");
    assert_eq!(fs::read(&path).unwrap(), b"unrelated replacement");
}

#[test]
fn missing_runtime_leaf_is_created_only_on_tmpfs_and_logs_are_cleaned() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let runtime = root.path().join("runtime");

    iman::runtime::startup::prepare_runtime(&runtime).unwrap();
    let mut log = RamLog::temporary(&runtime).unwrap();
    log.record("synthetic temporary log\n").unwrap();

    assert_eq!(fs::read_dir(&runtime).unwrap().count(), 1);
    drop(log);
    assert_eq!(fs::read_dir(&runtime).unwrap().count(), 0);
    iman::runtime::startup::prepare_runtime(&runtime).unwrap();
    symlink(&runtime, root.path().join("alias")).unwrap();
    assert!(iman::runtime::startup::prepare_runtime(&root.path().join("alias/child")).is_err());
    assert!(!runtime.join("child").exists());
}

#[test]
fn missing_runtime_on_non_tmpfs_is_refused_before_creation() {
    let path = std::path::Path::new("/proc/iman-runtime-must-not-create");

    let error = iman::runtime::startup::prepare_runtime(path).unwrap_err();

    assert!(error.to_string().contains("tmpfs"));
    assert!(!path.exists());
}
