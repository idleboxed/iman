// SPDX-License-Identifier: BSD-3-Clause
//! Validated libretro options, created only in a fresh descriptor-held RAM runtime.
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Write},
    os::{fd::{AsRawFd, FromRawFd}, unix::fs::MetadataExt},
    path::Path,
};

use serde::Deserialize;

use crate::{emulation::{deserialize_parameters, Value}, runtime::paths};

const OPTION_LIMIT: usize = 128;
const CONFIG_LIMIT: usize = 32 * 1024;

#[derive(Clone, Debug, Default)]
pub struct CoreOptions(BTreeMap<String, Value>);

impl<'de> Deserialize<'de> for CoreOptions {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let options = Self(deserialize_parameters(deserializer)?);
        options.config().map_err(serde::de::Error::custom)?;
        Ok(options)
    }
}

impl CoreOptions {
    pub fn config(&self) -> io::Result<String> {
        if self.0.len() > OPTION_LIMIT {
            return Err(io::Error::other("too many core options"));
        }
        let mut config = String::new();
        for (key, value) in &self.0 {
            // Libretro keys contain hyphens; frontend settings/remaps retain
            // their separate, stricter naming rules.
            if key.is_empty() || key.len() > 128
                || !key.bytes().next().is_some_and(|byte| byte.is_ascii_alphanumeric())
                || !key.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
            {
                return Err(io::Error::other(format!("invalid core option name: {key}")));
            }
            let Value::Text(text) = value else {
                return Err(io::Error::other("core option values must be strings"));
            };
            value.validate()?;
            let line = format!("{key} = \"{text}\"\n");
            if config.len() + line.len() > CONFIG_LIMIT {
                return Err(io::Error::other("core options configuration exceeds 32 KiB"));
            }
            config.push_str(&line);
        }
        Ok(config)
    }

    /// No path fallback, overwrite or following of pre-existing runtime entries.
    pub fn prepare(&self, runtime: &Path) -> io::Result<()> {
        let config = self.config()?;
        let directory = crate::runtime::bundle::directory(runtime)?;
        crate::runtime::startup::ram_space(&directory)?;
        let name = std::ffi::CString::new(paths::CORE_OPTIONS_FILE).expect("fixed options filename");
        let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: live parent FD, fixed NUL-terminated filename, exclusive creation.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags, 0o600) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a fresh owned descriptor.
        let mut file = unsafe { File::from_raw_fd(fd) };
        let meta = file.metadata()?;
        let current = crate::runtime::bundle::directory(runtime)?.metadata()?;
        let held = directory.metadata()?;
        if !meta.is_file() || meta.nlink() != 1 || meta.dev() != held.dev()
            || current.dev() != held.dev() || current.ino() != held.ino()
        {
            return Err(io::Error::other("core options runtime identity changed"));
        }
        file.write_all(config.as_bytes())
    }
}
