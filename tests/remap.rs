// SPDX-License-Identifier: BSD-3-Clause
use std::{fs, os::unix::fs::{symlink, PermissionsExt}, path::Path};

use iman::{runtime::paths, emulation::remap::{Profile, Remap}, session};
use serde_json::json;

#[test]
fn remap_serializes_standard_buttons_axes_ports_keyboard_and_turbo_without_policy() {
    let remap: Remap = serde_json::from_value(json!({
        "input_player1_btn_x": 10,
        "input_player1_btn_y": 11,
        "input_player2_btn_b": -1,
        "input_player1_stk_l_x+": 4,
        "input_player1_stk_l_y-": 5,
        "input_player1_key_a": 32,
        "input_remap_port_p1": 1,
        "input_libretro_device_p2": 1,
        "input_player1_analog_dpad_mode": 1,
        "input_turbo_enable": true,
        "future_input_setting": "enabled"
    })).unwrap();

    let text = remap.config().unwrap();

    assert!(!remap.is_empty());
    assert_eq!(text.lines().count(), 11);
    for line in [
        "input_player1_btn_x = \"10\"",
        "input_player1_btn_y = \"11\"",
        "input_player2_btn_b = \"-1\"",
        "input_player1_stk_l_x+ = \"4\"",
        "input_player1_stk_l_y- = \"5\"",
        "input_turbo_enable = \"true\"",
    ] {
        assert!(text.lines().any(|actual| actual == line), "{line}");
    }
    assert!(!text.contains("save_on_exit"));
}

#[test]
fn malformed_remaps_and_config_injection_are_rejected() {
    for (value, expected) in [
        (json!(null), "unique RetroArch settings"),
        (json!([]), "unique RetroArch settings"),
        (json!(3), "unique RetroArch settings"),
        (json!({"nested": {}}), "untagged enum Value"),
        (json!({"array": [1]}), "untagged enum Value"),
        (json!({"null": null}), "untagged enum Value"),
        (json!({"": 1}), "invalid RetroArch setting name"),
        (json!({"bad name": 1}), "invalid RetroArch setting name"),
        (json!({"bad=name": 1}), "invalid RetroArch setting name"),
        (json!({"bad-name": 1}), "invalid RetroArch setting name"),
        (json!({"input_player1_stk_l_x++": 1}), "invalid RetroArch setting name"),
        (json!({"+": 1}), "invalid RetroArch setting name"),
        (json!({"bad\nname": 1}), "invalid RetroArch setting name"),
        (json!({"key": "bad\\path"}), "unsafe RetroArch string value"),
        (json!({"key": "value\"\nconfig_save_on_exit=true"}), "unsafe RetroArch string value"),
        (json!({"key": "x".repeat(16 * 1024 + 1)}), "unsafe RetroArch string value"),
        (json!({"x".repeat(129): 1}), "invalid RetroArch setting name"),
    ] {
        let error = serde_json::from_value::<Remap>(value.clone()).unwrap_err();

        assert!(error.is_data(), "{value}: {error}");
        assert!(error.to_string().contains(expected), "{value}: {error}");
    }
    let error = serde_json::from_str::<Remap>(r#"{"input_player1_btn_x":10,"input_player1_btn_x":11}"#).unwrap_err();
    assert!(error.to_string().contains("duplicate RetroArch setting"));
}

#[test]
fn empty_remap_has_no_entries() {
    for remap in [Remap::default(), serde_json::from_str("{}").unwrap()] {
        assert!(remap.is_empty());
        assert!(remap.config().unwrap().is_empty());
    }
}

#[test]
fn unsafe_library_names_are_rejected_before_creating_files() {
    let remap: Remap = serde_json::from_value(json!({"input_player1_btn_x":10})).unwrap();
    for name in ["", ".", "..", "../escape", "/absolute", "a/b", "a\\b", "a\n", "a\"", " padded", "padded "] {
        assert!(Profile::new(name, &remap).unwrap_err().to_string().contains("library_name"));
    }
    assert!(Profile::new(&"a".repeat(129), &remap).is_err());
}

#[test]
fn profile_creates_private_standard_core_remap_and_refuses_overwrite() {
    let runtime = session::runtime_directory(Path::new(paths::RUNTIME_ROOT)).unwrap();
    let remap: Remap = serde_json::from_value(json!({"input_player1_btn_x":10})).unwrap();
    let profile = Profile::new("Synthetic Core (1)", &remap).unwrap();
    let path = runtime.path().join("remaps/Synthetic Core (1)/Synthetic Core (1).rmp");

    profile.prepare(runtime.path()).unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), "input_player1_btn_x = \"10\"\n");
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(profile.prepare(runtime.path()).unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read_to_string(&path).unwrap(), remap.config().unwrap());
}

#[test]
fn symlinked_remap_directories_are_not_followed_and_missing_parent_is_not_created() {
    let runtime = session::runtime_directory(Path::new(paths::RUNTIME_ROOT)).unwrap();
    let outside = tempfile::tempdir().unwrap();
    let profile = Profile::new("Synthetic", &Remap::default()).unwrap();
    let base = runtime.path().join("remaps");
    fs::remove_dir(&base).unwrap();

    assert!(profile.prepare(runtime.path()).is_err());
    assert!(!base.exists());
    symlink(outside.path(), &base).unwrap();
    assert!(profile.prepare(runtime.path()).is_err());
    fs::remove_file(&base).unwrap();
    fs::create_dir(&base).unwrap();
    symlink(outside.path(), base.join("Synthetic")).unwrap();
    assert!(profile.prepare(runtime.path()).is_err());
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}
