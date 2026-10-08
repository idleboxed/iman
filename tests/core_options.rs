// SPDX-License-Identifier: BSD-3-Clause
use std::{fs, os::unix::fs::{symlink, MetadataExt, PermissionsExt}, path::Path};

use iman::{emulation::core_options::CoreOptions, runtime::paths, session};
use serde_json::json;

#[test]
fn serializes_hyphenated_core_keys_and_preserves_case_sensitive_string_values() {
    let options: CoreOptions = serde_json::from_value(json!({
        "mupen64plus-43screensize": "320x240",
        "mupen64plus-EnableTextureCache": "False",
        "other_core-option": "Enabled (original)",
    })).unwrap();

    let config = options.config().unwrap();

    assert_eq!(config, concat!(
        "mupen64plus-43screensize = \"320x240\"\n",
        "mupen64plus-EnableTextureCache = \"False\"\n",
        "other_core-option = \"Enabled (original)\"\n",
    ));
    assert!(!config.contains("core_options_path"));
}

#[test]
fn rejects_non_string_values_unsafe_names_and_config_injection() {
    for (value, expected) in [
        (json!(null), "unique RetroArch settings"),
        (json!([]), "unique RetroArch settings"),
        (json!(true), "unique RetroArch settings"),
        (json!({"option": true}), "values must be strings"),
        (json!({"option": 1}), "values must be strings"),
        (json!({"option": null}), "untagged enum Value"),
        (json!({"option": []}), "untagged enum Value"),
        (json!({"": "yes"}), "invalid core option name"),
        (json!({"-only": "yes"}), "invalid core option name"),
        (json!({"bad name": "yes"}), "invalid core option name"),
        (json!({"bad/name": "yes"}), "invalid core option name"),
        (json!({"bad.name": "yes"}), "invalid core option name"),
        (json!({"bad=name": "yes"}), "invalid core option name"),
        (json!({"bad\nname": "yes"}), "invalid core option name"),
        (json!({"x".repeat(129): "yes"}), "invalid core option name"),
        (json!({"option": "quote\""}), "unsafe RetroArch string value"),
        (json!({"option": "line\nnext"}), "unsafe RetroArch string value"),
        (json!({"option": "tab\t"}), "unsafe RetroArch string value"),
        (json!({"option": "slash\\"}), "unsafe RetroArch string value"),
        (json!({"option": "x".repeat(16 * 1024 + 1)}), "unsafe RetroArch string value"),
    ] {
        let error = serde_json::from_value::<CoreOptions>(value.clone()).unwrap_err();

        assert!(error.is_data(), "{value}: {error}");
        assert!(error.to_string().contains(expected), "{value}: {error}");
    }
}

#[test]
fn duplicate_option_keys_are_refused_instead_of_silently_taking_the_last_value() {
    let error = serde_json::from_str::<CoreOptions>(
        r#"{"mupen64plus-aspect":"4:3","mupen64plus-aspect":"16:9"}"#,
    ).unwrap_err();

    assert!(error.to_string().contains("duplicate RetroArch setting"));
}

#[test]
fn option_count_and_serialized_size_limits_accept_boundaries_and_reject_overflow() {
    let mut entries = serde_json::Map::new();
    for index in 0..128 {
        entries.insert(format!("option-{index}"), json!("enabled"));
    }
    let options: CoreOptions = serde_json::from_value(json!(entries)).unwrap();
    assert_eq!(options.config().unwrap().lines().count(), 128);
    entries.insert("extra-option".into(), json!("enabled"));
    assert!(serde_json::from_value::<CoreOptions>(json!(entries)).unwrap_err().to_string().contains("too many"));

    let options: CoreOptions = serde_json::from_value(json!({
        "a": "x".repeat(16 * 1024), "b": "x".repeat(16 * 1024 - 14),
    })).unwrap();
    assert_eq!(options.config().unwrap().len(), 32 * 1024);
    let error = serde_json::from_value::<CoreOptions>(json!({
        "a": "x".repeat(16 * 1024), "b": "x".repeat(16 * 1024 - 13),
    })).unwrap_err();
    assert!(error.to_string().contains("exceeds 32 KiB"));
}

