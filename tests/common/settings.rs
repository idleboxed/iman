// SPDX-License-Identifier: BSD-3-Clause
//! Synthetic settings shared by parser and lifecycle tests, not a shipped profile.
use serde_json::{json, Value};

pub fn create_settings() -> Value {
    json!({
        "video_driver": "gl",
        "video_context_driver": "kms",
        "video_autoswitch_refresh_rate": 2,
        "input_driver": "udev",
        "input_joypad_driver": "udev",
        "audio_driver": "alsa",
        "audio_latency": 123,
        "autosave_interval": 60,
    })
}
