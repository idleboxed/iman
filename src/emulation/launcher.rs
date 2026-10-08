// SPDX-License-Identifier: BSD-3-Clause
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read},
    os::{fd::AsRawFd, unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt}},
    path::{Path, PathBuf},
    process::Command,
};

use serde::Deserialize;

use crate::runtime::paths;
use imanlib::igui::{valid_hash, valid_image_path, LaunchRequest};

#[derive(Deserialize)]
struct Component {
    path: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SystemDirectory {
    #[default]
    Bios,
    Ram,
}

#[derive(Deserialize)]
struct CoreComponent {
    path: String,
    shader_preset: Option<String>,
    library_name: Option<String>,
    #[serde(default)]
    remap: crate::emulation::remap::Remap,
    #[serde(default)]
    system_directory: SystemDirectory,
    #[serde(default)]
    core_options: crate::emulation::core_options::CoreOptions,
}

#[derive(Deserialize)]
struct Manifest {
    architecture: String,
    retroarch: Component,
    cores: BTreeMap<String, CoreComponent>,
    settings: crate::emulation::Settings,
}

#[derive(Debug)]
pub struct LaunchPlan {
    pub root: PathBuf,
    pub retroarch: PathBuf,
    pub core: PathBuf,
    pub rom: PathBuf,
    pub state: PathBuf,
    pub settings: crate::emulation::Settings,
    pub bios: Option<PathBuf>,
    pub core_options: crate::emulation::core_options::CoreOptions,
    pub remap: Option<crate::emulation::remap::Profile>,
    pub shader_preset: Option<PathBuf>,
}

/// Keep the selected SD path in failures, not RetroArch's temporary hash-named link.
pub(crate) fn failure_details(root: &Path, game: &LaunchRequest, reason: impl std::fmt::Display) -> String {
    let rom = root.join(paths::GAMES_DIR).join(paths::IMAGES_DIR).join(&game.image);
    format!("ROM: {}\n{reason}", rom.display())
}

fn read_manifest(system: &Path) -> io::Result<Manifest> {
    let manifest_path = contained_file(system, paths::EMULATION_FILE)?;
    let mut bytes = Vec::new();
    File::open(manifest_path)?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 {
        return Err(io::Error::other("component manifest too large"));
    }
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    if manifest.architecture != std::env::consts::ARCH {
        return Err(io::Error::other("unsupported manifest architecture"));
    }
    Ok(manifest)
}

/// GUI-only installations need no manifest; malformed existing files are not ignored.
pub fn configured_video_context(root: &Path) -> io::Result<Option<Option<String>>> {
    let system = root.join(paths::SYSTEM_DIR);
    if !system.join(paths::EMULATION_FILE).try_exists()? {
        return Ok(None);
    }
    Ok(Some(
        read_manifest(&system)?
            .settings
            .video_context_driver()
            .map(str::to_owned),
    ))
}

/// Canonicalize only the selected path, and reject escaping symlinks.
pub fn contained_file(root: &Path, relative: &str) -> io::Result<PathBuf> {
    if !valid_image_path(relative) {
        return Err(io::Error::other("invalid relative path"));
    }
    let root = root.canonicalize()?;
    let path = root.join(relative).canonicalize()?;
    if !path.starts_with(&root) || !path.is_file() {
        return Err(io::Error::other(
            "path escapes root or is not a regular file",
        ));
    }
    Ok(path)
}

fn resolve_component(system: &Path, relative: &str) -> io::Result<PathBuf> {
    let path = contained_file(system, relative)?;
    if fs::metadata(&path)?.len() > 128 * 1024 * 1024 {
        return Err(io::Error::other("component exceeds size limit"));
    }
    Ok(path)
}

fn resolve_shader_preset(root: &Path, relative: &str) -> io::Result<PathBuf> {
    let check = || -> io::Result<PathBuf> {
        if !valid_image_path(relative) || Path::new(relative).extension() != Some(std::ffi::OsStr::new("glslp")) {
            return Err(io::Error::other("shader_preset must be a system-relative .glslp path"));
        }
        let held_root = crate::runtime::bundle::Root::open(root)?;
        let file = held_root.open_file(&format!("{}/{relative}", paths::SYSTEM_DIR))?;
        if file.metadata()?.len() > 1024 * 1024 {
            return Err(io::Error::other("shader preset exceeds size limit"));
        }
        // RetroArch resolves references itself. A RAM copy or /proc FD path
        // would change their base; pass the original installation path instead.
        Ok(held_root.path().join(paths::SYSTEM_DIR).join(relative))
    };
    check().map_err(|error| io::Error::new(error.kind(), format!("Cannot read shader preset: {relative}. {error}")))
}

pub fn resolve(root: &Path, request: &LaunchRequest) -> io::Result<LaunchPlan> {
    if !valid_hash(&request.hash) {
        return Err(io::Error::other("invalid game hash"));
    }
    let root = root.canonicalize()?;
    let system = root.join(paths::SYSTEM_DIR).canonicalize()?;
    let images = root
        .join(paths::GAMES_DIR)
        .join(paths::IMAGES_DIR)
        .canonicalize()?;
    if !system.starts_with(&root) || !images.starts_with(&root) {
        return Err(io::Error::other("system or images escapes SD root"));
    }
    let manifest = read_manifest(&system)?;
    let core = manifest
        .cores
        .get(&request.platform)
        .ok_or_else(|| io::Error::other(format!("no core for platform {}", request.platform)))?;
    let shader_preset = core.shader_preset.as_deref()
        .map(|relative| resolve_shader_preset(&root, relative)).transpose()?;
    let remap = if core.remap.is_empty() {
        None
    } else {
        let name = core.library_name.as_deref()
            .ok_or_else(|| io::Error::other("nonempty remap requires library_name"))?;
        Some(crate::emulation::remap::Profile::new(name, &core.remap)?)
    };
    let rom = contained_file(&images, &request.image)?;
    let retroarch = resolve_component(&system, &manifest.retroarch.path)?;
    if fs::metadata(&retroarch)?.permissions().mode() & 0o111 == 0 {
        return Err(io::Error::other("RetroArch is not executable"));
    }
    if !crate::emulation::bios::valid_platform(&request.platform) {
        return Err(io::Error::other("invalid BIOS platform identifier"));
    }
    let bios = match core.system_directory {
        SystemDirectory::Bios => crate::emulation::bios::directory(&root, &request.platform)?,
        SystemDirectory::Ram => None,
    };
    save_path(&root, &request.hash)?;
    Ok(LaunchPlan {
        bios,
        core_options: core.core_options.clone(),
        remap,
        shader_preset,
        state: state_path(&root, &request.hash)?,
        root,
        retroarch,
        core: resolve_component(&system, &core.path)?,
        rom,
        settings: manifest.settings,
    })
}

/// Preparation belongs to the PC installer, not to launching/closing a game.
/// Never fall back to the ROM directory when persistent storage is unavailable.
pub fn state_path(root: &Path, game_hash: &str) -> io::Result<PathBuf> {
    persistent_path(root, game_hash, paths::PERSISTENT_STATES_DIR, "states", "state", "state")
}

/// RetroArch loads and writes SRAM/RTC directly, with the explicitly accepted
/// periodic/exit-time policy. Launch itself only validates existing storage.
pub fn save_path(root: &Path, game_hash: &str) -> io::Result<PathBuf> {
    persistent_path(root, game_hash, paths::PERSISTENT_SAVES_DIR, "saves", "srm", "")
}

fn persistent_path(
    root: &Path, game_hash: &str, relative: &str, kind: &str, extension: &str, slot_prefix: &str,
) -> io::Result<PathBuf> {
    if !valid_hash(game_hash) {
        return Err(io::Error::other("invalid persistent save identity"));
    }
    let directory = root.join(relative);
    // Check games as well as states: the nested layout must not introduce an
    // ancestor-symlink or cross-device bypass of the persistent destination.
    let held_root = crate::runtime::bundle::Root::open(root)?;
    let held = held_root.open_directory(relative).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("prepare the {kind} directory {relative} on the PC: {e}"),
        )
    })?;
    let anchored = PathBuf::from(format!("/proc/self/fd/{}", held.as_raw_fd()));
    let device = held.metadata()?.dev();
    let name = format!("{game_hash}.{extension}");
    let prefix = format!("{game_hash}.{slot_prefix}");
    for entry in fs::read_dir(&anchored)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let meta = fs::symlink_metadata(entry.path())?;
            if !meta.is_file() || meta.file_type().is_symlink() || meta.nlink() != 1 || meta.dev() != device {
                return Err(io::Error::other(format!("{kind} slot must be a regular file on the same device with one link")));
            }
            let file = OpenOptions::new().read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(entry.path())?;
            let opened = file.metadata()?;
            if !opened.is_file() || opened.nlink() != 1 || opened.dev() != device || opened.ino() != meta.ino() {
                return Err(io::Error::other(format!("{kind} slot identity changed during preflight")));
            }
        }
    }
    let current = held_root.open_directory(relative)?.metadata()?;
    if current.dev() != device || current.ino() != held.metadata()?.ino() {
        return Err(io::Error::other(format!("{kind} directory identity changed during preflight")));
    }
    Ok(directory.join(name))
}

