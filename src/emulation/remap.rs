// SPDX-License-Identifier: BSD-3-Clause
//! Platform-selected RetroArch input remaps, serialized only into private RAM.
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

use serde::Deserialize;

use crate::{emulation::{deserialize_parameters, setting_line, Value}, runtime::paths};

#[derive(Clone, Debug, Default)]
pub struct Remap(BTreeMap<String, Value>);

impl<'de> Deserialize<'de> for Remap {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let remap = Self(deserialize_parameters(deserializer)?);
        remap.config().map_err(serde::de::Error::custom)?;
        Ok(remap)
    }
}

impl Remap {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn config(&self) -> io::Result<String> {
        let mut config = String::new();
        for (key, value) in &self.0 {
            config.push_str(&setting_line(key, value, true)?);
        }
        Ok(config)
    }
}

#[derive(Clone, Debug)]
pub struct Profile {
    library_name: String,
    config: String,
}

impl Profile {
    pub fn new(library_name: &str, remap: &Remap) -> io::Result<Self> {
        // RetroArch appends the core's reported name to its remap directory.
        // Keep that name a single path component, not a path supplied by JSON.
        if library_name.is_empty()
            || library_name.len() > 128
            || matches!(library_name, "." | "..")
            || library_name.trim() != library_name
            || library_name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | '"'))
        {
            return Err(io::Error::other("invalid remap library_name"));
        }
        Ok(Self { library_name: library_name.into(), config: remap.config()? })
    }

    /// Caller must supply the fresh private verified-tmpfs game runtime.
    /// Existing files/directories are never followed or overwritten.
    pub fn prepare(&self, runtime: &Path) -> io::Result<()> {
        let base = runtime.join(paths::REMAPS_DIR);
        if !fs::symlink_metadata(&base)?.is_dir() {
            return Err(io::Error::other("remap directory must be a real directory"));
        }
        let directory = base.join(&self.library_name);
        fs::create_dir(&directory)?;
        let path = directory.join(format!("{}.rmp", self.library_name));
        OpenOptions::new().write(true).create_new(true).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW).open(path)?.write_all(self.config.as_bytes())
    }
}
