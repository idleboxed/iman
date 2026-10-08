// SPDX-License-Identifier: BSD-3-Clause
//! Session-manager layout; keep the SD format aligned with IGUI and its installer.

pub const SYSTEM_DIR: &str = "system";
pub const GAMES_DIR: &str = "games";
pub const IMAGES_DIR: &str = "images";
pub const CORES_DIR: &str = "retroarch/cores";
pub const RETROARCH_AUTOCONFIG_DIR: &str = "retroarch/padconf";
pub const BIOS_DIR: &str = "bios";
pub const GUI_BINARY: &str = "igui/igui";
pub const EMULATION_FILE: &str = "iman/emulation.json";
pub const PROFILE_FILE: &str = "iman/iman.json";
pub const PERSISTENT_STATES_DIR: &str = "games/states";
pub const PERSISTENT_SAVES_DIR: &str = "games/saves";

// The tmpfs check remains mandatory even if this location is changed.
pub const RUNTIME_ROOT: &str = "/dev/shm";
pub const RUNTIME_PREFIX: &str = "iman-";
pub const RETROARCH_CONFIG_FILE: &str = "retroarch.cfg";
pub const CORE_OPTIONS_FILE: &str = "core-options.cfg";
pub const SAVES_DIR: &str = "saves";
pub const STATES_DIR: &str = "states";
pub const CACHE_DIR: &str = "cache";
pub const SCREENSHOTS_DIR: &str = "screenshots";
pub const LOGS_DIR: &str = "logs";
pub const CONFIG_DIR: &str = "config";
pub const DATA_DIR: &str = "data";
pub const REMAPS_DIR: &str = "remaps";
pub const CONTENT_DIR: &str = "content";
pub const RUNTIME_DIRS: &[&str] = &[
    BIOS_DIR,
    CONTENT_DIR,
    SAVES_DIR,
    STATES_DIR,
    CACHE_DIR,
    SCREENSHOTS_DIR,
    LOGS_DIR,
    CONFIG_DIR,
    DATA_DIR,
    REMAPS_DIR,
];

pub const RETROARCH_POLICY: &str = include_str!("../emulation/retroarch-policy.cfg");

/// Only the installed layout may select an implicit write destination.
pub fn root_from_executable(executable: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let component = executable.parent().ok_or_else(|| std::io::Error::other("cannot determine installation root"))?;
    let system = component.parent().ok_or_else(|| std::io::Error::other("cannot determine installation root"))?;
    if component.file_name() != Some(std::ffi::OsStr::new("iman"))
        || system.file_name() != Some(std::ffi::OsStr::new(SYSTEM_DIR))
    {
        return Err(std::io::Error::other("use --root or install iman at system/iman/iman"));
    }
    system.parent().map(std::path::Path::to_path_buf)
        .ok_or_else(|| std::io::Error::other("cannot determine installation root"))
}
