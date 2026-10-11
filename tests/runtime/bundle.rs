// SPDX-License-Identifier: BSD-3-Clause
//! Local-file and tmpfs fixtures; no raw devices, mounts or board bypasses.
use iman::runtime::bundle::Root;
use std::{fs, os::unix::fs::{symlink, PermissionsExt}, path::Path};

#[test]
fn root_and_every_selected_component_refuse_symlinks() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("bundle");
    fs::create_dir_all(root.join("system/igui")).unwrap();
    fs::write(root.join("system/igui/igui"), b"synthetic").unwrap();
    symlink(&root, parent.path().join("alias")).unwrap();
    let held = Root::open(&root).unwrap();

    assert!(Root::open(&parent.path().join("alias")).is_err());
    assert!(Root::open(&parent.path().join("alias/system")).is_err());
    assert!(held.open_file("system/igui/igui").is_ok());
    symlink("igui", root.join("system/link")).unwrap();
    assert!(held.open_file("system/link/igui").is_err());
    symlink("igui", root.join("system/igui/link")).unwrap();
    assert!(held.open_file("system/igui/link").is_err());
}

#[test]
fn file_guards_reject_hardlinks_directories_non_executables_and_bad_paths() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("app");
    fs::write(&file, b"synthetic").unwrap();
    let held = Root::open(root.path()).unwrap();

    assert!(held.open_executable("app").unwrap_err().to_string().contains("not executable"));
    fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(held.open_executable("app").is_ok());
    fs::hard_link(&file, root.path().join("alias")).unwrap();
    assert!(held.open_executable("app").unwrap_err().to_string().contains("single-link"));
    fs::create_dir(root.path().join("directory")).unwrap();
    assert!(held.open_file("directory").is_err());
    for path in ["", "/app", "../app", "directory/../app"] {
        assert!(held.open_file(path).is_err(), "{path}");
    }
}

#[test]
fn replaced_root_is_refused_even_on_the_same_filesystem() {
    let parent = tempfile::tempdir().unwrap();
    let selected = parent.path().join("selected");
    fs::create_dir(&selected).unwrap();
    fs::write(selected.join("data"), b"original").unwrap();
    let held = Root::open(&selected).unwrap();

    fs::rename(&selected, parent.path().join("original")).unwrap();
    fs::create_dir(&selected).unwrap();
    fs::write(selected.join("data"), b"replacement").unwrap();

    assert!(held.validate().unwrap_err().to_string().contains("identity changed"));
    assert!(held.directory().is_err());
    assert!(held.open_file("data").is_err());
    assert_eq!(fs::read(selected.join("data")).unwrap(), b"replacement");
}

#[test]
fn installation_does_not_allow_nested_filesystems() {
    // /proc is a distinct filesystem on the Linux hosts required by iman. Only
    // directory metadata is read: no mount operation or device access is needed.
    let root = Root::open(Path::new("/")).unwrap();

    assert!(root.open_directory("proc").unwrap_err().to_string().contains("same device"));
}

#[test]
fn implicit_root_requires_the_installed_layout() {
    assert_eq!(iman::runtime::paths::root_from_executable(Path::new("/games/system/iman/iman")).unwrap(), Path::new("/games"));
    for path in ["/usr/bin/iman", "/project/target/debug/iman", "/games/system/igui/igui"] {
        assert!(iman::runtime::paths::root_from_executable(Path::new(path)).is_err());
    }
}
