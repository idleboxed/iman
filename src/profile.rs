// SPDX-License-Identifier: BSD-3-Clause
//! Explicit Linux session settings, independent of CPU and launching firmware.
use serde::Deserialize;
use std::{
    ffi::OsString,
    fs::OpenOptions,
    io::{self, Read},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use crate::runtime::paths;

fn runtime_default() -> PathBuf {
    paths::RUNTIME_ROOT.into()
}
fn card_default() -> PathBuf {
    "/dev/dri/card0".into()
}
#[derive(Debug, Default, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case")]
pub enum Display {
    #[default]
    Desktop,
    Headless,
    Drm {
        #[serde(default = "card_default")]
        device: PathBuf,
        #[serde(default)]
        connector: Option<u32>,
        #[serde(default)]
        allow_vendor_einval: bool,
    },
}

#[derive(Debug, Deserialize)]
pub struct Gui {
    #[serde(default)]
    display: Display,
    #[serde(default)]
    frames: Option<u32>,
    #[serde(default)]
    hid_profile: Option<PathBuf>,
    #[serde(default)]
    evdev_profile: Option<PathBuf>,
}

impl Default for Gui {
    fn default() -> Self {
        Self {
            display: Display::Desktop,
            frames: None,
            hid_profile: None,
            evdev_profile: None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Profile {
    #[serde(default = "runtime_default")]
    runtime_dir: PathBuf,
    #[serde(default)]
    gui: Gui,
}

impl Profile {
    /// Bounded regular JSON; loading never creates files or opens device nodes.
    /// Input-profile paths are relative to this file, not the launcher's cwd.
    pub fn load(path: &Path) -> io::Result<Self> {
        let path = std::path::absolute(path)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)?;
        Self::read(file, &path)
    }

    /// The installed profile is optional; malformed or unsafe existing entries are errors.
    pub fn load_default(root: &crate::runtime::bundle::Root) -> io::Result<Option<Self>> {
        let relative = format!("{}/{}", paths::SYSTEM_DIR, paths::PROFILE_FILE);
        match root.open_file(&relative) {
            Ok(file) => Self::read(file, &root.path().join(relative)).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn read(file: std::fs::File, path: &Path) -> io::Result<Self> {
        if !file.metadata()?.is_file() {
            return Err(io::Error::other("session profile must be a regular file"));
        }
        let mut bytes = Vec::new();
        file.take(65537).read_to_end(&mut bytes)?;
        if bytes.len() > 65536 {
            return Err(io::Error::other("session profile exceeds 64 KiB"));
        }
        let mut profile: Self = serde_json::from_slice(&bytes)?;
        profile.validate()?;
        for input in [&mut profile.gui.hid_profile, &mut profile.gui.evdev_profile]
            .into_iter()
            .flatten()
        {
            if input.is_relative() {
                *input = path
                    .parent()
                    .expect("absolute file has parent")
                    .join(&*input);
            }
        }
        Ok(profile)
    }

    fn validate(&self) -> io::Result<()> {
        if !self.runtime_dir.is_absolute() {
            return Err(io::Error::other("expected absolute runtime_dir"));
        }
        if self.gui.frames == Some(0) {
            return Err(io::Error::other("frames must be positive"));
        }
        if matches!(self.gui.display, Display::Headless)
            && (self.gui.frames.is_some()
                || self.gui.hid_profile.is_some()
                || self.gui.evdev_profile.is_some())
        {
            return Err(io::Error::other(
                "headless profile forbids frames and input profiles",
            ));
        }
        if let Display::Drm {
            device, connector, ..
        } = &self.gui.display
        {
            if !device.is_absolute() || *connector == Some(0) {
                return Err(io::Error::other(
                    "DRM requires an absolute device and a positive connector ID",
                ));
            }
        }
        for input in [&self.gui.hid_profile, &self.gui.evdev_profile]
            .into_iter()
            .flatten()
        {
            if input.as_os_str().is_empty() {
                return Err(io::Error::other("input profile path is empty"));
            }
        }
        Ok(())
    }

    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }

    pub fn display(&self) -> &Display {
        &self.gui.display
    }

    pub fn gui_arguments(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = Vec::new();
        match &self.gui.display {
            Display::Desktop => {}
            Display::Headless => args.push("--headless".into()),
            Display::Drm {
                device,
                connector,
                allow_vendor_einval,
            } => {
                args.extend(["--drm".into(), "--drm-device".into(), device.into()]);
                if let Some(id) = connector {
                    args.extend(["--drm-connector".into(), id.to_string().into()]);
                }
                if *allow_vendor_einval {
                    args.push("--allow-vendor-einval".into());
                }
            }
        }
        if let Some(frames) = self.gui.frames {
            args.extend(["--frames".into(), frames.to_string().into()]);
        }
        for (flag, path) in [
            ("--hid-profile", &self.gui.hid_profile),
            ("--evdev-profile", &self.gui.evdev_profile),
        ] {
            if let Some(path) = path {
                args.extend([flag.into(), path.into()]);
            }
        }
        args
    }
}
