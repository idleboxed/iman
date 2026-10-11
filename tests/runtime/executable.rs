// SPDX-License-Identifier: BSD-3-Clause
//! Host executable fixtures; no board, firmware or DRM access.
use iman::runtime::executable;
use std::{fs, os::unix::fs::PermissionsExt};

#[test]
fn real_supervisor_executes_pinned_elf_after_path_replacement_and_reaps_child() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("child");
    fs::copy("/bin/true", &path).unwrap();
    let file = fs::File::open(&path).unwrap();
    fs::rename(&path, root.path().join("original")).unwrap();
    fs::write(&path, b"invalid replacement").unwrap();
    let mut phases = Vec::new();

    let status = executable::supervise(file.into(), vec![c"synthetic-child"], &mut |phase| {
        phases.push(phase)
    })
    .unwrap();

    assert!(status.success());
    assert_eq!(phases.first(), Some(&"gui_start"));
    assert!(phases.contains(&"gui"));
    assert_eq!(phases.last(), Some(&"gui_exit"));
    assert_eq!(fs::read(path).unwrap(), b"invalid replacement");
}

#[test]
fn child_status_and_exec_errors_are_not_silently_successful() {
    let child = fs::File::open("/bin/sh").unwrap();
    let status =
        executable::supervise(child.into(), vec![c"sh", c"-c", c"exit 23"], &mut |_| {}).unwrap();
    assert_eq!(status.code(), Some(23));

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("bad-elf");
    fs::write(&path, b"not an ELF").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    let child = fs::File::open(path).unwrap();
    let error = executable::supervise(child.into(), vec![c"bad"], &mut |_| {}).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ENOEXEC));
}

#[test]
fn gui_descriptor_needs_no_build_hash_and_survives_replacement() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("igui");
    fs::copy("/bin/true", &path).unwrap();
    let dir = iman::runtime::bundle::Root::open(root.path()).unwrap();
    let app = dir.open_executable("igui").unwrap();
    fs::rename(&path, root.path().join("previous")).unwrap();
    fs::write(&path, b"new contents at the old name").unwrap();

    let status = executable::supervise(app.into(), vec![c"igui"], &mut |_| {}).unwrap();

    assert!(status.success());
    assert_eq!(fs::read(path).unwrap(), b"new contents at the old name");
}
