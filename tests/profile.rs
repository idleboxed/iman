// SPDX-License-Identifier: BSD-3-Clause
//! Profile parsing and argument/config generation; no real devices.
use iman::profile::Profile;
use serde_json::{json, Value};
use std::{fs, os::unix::fs::symlink, path::Path, process::Command};

fn load(value: Value) -> (tempfile::TempDir, Profile) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("iman.json");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let profile = Profile::load(&path).unwrap();
    (dir, profile)
}

#[test]
fn minimal_profile_has_no_public_preference_policy_flag() {
    let (_dir, profile) = load(json!({}));

    assert_eq!(profile.runtime_dir(), Path::new("/dev/shm"));
    assert!(profile.gui_arguments().is_empty());
}

#[test]
fn drm_selection_and_input_paths_are_explicit_and_cwd_independent() {
    let (dir, profile) = load(json!({
        "version":1, "runtime_dir":"/run/user/1000",
        "gui": {"display":{"backend":"drm", "device":"/dev/dri/card2", "connector":91},
                "hid_profile":"pads/hid.json", "evdev_profile":"/opt/evdev.json"}
    }));

    let args = profile.gui_arguments();
    assert_eq!(profile.runtime_dir(), Path::new("/run/user/1000"));
    assert!(args
        .windows(2)
        .any(|v| v == ["--drm-device", "/dev/dri/card2"]));
    assert!(args.windows(2).any(|v| v == ["--drm-connector", "91"]));
    assert!(args.contains(&dir.path().join("pads/hid.json").into_os_string()));
    assert!(args.contains(&"/opt/evdev.json".into()));
    assert!(!args.contains(&"--frames".into()));
    assert!(!args.contains(&"--allow-vendor-einval".into()));
}

#[test]
fn display_quirk_needs_no_public_preference_policy_flag() {
    let (_dir, profile) = load(json!({"version":1,"gui":{
        "frames":19000,"display":{
            "backend":"drm","allow_vendor_einval":true
        }
    }}));

    let args = profile.gui_arguments();
    for value in ["--allow-vendor-einval", "19000"] {
        assert!(args.contains(&value.into()));
    }
}

#[test]
fn invalid_profiles_are_rejected_before_resources_are_acquired() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("iman.json");
    for (value, expected) in [
        (json!({"runtime_dir":"relative"}), "expected absolute runtime_dir"),
        (json!({"gui":{"frames":0}}), "frames must be positive"),
        (json!({"gui":{"display":{"backend":"headless"},"frames":1}}), "headless profile forbids"),
        (json!({"gui":{"display":{"backend":"headless"},"hid_profile":"pads.json"}}), "headless profile forbids"),
        (json!({"gui":{"display":{"backend":"drm","connector":0}}}), "DRM requires an absolute device and a positive connector ID"),
        (json!({"gui":{"display":{"backend":"drm","device":"relative"}}}), "DRM requires an absolute device and a positive connector ID"),
        (json!({"gui":{"hid_profile":""}}), "input profile path is empty"),
    ] {
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();

        let error = Profile::load(&path).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::Other, "{value}: {error}");
        assert!(error.to_string().contains(expected), "{value}: {error}");
    }

    fs::write(&path, b"{\"runtime_dir\":\"/tmp\",\"runtime_dir\":\"/tmp\"}").unwrap();
    let error = Profile::load(&path).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("duplicate field `runtime_dir`"));

    fs::write(&path, vec![b' '; 65537]).unwrap();
    let error = Profile::load(&path).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Other);
    assert!(error.to_string().contains("64 KiB"));

    symlink(&path, dir.path().join("link")).unwrap();
    assert_eq!(Profile::load(&dir.path().join("link")).unwrap_err().raw_os_error(), Some(libc::ELOOP));
    let error = Profile::load(dir.path()).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Other);
    assert!(error.to_string().contains("regular file"));
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
}

#[test]
fn profile_ignores_unknown_fields_at_every_level() {
    let (_dir, profile) = load(json!({
        "version": 999,
        "future": true,
        "gui": {
            "future": true,
            "display": {"backend": "drm", "future": true}
        }
    }));

    assert!(matches!(profile.display(), iman::profile::Display::Drm { .. }));
}

