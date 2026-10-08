use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::Path,
};

use iman::emulation::launcher;
use imanlib::igui::LaunchRequest;
use serde_json::json;
use tempfile::TempDir;

#[path = "common/bundle.rs"]
mod bundle_fixture;
#[path = "common/settings.rs"]
mod settings_fixture;

fn fixture() -> (TempDir, LaunchRequest) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    bundle_fixture::create_bundle_layout(root);
    fs::write(root.join("system/retroarch/retroarch"), b"synthetic frontend").unwrap();
    fs::set_permissions(
        root.join("system/retroarch/retroarch"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fs::write(root.join("system/retroarch/cores/core.so"), b"synthetic core").unwrap();
    fs::write(
        root.join("games/images/NES/ab/Game ' one.nes"),
        b"synthetic ROM",
    )
    .unwrap();
    let component = |path: &str| json!({"path": path});
    fs::write(
        root.join("system/iman/emulation.json"),
        serde_json::to_vec(&json!({
            "architecture": std::env::consts::ARCH,
            "retroarch": {"path": "retroarch/retroarch", "future": true},
            "cores": {"NES": component("retroarch/cores/core.so")},
            "settings": {"microphone_enable": false, "user_language": 0},
            "future": true
        }))
        .unwrap(),
    )
    .unwrap();
    (
        temp,
        LaunchRequest {
            hash: "ab".repeat(32),
            image: "NES/ab/Game ' one.nes".into(),
            platform: "NES".into(),
        },
    )
}

#[test]
fn manifest_reports_missing_required_fields() {
    let (root, request) = fixture();
    let path = root.path().join("system/iman/emulation.json");
    for (document, field) in [
        (json!({"retroarch": {"path": "retroarch/retroarch"}, "cores": {}}), "architecture"),
        (json!({"architecture": std::env::consts::ARCH, "cores": {}}), "retroarch"),
        (json!({"architecture": std::env::consts::ARCH, "retroarch": {"path": "retroarch/retroarch"}}), "cores"),
        (json!({"architecture": std::env::consts::ARCH, "retroarch": {"path": "retroarch/retroarch"}, "cores": {}}), "settings"),
    ] {
        fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        let error = launcher::resolve(root.path(), &request).unwrap_err();
        assert!(error.to_string().contains(field), "{error}");
    }
}

#[test]
fn resolves_selected_rom_and_keeps_spaces_in_one_argument() {
    let (root, request) = fixture();

    let plan = launcher::resolve(root.path(), &request).unwrap();
    let command = launcher::command(&plan, Path::new("/dev/shm/session with spaces"));

    assert_eq!(
        plan.rom,
        root.path().join("games/images").join(request.image)
    );
    let args: Vec<_> = command.get_args().collect();
    assert_eq!(
        args[args.len() - 1],
        launcher::content_path(&plan, Path::new("/dev/shm/session with spaces"))
    );
    assert!(args.contains(&std::ffi::OsStr::new("load-save")));
    assert_eq!(command.get_program(), plan.retroarch);
    assert_eq!(
        command.get_current_dir().unwrap(),
        Path::new("/dev/shm/session with spaces")
    );
}

#[test]
fn rejects_unknown_platform_missing_rom_traversal_and_wrong_hash() {
    let (root, request) = fixture();
    for (image, platform, hash, expected) in [
        (
            request.image.as_str(),
            "OTHER",
            request.hash.as_str(),
            "no core",
        ),
        ("missing.nes", "NES", request.hash.as_str(), "No such file"),
        (
            "../outside.nes",
            "NES",
            request.hash.as_str(),
            "invalid relative path",
        ),
        (
            "/absolute.nes",
            "NES",
            request.hash.as_str(),
            "invalid relative path",
        ),
        (request.image.as_str(), "NES", "bad", "invalid game hash"),
    ] {
        let changed = LaunchRequest {
            image: image.into(),
            platform: platform.into(),
            hash: hash.into(),
        };

        let error = launcher::resolve(root.path(), &changed).unwrap_err();

        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn refuses_symlink_escape_but_accepts_updated_components_without_hash_pins() {
    let (root, mut request) = fixture();
    fs::write(root.path().join("outside.nes"), b"synthetic").unwrap();
    symlink(
        root.path().join("outside.nes"),
        root.path().join("games/images/escape.nes"),
    )
    .unwrap();
    request.image = "escape.nes".into();

    assert!(launcher::resolve(root.path(), &request)
        .unwrap_err()
        .to_string()
        .contains("escapes root"));

    request.image = "NES/ab/Game ' one.nes".into();
    fs::write(root.path().join("system/retroarch/cores/core.so"), b"changed").unwrap();

    assert!(launcher::resolve(root.path(), &request).is_ok());
}

#[test]
fn refuses_wrong_architecture_and_nonexecutable_frontend() {
    let (root, request) = fixture();
    let manifest_path = root.path().join("system/iman/emulation.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["architecture"] = json!("wrong");
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();

    assert!(launcher::resolve(root.path(), &request)
        .unwrap_err()
        .to_string()
        .contains("architecture"));

    manifest["architecture"] = json!(std::env::consts::ARCH);
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    fs::set_permissions(
        root.path().join("system/retroarch/retroarch"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();

    assert!(launcher::resolve(root.path(), &request)
        .unwrap_err()
        .to_string()
        .contains("not executable"));
}

#[test]
fn config_uses_persistent_game_saves_and_temporary_auxiliary_paths() {
    let (root, request) = fixture();
    let plan = launcher::resolve(root.path(), &request).unwrap();

    let config = launcher::config(&plan, Path::new("/dev/shm/test")).unwrap();

    for key in [
        "history_list_enable",
        "config_save_on_exit",
        "savestate_auto_save",
        "savestate_auto_load",
    ] {
        assert!(config.contains(&format!("{key} = \"false\"")));
    }
    assert!(config.contains(&format!("savefile_directory = \"{}\"", root.path().join("games/saves").display())));

    assert!(config.contains("system_directory = \"/dev/shm/test/bios\""));
    assert!(!config.contains(root.path().join("system/bios").to_str().unwrap()));
    assert!(config.contains("microphone_enable = \"false\""));
    assert!(config.contains("user_language = \"0\""));
    assert!(launcher::config(&plan, Path::new("/tmp/injected\"\nvalue")).is_err());
}

#[test]
fn non_tmpfs_runtime_is_refused_without_creating_files() {
    let root = tempfile::tempdir().unwrap();
    // The test runner uses an on-disk /tmp in this project. Do not assume that
    // property on other Linux hosts; in-memory directories are valid there.
    match iman::session::runtime_directory(root.path()) {
        Ok(runtime) => {
            runtime.close().unwrap();
        }
        Err(error) => assert!(error.to_string().contains("tmpfs")),
    }
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn persistent_slots_use_only_rom_hash_not_filename_or_core_version() {
    let (root, mut request) = fixture();
    let first = launcher::resolve(root.path(), &request).unwrap();
    assert_eq!(
        first.state,
        root.path()
            .join("games/states")
            .join(format!("{}.state", request.hash))
    );
    let command = launcher::command(&first, Path::new("/dev/shm/session"));
    let args: Vec<_> = command.get_args().collect();
    assert!(!args.contains(&std::ffi::OsStr::new("--savestate")));
    assert_eq!(
        Path::new(args.last().unwrap()).file_stem(),
        Some(std::ffi::OsStr::new(&request.hash))
    );
    let config = launcher::config(&first, Path::new("/dev/shm/session")).unwrap();
    assert!(config.contains(&format!(
        "savestate_directory = \"{}\"",
        root.path().join("games/states").display()
    )));
    assert!(config.contains("savestate_auto_index = \"false\""));
    assert!(config.contains("savestate_thumbnail_enable = \"false\""));
    assert_eq!(fs::read_dir(root.path().join("games/states")).unwrap().count(), 0);

    fs::rename(&first.rom, first.rom.parent().unwrap().join("renamed.nes")).unwrap();
    request.image = "NES/ab/renamed.nes".into();
    fs::write(&first.core, b"new synthetic core version").unwrap();
    assert_eq!(
        launcher::resolve(root.path(), &request).unwrap().state,
        first.state
    );
    request.hash = "cd".repeat(32);
    assert_ne!(
        launcher::resolve(root.path(), &request).unwrap().state,
        first.state
    );
}

#[test]
fn stable_content_alias_is_ram_only_never_overwrites_or_copies_the_rom() {
    let (root, request) = fixture();
    let plan = launcher::resolve(root.path(), &request).unwrap();
    let original = fs::read(&plan.rom).unwrap();
    let runtime = iman::session::runtime_directory(Path::new("/dev/shm")).unwrap();
    let alias = launcher::content_path(&plan, runtime.path());

    launcher::prepare_content(&plan, runtime.path()).unwrap();

    assert!(fs::symlink_metadata(&alias)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        alias.file_name().unwrap(),
        std::ffi::OsStr::new(&format!("{}.nes", request.hash))
    );
    assert_eq!(fs::read_link(&alias).unwrap(), plan.rom);
    assert_eq!(fs::read(&alias).unwrap(), original);
    assert_eq!(
        launcher::prepare_content(&plan, runtime.path())
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::AlreadyExists
    );
    runtime.close().unwrap();
    assert!(!alias.exists());
    assert_eq!(fs::read(&plan.rom).unwrap(), original);
    assert_eq!(fs::read_dir(root.path().join("games/states")).unwrap().count(), 0);
}

#[test]
fn state_preflight_refuses_missing_directory_and_symlink_slots_without_writing() {
    let (root, request) = fixture();
    let path = launcher::resolve(root.path(), &request).unwrap().state;
    fs::remove_dir(root.path().join("games/states")).unwrap();
    assert!(launcher::resolve(root.path(), &request)
        .unwrap_err()
        .to_string()
        .contains("prepare the states"));
    assert!(!root.path().join("games/states").exists());
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), root.path().join("games/states")).unwrap();
    assert!(launcher::resolve(root.path(), &request).is_err());
    fs::remove_file(root.path().join("games/states")).unwrap();
    fs::create_dir(root.path().join("games/states")).unwrap();
    for slot in [path.clone(), path.with_extension("state1")] {
        symlink(outside.path().join("untouched"), &slot).unwrap();
        assert!(launcher::resolve(root.path(), &request)
            .unwrap_err()
            .to_string()
            .contains("regular file"));
        assert!(!outside.path().join("untouched").exists());
        fs::remove_file(slot).unwrap();
    }
    let original = outside.path().join("original");
    fs::write(&original, b"must not be truncated").unwrap();
    fs::hard_link(&original, &path).unwrap();
    assert!(launcher::resolve(root.path(), &request).is_err());
    assert_eq!(fs::read(&original).unwrap(), b"must not be truncated");
}

#[test]
fn emulation_settings_generate_config_without_changing_paths() {
    let (root, request) = fixture();
    let path = root.path().join("system/iman/emulation.json");
    let mut document: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    document["settings"] = settings_fixture::create_settings();
    fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();

    let plan = launcher::resolve(root.path(), &request).unwrap();
    let config = launcher::config(&plan, Path::new("/dev/shm/test")).unwrap();

    assert!(config.contains("audio_driver = \"alsa\"\n"));
    assert!(config.contains("audio_latency = \"123\"\n"));
    assert!(!config.contains("audio_driver = \"pulse\""));
    assert!(config.contains("video_autoswitch_refresh_rate = \"2\"\n"));
    assert_eq!(config.matches("config_save_on_exit =").count(), 1);
    assert!(config.contains(&format!("savefile_directory = \"{}\"", root.path().join("games/saves").display())));
    assert!(config.contains(&format!(
        "joypad_autoconfig_dir = \"{}\"",
        root.path().join("system/retroarch/padconf").display(),
    )));
    assert_eq!(launcher::configured_video_context(root.path()).unwrap(), Some(Some("kms".into())));

    fs::write(root.path().join("system/retroarch/cores/core.so"), b"changed").unwrap();
    assert!(launcher::resolve(root.path(), &request).is_ok());
}

fn fds_fixture() -> (TempDir, LaunchRequest) {
    let (root, mut request) = fixture();
    let manifest = root.path().join("system/iman/emulation.json");
    let mut document: serde_json::Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    document["cores"]["FDS"] = document["cores"]["NES"].clone();
    fs::write(manifest, serde_json::to_vec(&document).unwrap()).unwrap();
    fs::create_dir_all(root.path().join("games/images/FDS/ab")).unwrap();
    fs::write(root.path().join("games/images/FDS/ab/Game.fds"), b"synthetic disk").unwrap();
    request.platform = "FDS".into();
    request.image = "FDS/ab/Game.fds".into();
    (root, request)
}

#[test]
fn missing_bios_is_left_to_the_core_for_fds_and_other_platforms() {
    let (root, request) = fds_fixture();

    assert!(launcher::resolve(root.path(), &request).unwrap().bios.is_none());
    assert!(!root.path().join("system/bios").exists());
    let nes = LaunchRequest { platform: "NES".into(), ..request };
    assert!(launcher::resolve(root.path(), &nes).unwrap().bios.is_none());
}

#[test]
fn fds_uses_sd_directory_without_copying_firmware_or_changing_save_policy() {
    let (root, request) = fds_fixture();
    let source = root.path().join("system/bios/FDS/disksys.rom");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let bytes = vec![0x5a; 8192]; // Synthetic firmware, not a playable BIOS.
    fs::write(&source, &bytes).unwrap();
    let before = fs::metadata(&source).unwrap().modified().unwrap();
    let plan = launcher::resolve(root.path(), &request).unwrap();
    let runtime = iman::session::runtime_directory(Path::new("/dev/shm")).unwrap();

    launcher::prepare_content(&plan, runtime.path()).unwrap();
    let config = launcher::config(&plan, runtime.path()).unwrap();

    assert!(config.contains(&format!("system_directory = \"{}\"", source.parent().unwrap().display())));
    assert_eq!(fs::read_dir(runtime.path().join("bios")).unwrap().count(), 0);
    assert_eq!(fs::read(&source).unwrap(), bytes);
    assert_eq!(fs::metadata(&source).unwrap().modified().unwrap(), before);
    assert!(config.contains(&format!("savefile_directory = \"{}\"", root.path().join("games/saves").display())));
    assert!(config.contains("savestate_auto_save = \"false\""));
    assert_eq!(launcher::content_path(&plan, runtime.path()).extension().unwrap(), "fds");
    assert_eq!(launcher::prepare_content(&plan, runtime.path()).unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
}

#[test]
fn bios_contents_and_exact_fds_size_are_left_to_the_core() {
    let (root, request) = fds_fixture();
    let source = root.path().join("system/bios/FDS/disksys.rom");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    for size in [0, 8191, 8192, 8193, 40976] {
        fs::write(&source, vec![0; size]).unwrap();

        let plan = launcher::resolve(root.path(), &request).unwrap();

        assert!(plan.bios.is_some());
        assert_eq!(fs::metadata(&source).unwrap().len(), size as u64);
    }
}

#[test]
fn fds_rejects_symlinks_in_each_bios_path_component() {
    for part in ["system/bios", "system/bios/FDS"] {
        let (root, request) = fds_fixture();
        let source = root.path().join("system/bios/FDS/disksys.rom");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, vec![0; 8192]).unwrap();
        let original = root.path().join(part);
        let moved = original.with_extension("moved");
        fs::rename(&original, &moved).unwrap();
        symlink(&moved, &original).unwrap();

        let error = launcher::resolve(root.path(), &request).unwrap_err();

        assert!(error.to_string().starts_with("Cannot read BIOS:"), "{part}: {error}");
    }
}

#[test]
fn arbitrary_platform_bios_is_loaded_without_a_firmware_manifest_or_name_list() {
    let (root, mut request) = fixture();
    let manifest = root.path().join("system/iman/emulation.json");
    let mut document: serde_json::Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    document["cores"]["NEW"] = document["cores"]["NES"].clone();
    fs::write(&manifest, serde_json::to_vec(&document).unwrap()).unwrap();
    request.platform = "NEW".into();
    fs::create_dir_all(root.path().join("system/bios/NEW/sub")).unwrap();
    fs::write(root.path().join("system/bios/NEW/sub/arbitrary-name.bin"), b"firmware").unwrap();

    let plan = launcher::resolve(root.path(), &request).unwrap();
    let runtime = iman::session::runtime_directory(Path::new("/dev/shm")).unwrap();
    launcher::prepare_content(&plan, runtime.path()).unwrap();

    let expected = root.path().join("system/bios/NEW");
    assert_eq!(plan.bios, Some(expected.clone()));
    assert!(launcher::config(&plan, runtime.path()).unwrap()
        .contains(&format!("system_directory = \"{}\"", expected.display())));
    assert_eq!(fs::read(expected.join("sub/arbitrary-name.bin")).unwrap(), b"firmware");
    assert_eq!(fs::read_dir(runtime.path().join("bios")).unwrap().count(), 0);
}

#[test]
fn remap_is_selected_by_manifest_platform_not_by_core_or_rom_extension() {
    let (root, mut request) = fixture();
    let manifest = root.path().join("system/iman/emulation.json");
    let mut document: serde_json::Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    document["cores"]["ANY_PLATFORM"] = json!({
        "path": "retroarch/cores/core.so", "library_name": "Any Core",
        "remap": {"input_player1_btn_x": 10, "input_player1_btn_y": 11}
    });
    document["settings"] = json!({
        "auto_remaps_enable": false, "input_remap_binds_enable": false,
        "input_remap_sort_by_controller_enable": true, "remap_save_on_exit": true,
        "input_remapping_directory": "/unsafe-sd/remaps"
    });
    fs::write(&manifest, serde_json::to_vec(&document).unwrap()).unwrap();
    let plain = launcher::resolve(root.path(), &request).unwrap();
    request.platform = "ANY_PLATFORM".into();

    let mapped = launcher::resolve(root.path(), &request).unwrap();
    let config = launcher::config(&mapped, Path::new("/dev/shm/isolated")).unwrap();

    assert_eq!(mapped.core, plain.core);
    assert!(plain.remap.is_none());
    assert!(mapped.remap.is_some());
    for (key, expected) in [
        ("auto_remaps_enable", "true"), ("input_remap_binds_enable", "true"),
        ("input_remap_sort_by_controller_enable", "false"), ("remap_save_on_exit", "false"),
        ("config_save_on_exit", "false"), ("auto_overrides_enable", "false"),
    ] {
        assert_eq!(config.matches(&format!("{key} =")).count(), 1);
        assert!(config.contains(&format!("{key} = \"{expected}\"\n")));
    }
    assert!(config.contains("input_remapping_directory = \"/dev/shm/isolated/remaps\""));
    let plain_config = launcher::config(&plain, Path::new("/dev/shm/isolated")).unwrap();
    assert!(plain_config.contains("auto_remaps_enable = \"false\""));
}

#[test]
fn absent_or_empty_remap_keeps_old_launch_without_requiring_library_name() {
    let (root, request) = fixture();
    let manifest = root.path().join("system/iman/emulation.json");
    let mut document: serde_json::Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    let original = launcher::config(&launcher::resolve(root.path(), &request).unwrap(), Path::new("/dev/shm/t")).unwrap();
    document["cores"]["NES"]["remap"] = json!({});
    fs::write(&manifest, serde_json::to_vec(&document).unwrap()).unwrap();

    let plan = launcher::resolve(root.path(), &request).unwrap();

    assert!(plan.remap.is_none());
    assert_eq!(launcher::config(&plan, Path::new("/dev/shm/t")).unwrap(), original);
}

#[test]
fn remap_without_library_name_or_with_escaping_name_is_refused_during_preflight() {
    let (root, request) = fixture();
    let manifest = root.path().join("system/iman/emulation.json");
    let mut document: serde_json::Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    document["cores"]["NES"]["remap"] = json!({"input_player1_btn_x": 10});
    for name in [json!(null), json!("../escape")] {
        document["cores"]["NES"]["library_name"] = name;
        fs::write(&manifest, serde_json::to_vec(&document).unwrap()).unwrap();

        assert!(launcher::resolve(root.path(), &request).unwrap_err().to_string().contains("library_name"));
    }
}

#[test]
fn computed_runtime_paths_cannot_be_shadowed_by_earlier_user_settings() {
    let (root, request) = fixture();
    let manifest = root.path().join("system/iman/emulation.json");
    let mut document: serde_json::Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    let keys = [
        "savefile_directory", "savestate_directory", "cache_directory", "screenshot_directory",
        "runtime_log_directory", "core_options_path", "input_remapping_directory",
        "system_directory", "libretro_directory", "joypad_autoconfig_dir",
    ];
    for key in keys {
        document["settings"][key] = json!("/unsafe-user-path");
    }
    fs::write(&manifest, serde_json::to_vec(&document).unwrap()).unwrap();

    let plan = launcher::resolve(root.path(), &request).unwrap();
    let config = launcher::config(&plan, Path::new("/dev/shm/session")).unwrap();

    for key in keys {
        assert_eq!(config.matches(&format!("{key} =")).count(), 1, "{key}");
    }
    assert!(!config.contains("/unsafe-user-path"));
    assert!(config.contains("input_remapping_directory = \"/dev/shm/session/remaps\"\n"));
}

#[test]
fn core_ram_system_directory_ignores_sd_bios_presence_and_user_path_overrides() {
    for present in [false, true] {
        let (root, mut request) = fixture();
        let manifest = root.path().join("system/iman/emulation.json");
        let mut document: serde_json::Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
        document["cores"]["N64"] = json!({
            "path": "retroarch/cores/core.so", "system_directory": "ram",
            "core_options": {"mupen64plus-43screensize": "320x240", "mupen64plus-EnableTextureCache": "False"},
        });
        document["settings"]["system_directory"] = json!("/unsafe-sd/bios");
        document["settings"]["core_options_path"] = json!("/unsafe-sd/options.cfg");
        document["settings"]["global_core_options"] = json!(false);
        fs::write(&manifest, serde_json::to_vec(&document).unwrap()).unwrap();
        request.platform = "N64".into();
        let source = root.path().join("system/bios/N64");
        if present {
            fs::create_dir_all(&source).unwrap();
            fs::write(source.join("unchanged.ini"), b"synthetic firmware data").unwrap();
        }
        let runtime = iman::session::runtime_directory(Path::new(iman::runtime::paths::RUNTIME_ROOT)).unwrap();

        let plan = launcher::resolve(root.path(), &request).unwrap();
        let config = launcher::config(&plan, runtime.path()).unwrap();
        plan.core_options.prepare(runtime.path()).unwrap();

        assert!(plan.bios.is_none());
        assert_eq!(config.matches("system_directory =").count(), 1);
        assert!(config.contains(&format!("system_directory = \"{}\"", runtime.path().join("bios").display())));
        assert!(config.contains(&format!("core_options_path = \"{}\"", runtime.path().join("core-options.cfg").display())));
        assert!(config.contains("global_core_options = \"true\""));
        assert!(!config.contains("/unsafe-sd"));
        assert!(!config.contains("mupen64plus-43screensize"));
        let options = fs::read_to_string(runtime.path().join("core-options.cfg")).unwrap();
        assert!(options.contains("mupen64plus-EnableTextureCache = \"False\""));
        if present {
            assert_eq!(fs::read(source.join("unchanged.ini")).unwrap(), b"synthetic firmware data");
            assert_eq!(fs::read_dir(&source).unwrap().count(), 1);
        } else {
            assert!(!source.exists());
        }
        assert_eq!(fs::read_dir(root.path().join("games/saves")).unwrap().count(), 0);
        assert_eq!(fs::read_dir(root.path().join("games/states")).unwrap().count(), 0);
    }
}

#[test]
fn ram_policy_does_not_open_or_follow_an_unused_sd_bios_tree() {
    let (root, request) = fixture();
    let manifest = root.path().join("system/iman/emulation.json");
    let mut document: serde_json::Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    document["cores"]["NES"]["system_directory"] = json!("ram");
    fs::write(&manifest, serde_json::to_vec(&document).unwrap()).unwrap();
    symlink("/unreadable-missing-synthetic-bios", root.path().join("system/bios")).unwrap();

    let plan = launcher::resolve(root.path(), &request).unwrap();

    assert!(plan.bios.is_none());
}

#[test]
fn explicit_bios_policy_retains_existing_guarded_paths_and_empty_options_default() {
    let (root, request) = fds_fixture();
    let source = root.path().join("system/bios/FDS");
    fs::create_dir_all(&source).unwrap();
    let manifest = root.path().join("system/iman/emulation.json");
    let mut document: serde_json::Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    document["cores"]["FDS"]["system_directory"] = json!("bios");
    fs::write(&manifest, serde_json::to_vec(&document).unwrap()).unwrap();

    let plan = launcher::resolve(root.path(), &request).unwrap();

    assert_eq!(plan.bios, Some(source));
    assert!(plan.core_options.config().unwrap().is_empty());
}

#[test]
fn invalid_system_policy_and_core_option_values_are_refused_during_preflight() {
    let (root, request) = fixture();
    let manifest = root.path().join("system/iman/emulation.json");
    let original: serde_json::Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    for fields in [
        json!({"system_directory": "sd"}), json!({"system_directory": "/tmp/injected"}),
        json!({"system_directory": null}), json!({"system_directory": true}),
        json!({"core_options": null}), json!({"core_options": {"mupen64plus-aspect": true}}),
        json!({"core_options": {"mupen64plus-aspect": "4:3\"\nconfig_save_on_exit=true"}}),
    ] {
        let mut document = original.clone();
        document["cores"]["NES"].as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
        fs::write(&manifest, serde_json::to_vec(&document).unwrap()).unwrap();

        assert!(launcher::resolve(root.path(), &request).is_err(), "{fields}");

        assert_eq!(fs::read_dir(root.path().join("games/saves")).unwrap().count(), 0);
        assert_eq!(fs::read_dir(root.path().join("games/states")).unwrap().count(), 0);
    }
}

#[test]
fn nested_state_directory_rejects_symlinked_games_without_touching_existing_slots() {
    let (root, request) = fixture();
    let state = root.path().join("games/states").join(format!("{}.state2", request.hash));
    fs::write(&state, b"previous state").unwrap();
    fs::rename(root.path().join("games"), root.path().join("original-games")).unwrap();
    symlink(root.path().join("original-games"), root.path().join("games")).unwrap();

    let error = launcher::state_path(root.path(), &request.hash).unwrap_err();

    assert!(error.to_string().contains("prepare the states"));
    assert_eq!(fs::read(&state).unwrap(), b"previous state");
}

#[test]
fn legacy_states_are_not_selected_or_migrated_implicitly() {
    let (root, request) = fixture();
    fs::create_dir(root.path().join("states")).unwrap();
    let old = root.path().join("states").join(format!("{}.state3", request.hash));
    fs::write(&old, b"legacy state").unwrap();
    fs::remove_dir(root.path().join("games/states")).unwrap();

    let error = launcher::state_path(root.path(), &request.hash).unwrap_err();

    assert!(error.to_string().contains("prepare the states"));
    assert_eq!(fs::read(&old).unwrap(), b"legacy state");
    assert!(!root.path().join("games/states").exists());
}

#[test]
fn save_ram_directory_is_required_without_fallback_or_launch_time_writes() {
    let (root, request) = fixture();
    fs::remove_dir(root.path().join("games/saves")).unwrap();

    let error = launcher::resolve(root.path(), &request).unwrap_err();

    assert!(error.to_string().contains("prepare the saves directory games/saves"));
    assert!(!root.path().join("games/saves").exists());
    assert!(!root.path().join("saves").exists());
    assert_eq!(fs::read_dir(root.path().join("games/states")).unwrap().count(), 0);
}

#[test]
fn save_ram_sram_rtc_and_parent_guards_preserve_existing_data() {
    let (root, request) = fixture();
    let external = root.path().join("original");
    fs::write(&external, b"previous progress").unwrap();
    for extension in ["srm", "rtc"] {
        let path = root.path().join("games/saves").join(format!("{}.{}", request.hash, extension));
        symlink(&external, &path).unwrap();
        assert!(launcher::resolve(root.path(), &request).is_err());
        fs::remove_file(&path).unwrap();
        fs::hard_link(&external, &path).unwrap();
        assert!(launcher::resolve(root.path(), &request).is_err());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(launcher::resolve(root.path(), &request).is_err());
        fs::remove_dir(&path).unwrap();
    }
    fs::rename(root.path().join("games"), root.path().join("original-games")).unwrap();
    symlink(root.path().join("original-games"), root.path().join("games")).unwrap();
    assert!(launcher::save_path(root.path(), &request.hash).is_err());
    assert_eq!(fs::read(external).unwrap(), b"previous progress");
}

#[test]
fn save_ram_uses_rom_hash_and_settings_cannot_redirect_or_sort_persistent_files() {
    let (root, mut request) = fixture();
    let first = launcher::save_path(root.path(), &request.hash).unwrap();
    fs::write(&first, b"previous SRAM").unwrap();
    fs::write(first.with_extension("rtc"), b"previous RTC").unwrap();
    let manifest = root.path().join("system/iman/emulation.json");
    let mut document: serde_json::Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    for key in ["savefiles_in_content_dir", "sort_savefiles_enable", "sort_savefiles_by_content_enable"] {
        document["settings"][key] = json!(true);
    }
    document["settings"]["savefile_directory"] = json!("/unsafe-location");
    document["settings"]["autosave_interval"] = json!(60);
    fs::write(&manifest, serde_json::to_vec(&document).unwrap()).unwrap();
    let original = launcher::resolve(root.path(), &request).unwrap();
    let moved = original.rom.parent().unwrap().join("renamed.nes");
    fs::rename(&original.rom, moved).unwrap();
    request.image = "NES/ab/renamed.nes".into();

    let plan = launcher::resolve(root.path(), &request).unwrap();
    let config = launcher::config(&plan, Path::new("/dev/shm/fixture")).unwrap();

    assert_eq!(first, root.path().join("games/saves").join(format!("{}.srm", request.hash)));
    assert_eq!(launcher::save_path(root.path(), &request.hash).unwrap(), first);
    assert_eq!(fs::read(&first).unwrap(), b"previous SRAM");
    assert_eq!(fs::read(first.with_extension("rtc")).unwrap(), b"previous RTC");
    for key in ["savefiles_in_content_dir", "sort_savefiles_enable", "sort_savefiles_by_content_enable"] {
        assert_eq!(config.matches(&format!("{key} =")).count(), 1);
        assert!(config.contains(&format!("{key} = \"false\"\n")));
    }
    assert!(config.contains("autosave_interval = \"60\"\n"));
    assert!(!config.contains("/unsafe-location"));
    request.hash = "cd".repeat(32);
    assert_ne!(launcher::save_path(root.path(), &request.hash).unwrap(), first);
}

#[test]
fn shader_preset_is_selected_by_platform_and_passed_as_one_absolute_argument() {
    let (root, mut request) = fixture();
    let manifest = root.path().join("system/iman/emulation.json");
    let mut document: serde_json::Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    let relative = "retroarch/shaders/presets/Custom ' platform.glslp";
    let preset = root.path().join("system").join(relative);
    fs::create_dir_all(preset.parent().unwrap()).unwrap();
    fs::write(&preset, b"#reference \"../synthetic.glslp\"\n").unwrap();
    let before = fs::metadata(&preset).unwrap().modified().unwrap();
    document["cores"]["OTHER"] = json!({"path": "retroarch/cores/core.so", "shader_preset": relative});
    document["settings"]["video_shader_enable"] = json!(false);
    document["settings"]["auto_shaders_enable"] = json!(true);
    fs::write(&manifest, serde_json::to_vec(&document).unwrap()).unwrap();
    let plain = launcher::resolve(root.path(), &request).unwrap();
    request.platform = "OTHER".into();

    let selected = launcher::resolve(root.path(), &request).unwrap();
    let command = launcher::command(&selected, Path::new("/dev/shm/session with spaces"));
    let config = launcher::config(&selected, Path::new("/dev/shm/session with spaces")).unwrap();

    assert_eq!(selected.rom, plain.rom);
    assert_eq!(selected.core, plain.core);
    assert!(plain.shader_preset.is_none());
    assert_eq!(selected.shader_preset.as_deref(), Some(preset.as_path()));
    let args: Vec<_> = command.get_args().collect();
    let index = args.iter().position(|arg| *arg == "--set-shader").unwrap();
    assert_eq!(args[index + 1], preset);
    assert_eq!(args[index + 2], "-L");
    assert_eq!(config.matches("video_shader_enable =").count(), 1);
    assert!(config.contains("video_shader_enable = \"true\"\n"));
    assert!(config.contains("auto_shaders_enable = \"false\"\n"));
    assert!(config.contains("config_save_on_exit = \"false\"\n"));
    assert_eq!(fs::metadata(&preset).unwrap().modified().unwrap(), before);
}

#[test]
fn missing_or_null_shader_preset_disables_shaders_even_with_user_enable_setting() {
    let (root, request) = fixture();
    let manifest = root.path().join("system/iman/emulation.json");
    let mut document: serde_json::Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    document["settings"]["video_shader_enable"] = json!(true);
    document["settings"]["auto_shaders_enable"] = json!(true);
    for explicit_null in [false, true] {
        if explicit_null {
            document["cores"]["NES"]["shader_preset"] = json!(null);
        }
        fs::write(&manifest, serde_json::to_vec(&document).unwrap()).unwrap();

        let plan = launcher::resolve(root.path(), &request).unwrap();
        let command = launcher::command(&plan, Path::new("/dev/shm/session"));
        let config = launcher::config(&plan, Path::new("/dev/shm/session")).unwrap();

        assert!(plan.shader_preset.is_none());
        assert!(config.contains("video_shader_enable = \"false\"\n"));
        assert_eq!(config.matches("video_shader_enable =").count(), 1);
        assert!(config.contains("auto_shaders_enable = \"false\"\n"));
        let args: Vec<_> = command.get_args().collect();
        let index = args.iter().position(|arg| *arg == "--set-shader").unwrap();
        assert_eq!(args[index + 1], "");
        assert!(!root.path().join("system/retroarch/shaders").exists());
    }
}

#[test]
fn invalid_shader_type_path_and_missing_file_fail_during_launch_preflight() {
    let (root, request) = fixture();
    let manifest = root.path().join("system/iman/emulation.json");
    let mut document: serde_json::Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    for value in [
        json!(true), json!(1), json!([]), json!({"path": "a.glslp"}),
        json!(""), json!("../outside.glslp"), json!("/absolute.glslp"),
        json!("retroarch/../outside.glslp"), json!("retroarch//a.glslp"),
        json!("retroarch/bad\nname.glslp"), json!("retroarch/bad\\name.glslp"),
        json!("retroarch/file.slangp"), json!("retroarch/file.glsl"), json!("retroarch/missing.glslp"),
    ] {
        document["cores"]["NES"]["shader_preset"] = value.clone();
        fs::write(&manifest, serde_json::to_vec(&document).unwrap()).unwrap();

        let error = launcher::resolve(root.path(), &request).unwrap_err();

        assert!(error.to_string().contains("shader") || error.to_string().contains("expected a string"), "{value}: {error}");
        assert!(!root.path().join("system/retroarch/shaders").exists());
    }
}

#[test]
fn shader_presets_reject_links_directories_special_files_and_oversized_files() {
    let (root, request) = fixture();
    let manifest = root.path().join("system/iman/emulation.json");
    let mut document: serde_json::Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    let relative = "retroarch/shaders/presets/NES.glslp";
    document["cores"]["NES"]["shader_preset"] = json!(relative);
    fs::write(&manifest, serde_json::to_vec(&document).unwrap()).unwrap();
    let preset = root.path().join("system").join(relative);
    fs::create_dir_all(preset.parent().unwrap()).unwrap();
    let original = root.path().join("original.glslp");
    fs::write(&original, b"synthetic preset").unwrap();

    symlink(&original, &preset).unwrap();
    assert!(launcher::resolve(root.path(), &request).is_err());
    fs::remove_file(&preset).unwrap();
    fs::hard_link(&original, &preset).unwrap();
    assert!(launcher::resolve(root.path(), &request).is_err());
    fs::remove_file(&preset).unwrap();
    fs::create_dir(&preset).unwrap();
    assert!(launcher::resolve(root.path(), &request).is_err());
    fs::remove_dir(&preset).unwrap();
    let name = std::ffi::CString::new(preset.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(launcher::resolve(root.path(), &request).is_err());
    fs::remove_file(&preset).unwrap();
    fs::File::create(&preset).unwrap().set_len(1024 * 1024 + 1).unwrap();
    assert!(launcher::resolve(root.path(), &request).unwrap_err().to_string().contains("size limit"));
    fs::write(&preset, b"synthetic preset").unwrap();
    for directory in ["system/retroarch/shaders/presets", "system/retroarch/shaders"] {
        let path = root.path().join(directory);
        let moved = path.with_extension("moved");
        fs::rename(&path, &moved).unwrap();
        symlink(&moved, &path).unwrap();
        assert!(launcher::resolve(root.path(), &request).is_err());
        fs::remove_file(&path).unwrap();
        fs::rename(&moved, &path).unwrap();
    }
    assert_eq!(fs::read(&original).unwrap(), b"synthetic preset");
}
