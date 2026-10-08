// SPDX-License-Identifier: BSD-3-Clause
//! Synthetic directory selection, without executing an emulator or touching SD.
use std::{fs, os::unix::fs::symlink};
use iman::emulation::bios;

#[test]
fn arbitrary_platform_directory_is_selected_without_enumerating_its_files() {
    let source = tempfile::tempdir().unwrap();
    let platform = source.path().join("system/bios/TEST_2");
    fs::create_dir_all(platform.join("sub")).unwrap();
    fs::write(platform.join("sub/日本語.bin"), b"firmware").unwrap();
    // No firmware files or unrelated platforms are opened or validated by iman.
    symlink("/missing", platform.join("unreadable")).unwrap();
    symlink("/missing", platform.parent().unwrap().join("OTHER")).unwrap();

    assert_eq!(bios::directory(source.path(), "TEST_2").unwrap(), Some(platform));
}

#[test]
fn absent_tree_is_optional_and_is_not_created() {
    let source = tempfile::tempdir().unwrap();
    fs::create_dir(source.path().join("system")).unwrap();
    assert!(bios::directory(source.path(), "NEW").unwrap().is_none());
    assert!(!source.path().join("system/bios").exists());
    fs::create_dir(source.path().join("system/bios")).unwrap();
    assert!(bios::directory(source.path(), "NEW").unwrap().is_none());
    assert!(!source.path().join("system/bios/NEW").exists());
}

#[test]
fn empty_existing_directory_is_passed_to_the_core() {
    let source = tempfile::tempdir().unwrap();
    let platform = source.path().join("system/bios/NEW");
    fs::create_dir_all(&platform).unwrap();

    assert_eq!(bios::directory(source.path(), "NEW").unwrap(), Some(platform));
}

#[test]
fn invalid_platforms_are_rejected_before_reading() {
    for platform in ["", "/tmp", "../OTHER", "A/B", "lowercase", "_BAD", &"A".repeat(65)] {
        assert!(bios::directory(std::path::Path::new("/missing"), platform)
            .unwrap_err().to_string().contains("platform identifier"));
    }
}

#[test]
fn every_selected_directory_component_rejects_symlinks() {
    for relative in ["system", "system/bios", "system/bios/NEW"] {
        let source = tempfile::tempdir().unwrap();
        fs::create_dir_all(source.path().join("system/bios/NEW")).unwrap();
        let original = source.path().join(relative);
        let moved = original.with_extension("real");
        fs::rename(&original, &moved).unwrap();
        symlink(&moved, &original).unwrap();

        let error = bios::directory(source.path(), "NEW").unwrap_err();

        assert!(error.to_string().starts_with("Cannot read BIOS:"), "{relative}: {error}");
    }
}

#[test]
fn file_or_fifo_in_place_of_directory_fails_without_blocking() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    for kind in ["file", "fifo"] {
        let source = tempfile::tempdir().unwrap();
        fs::create_dir_all(source.path().join("system/bios")).unwrap();
        let path = source.path().join("system/bios/NEW");
        if kind == "file" {
            fs::write(&path, b"not a directory").unwrap();
        } else {
            let name = CString::new(path.as_os_str().as_bytes()).unwrap();
            // SAFETY: a NUL-terminated filename inside a synthetic fixture.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        }

        assert!(bios::directory(source.path(), "NEW").is_err(), "{kind}");
    }
}
