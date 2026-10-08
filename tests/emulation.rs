// SPDX-License-Identifier: BSD-3-Clause
//! Synthetic settings only; no shipped profiles, RetroArch or hardware.
use iman::emulation::Settings;
use serde_json::json;

#[path = "common/settings.rs"]
mod settings_fixture;

#[test]
fn driver_choices_and_sram_interval_are_preserved_without_persistent_paths() {
    for (video_context, audio_driver) in [("kms", "alsa"), ("x", "pulse")] {
        let mut document = settings_fixture::create_settings();
        document["video_context_driver"] = json!(video_context);
        document["audio_driver"] = json!(audio_driver);
        let settings: Settings = serde_json::from_value(document).unwrap();

        let config = settings.config().unwrap();

        assert!(config.contains(&format!("video_context_driver = \"{video_context}\"\n")));
        assert!(config.contains(&format!("audio_driver = \"{audio_driver}\"\n")));
        assert!(config.contains("config_save_on_exit = \"false\"\n"));
        assert!(config.contains("autosave_interval = \"60\"\n"));
        assert!(!config.contains("savefile_directory"));
    }
}

#[test]
fn explicit_player_indices_and_keyboard_bindings_are_preserved_without_merging() {
    let settings: Settings = serde_json::from_value(json!({
        "input_player1_joypad_index": 3,
        "input_player2_joypad_index": 1,
        "input_player1_a": "nul",
        "input_player2_a": "x",
    })).unwrap();

    let config = settings.config().unwrap();

    assert!(config.contains("input_player1_joypad_index = \"3\"\n"));
    assert!(config.contains("input_player2_joypad_index = \"1\"\n"));
    assert!(config.contains("input_player1_a = \"nul\"\n"));
    assert!(config.contains("input_player2_a = \"x\"\n"));
    assert!(!config.contains("input_player1_a_btn"));
    assert!(!config.contains("input_player2_a_btn"));
}

#[test]
fn absent_keyboard_bindings_are_not_added_implicitly() {
    let settings: Settings = serde_json::from_value(settings_fixture::create_settings()).unwrap();

    let config = settings.config().unwrap();

    for name in ["menu_toggle", "exit_emulator", "save_state", "load_state", "player1_a"] {
        assert!(!config.lines().any(|line| line.starts_with(&format!("input_{name} ="))));
    }
}

#[test]
fn arbitrary_retroarch_settings_and_joypad_players_are_serialized() {
    let settings: Settings = serde_json::from_value(json!({
        "input_player4_joypad_index": 3,
        "input_player4_a_btn": 61,
        "input_overlay_enable": true,
        "input_overlay": "/mnt/sdcard/overlays/My Pad.cfg",
        "custom_feature_from_future_retroarch": "enabled-value",
        "audio_volume": -6.5
    })).unwrap();
    let config = settings.config().unwrap();

    assert!(config.contains("input_player4_joypad_index = \"3\"\n"));
    assert!(config.contains("input_player4_a_btn = \"61\"\n"));
    assert!(config.contains("input_overlay_enable = \"true\"\n"));
    assert!(config.contains("input_overlay = \"/mnt/sdcard/overlays/My Pad.cfg\"\n"));
    assert!(config.contains("custom_feature_from_future_retroarch = \"enabled-value\"\n"));
    assert!(config.contains("audio_volume = \"-6.5\"\n"));
}

#[test]
fn manager_owned_safety_values_override_user_values_once() {
    let settings: Settings = serde_json::from_value(json!({
        "config_save_on_exit": true,
        "autosave_interval": 60,
        "history_list_enable": true,
        "menu_driver": "xmb"
    })).unwrap();
    let config = settings.config().unwrap();

    assert_eq!(config.matches("config_save_on_exit =").count(), 1);
    assert!(config.contains("config_save_on_exit = \"false\"\n"));
    assert_eq!(config.matches("autosave_interval =").count(), 1);
    assert!(config.contains("autosave_interval = \"60\"\n"));
    assert!(config.contains("menu_driver = \"xmb\"\n"));
}

#[test]
fn video_context_is_optional_but_exposed_for_the_display_guard() {
    let with_context: Settings = serde_json::from_value(json!({"video_context_driver":"kms"})).unwrap();
    let without_context: Settings = serde_json::from_value(json!({"input_player1_a_btn":14})).unwrap();

    assert_eq!(with_context.video_context_driver(), Some("kms"));
    assert_eq!(without_context.video_context_driver(), None);
}

#[test]
fn unsafe_names_values_and_non_scalar_json_fail_before_launch() {
    for data in [
        json!({"bad-name": true}),
        json!({"audio_driver": "alsa\"\nconfig_save_on_exit = true"}),
        json!({"input_overlay": "bad\\path"}),
        json!({"nested": {"value": 1}}),
        json!({"array": [1, 2]}),
        json!({"null": null}),
    ] {
        assert!(serde_json::from_value::<Settings>(data).is_err());
    }
}

#[test]
fn duplicate_keys_are_rejected_instead_of_last_value_wins() {
    let error = serde_json::from_str::<Settings>(r#"{"audio_driver":"alsa","audio_driver":"pulse"}"#)
        .unwrap_err();

    assert!(error.to_string().contains("duplicate RetroArch setting: audio_driver"));
}