/// States and SRAM use their guarded persistent directories; other data stay in RAM.
pub fn config(plan: &LaunchPlan, runtime: &Path) -> io::Result<String> {
    let managed_paths = [
        ("savefile_directory", plan.root.join(paths::PERSISTENT_SAVES_DIR)),
        (
            "savestate_directory",
            plan.root.join(paths::PERSISTENT_STATES_DIR),
        ),
        ("cache_directory", runtime.join(paths::CACHE_DIR)),
        ("screenshot_directory", runtime.join(paths::SCREENSHOTS_DIR)),
        ("runtime_log_directory", runtime.join(paths::LOGS_DIR)),
        ("core_options_path", runtime.join(paths::CORE_OPTIONS_FILE)),
        ("input_remapping_directory", runtime.join(paths::REMAPS_DIR)),
        // Existing firmware stays on SD unless the core explicitly requires RAM.
        // With no selected BIOS directory, never fall back to the ROM directory.
        ("system_directory", plan.bios.clone().unwrap_or_else(|| runtime.join(paths::BIOS_DIR))),
        (
            "libretro_directory",
            plan.root.join(paths::SYSTEM_DIR).join(paths::CORES_DIR),
        ),
        (
            "joypad_autoconfig_dir",
            plan.root
                .join(paths::SYSTEM_DIR)
                .join(paths::RETROARCH_AUTOCONFIG_DIR),
        ),
    ];
    // RetroArch gives the first duplicate key precedence. Exclude computed
    // paths before serialization, rather than attempting to override them later.
    let mut managed_keys: Vec<_> = managed_paths.iter().map(|(key, _)| *key).collect();
    managed_keys.push("video_shader_enable");
    let mut text = plan.settings.config_with_remap(plan.remap.is_some(), &managed_keys)?;
    text.push_str(&format!("video_shader_enable = \"{}\"\n", plan.shader_preset.is_some()));
    for (key, path) in managed_paths {
        let value = path
            .to_str()
            .ok_or_else(|| io::Error::other("config path is not UTF-8"))?;
        if value.chars().any(|c| c.is_control() || "\"\\".contains(c)) {
            return Err(io::Error::other(
                "config path contains unsupported characters",
            ));
        }
        text.push_str(&format!("{key} = \"{value}\"\n"));
    }
    Ok(text)
}