#[test]
fn profile_and_raw_gui_arguments_cannot_override_each_other() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_iman"))
        .args(["--future-mode", "opaque", "--config", "missing.json", "--", "--drm"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("raw IGUI"));
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn unknown_startup_tokens_are_ignored_with_one_warning_and_no_value_echo() {
    let dir = tempfile::tempdir_in("/dev/shm").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_iman"))
        .args(["--future-mode", "opaque-a,opaque-b", "-Z", "--future-key=private-value", "--help"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("Usage: iman"));
    assert!(help.contains("no CPU-specific launch flag is required"));
    assert!(!help.contains("--box2-diagnostic"));
    assert_eq!(String::from_utf8_lossy(&output.stderr),
        "iman: warning: ignoring unrecognized startup arguments before --\n");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn non_utf8_unknown_startup_token_is_ignored_without_panicking() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let dir = tempfile::tempdir_in("/dev/shm").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_iman"))
        .arg(OsString::from_vec(b"--opaque=\xff".to_vec()))
        .arg("--help")
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("ignoring unrecognized"));
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn unknown_startup_tokens_do_not_hide_missing_or_duplicate_known_options() {
    let dir = tempfile::tempdir_in("/dev/shm").unwrap();
    let cases: &[(&[&str], &str)] = &[
        (&["--root"], "--root needs a directory"),
        (&["--config"], "--config needs a file"),
        (&["--root", "/unused", "--future", "opaque", "--root", "/other"], "duplicate option: --root"),
        (&["--config", "missing.json", "--future", "opaque", "--config", "other.json"], "duplicate option: --config"),
    ];
    for (args, error) in cases {
        let output = Command::new(env!("CARGO_BIN_EXE_iman"))
            .args(["--future-mode", "opaque"])
            .args(*args)
            .current_dir(dir.path())
            .output()
            .unwrap();

        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(error), "{args:?}");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}

#[test]
fn unknown_startup_tokens_do_not_bypass_profile_validation() {
    let dir = tempfile::tempdir_in("/dev/shm").unwrap();
    let profile = dir.path().join("iman.json");
    fs::write(&profile, br#"{"runtime_dir":"relative"}"#).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_iman"))
        .args(["--future-mode", "opaque", "--root"])
        .arg(dir.path())
        .arg("--config").arg(&profile)
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("runtime_dir"));
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}






#[test]
fn unmanaged_display_owners_never_acquire_hardware() {
    use iman::{display, profile::Display};
    for settings in [Display::Desktop, Display::Headless] {
        if cfg!(feature = "pc") && matches!(settings, Display::Desktop) {
            continue; // Window adapter is tested with a fake backend, never live X11.
        }
        let mut lease = display::open(&settings, None).unwrap();
        assert!(lease.monitor_index().is_none());
        lease.lend().unwrap();
        lease.reclaim().unwrap();
        lease.finish().unwrap();
    }
}

#[test]
fn mismatched_drm_game_context_is_rejected_before_opening_a_device() {
    use iman::{display, profile::Display};
    let settings = Display::Drm {
        device: "/must-not-open/card0".into(),
        connector: None,
        allow_vendor_einval: false,
    };
    let result = display::open(&settings, Some(Some("x")));
    let error = match result {
        Ok(_) => panic!("invalid context accepted"),
        Err(e) => e,
    };
    #[cfg(feature = "drm")]
    assert!(error.to_string().contains("kms context"));
    #[cfg(not(feature = "drm"))]
    assert!(error.to_string().contains("drm build feature"));
}

#[test]
fn former_board_argument_is_just_an_unknown_token() {
    let dir = tempfile::tempdir_in("/dev/shm").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_iman"))
        .args(["--box2-diagnostic", "--future-mode", "--help"])
        .current_dir(dir.path()).output().unwrap();

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage: iman"));
    assert_eq!(String::from_utf8_lossy(&output.stderr),
        "iman: warning: ignoring unrecognized startup arguments before --\n");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn warnings_before_initialization_never_write_an_inherited_regular_file() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let path = root.path().join("inherited.log");
    fs::write(&path, b"keep existing data").unwrap();
    let stderr = fs::OpenOptions::new().append(true).open(&path).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_iman"))
        .args(["--box2-diagnostic", "--future", "--help"])
        .stderr(stderr).output().unwrap();

    assert!(output.status.success());
    assert_eq!(fs::read(&path).unwrap(), b"keep existing data");
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage: iman"));
}