#[test]
fn empty_options_produce_a_fresh_empty_ram_file_without_frontend_settings() {
    let runtime = session::runtime_directory(Path::new(paths::RUNTIME_ROOT)).unwrap();
    let options = CoreOptions::default();

    options.prepare(runtime.path()).unwrap();

    assert!(options.config().unwrap().is_empty());
    assert_eq!(fs::read(runtime.path().join(paths::CORE_OPTIONS_FILE)).unwrap(), b"");
}

#[test]
fn writer_creates_private_ram_options_and_refuses_overwrite_without_modifying_existing_data() {
    let runtime = session::runtime_directory(Path::new(paths::RUNTIME_ROOT)).unwrap();
    let options: CoreOptions = serde_json::from_value(json!({"core-resolution": "320x240"})).unwrap();
    let path = runtime.path().join(paths::CORE_OPTIONS_FILE);

    options.prepare(runtime.path()).unwrap();

    let before = fs::metadata(&path).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "core-resolution = \"320x240\"\n");
    assert_eq!(before.permissions().mode() & 0o777, 0o600);
    assert_eq!(before.nlink(), 1);
    assert_eq!(options.prepare(runtime.path()).unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(fs::metadata(&path).unwrap().ino(), before.ino());
    assert_eq!(fs::read_to_string(&path).unwrap(), options.config().unwrap());
}

#[test]
fn existing_symlink_hardlink_directory_and_special_file_are_never_followed_or_replaced() {
    for kind in ["symlink", "hardlink", "directory", "fifo"] {
        let runtime = session::runtime_directory(Path::new(paths::RUNTIME_ROOT)).unwrap();
        let outside = tempfile::tempdir_in(paths::RUNTIME_ROOT).unwrap();
        let source = outside.path().join("unchanged");
        fs::write(&source, b"unchanged synthetic data").unwrap();
        let path = runtime.path().join(paths::CORE_OPTIONS_FILE);
        match kind {
            "symlink" => symlink(&source, &path).unwrap(),
            "hardlink" => fs::hard_link(&source, &path).unwrap(),
            "directory" => fs::create_dir(&path).unwrap(),
            "fifo" => {
                let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            }
            _ => unreachable!(),
        }
        let before = fs::symlink_metadata(&path).unwrap();

        assert_eq!(CoreOptions::default().prepare(runtime.path()).unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);

        let after = fs::symlink_metadata(&path).unwrap();
        assert_eq!(after.ino(), before.ino(), "{kind}");
        assert_eq!(after.nlink(), before.nlink(), "{kind}");
        assert_eq!(fs::read(&source).unwrap(), b"unchanged synthetic data", "{kind}");
    }
}

#[test]
fn missing_or_symlinked_runtime_ancestry_is_refused_without_creating_paths() {
    let runtime = session::runtime_directory(Path::new(paths::RUNTIME_ROOT)).unwrap();
    let holder = tempfile::tempdir_in(paths::RUNTIME_ROOT).unwrap();
    let missing = holder.path().join("missing");
    let alias = holder.path().join("alias");
    symlink(runtime.path(), &alias).unwrap();

    assert!(CoreOptions::default().prepare(&missing).is_err());
    assert!(CoreOptions::default().prepare(&alias).is_err());
    assert!(CoreOptions::default().prepare(&alias.join("content")).is_err());

    assert!(!missing.exists());
    assert!(!runtime.path().join(paths::CORE_OPTIONS_FILE).exists());
    assert!(!runtime.path().join("content").join(paths::CORE_OPTIONS_FILE).exists());
}

#[test]
fn disk_backed_runtime_is_refused_before_options_creation() {
    let directory = tempfile::tempdir_in("/tmp").unwrap();
    let name = std::ffi::CString::new(directory.path().as_os_str().as_encoded_bytes()).unwrap();
    let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
    assert_eq!(unsafe { libc::statfs(name.as_ptr(), info.as_mut_ptr()) }, 0);
    if unsafe { info.assume_init() }.f_type as u64 != libc::TMPFS_MAGIC as u64 {
        let error = CoreOptions::default().prepare(directory.path()).unwrap_err();

        assert!(error.to_string().contains("tmpfs"));
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    }
}