/// Upstream RetroArch derives state names from the content basename. A tmpfs
/// symlink gives single-file ROMs a stable basename without renaming/copying the
/// SD content or patching RetroArch. This is not a multi-file media adapter.
pub fn content_path(plan: &LaunchPlan, runtime: &Path) -> PathBuf {
    runtime
        .join(paths::CONTENT_DIR)
        .join(plan.state.file_name().expect("validated state filename"))
        .with_extension(plan.rom.extension().unwrap_or_default())
}

/// Called only after creation of the private verified-tmpfs runtime. No overwrite.
pub fn prepare_content(plan: &LaunchPlan, runtime: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(&plan.rom, content_path(plan, runtime))
}

pub fn command(plan: &LaunchPlan, runtime: &Path) -> Command {
    let mut command = Command::new(&plan.retroarch);
    command
        .current_dir(runtime)
        .arg("--config")
        .arg(runtime.join(paths::RETROARCH_CONFIG_FILE))
        .args(["--verbose", "--sram-mode", "load-save", "--set-shader"])
        .arg(plan.shader_preset.as_deref().unwrap_or(Path::new("")))
        .arg("-L")
        .arg(&plan.core)
        .arg(content_path(plan, runtime))
        .env("HOME", runtime)
        .env("XDG_CONFIG_HOME", runtime.join(paths::CONFIG_DIR))
        .env("XDG_CACHE_HOME", runtime.join(paths::CACHE_DIR))
        .env("XDG_DATA_HOME", runtime.join(paths::DATA_DIR))
        .env("TMPDIR", runtime.join(paths::CACHE_DIR));
    command
}
