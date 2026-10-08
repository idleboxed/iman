// SPDX-License-Identifier: BSD-3-Clause
//! Free-form RetroArch settings with manager-owned storage policy appended last.
pub mod bios;
pub mod core_options;
pub mod launcher;
pub mod remap;
use std::{collections::BTreeMap, io};

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Bool(bool),
    Number(serde_json::Number),
    Text(String),
}

impl Value {
    fn text(&self) -> String {
        match self {
            Self::Bool(value) => value.to_string(),
            Self::Number(value) => value.to_string(),
            Self::Text(value) => value.clone(),
        }
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        if let Self::Text(value) = self {
            if value.len() > 16 * 1024
                || value
                    .chars()
                    .any(|character| character.is_control() || matches!(character, '"' | '\\'))
            {
                return Err(io::Error::other("unsafe RetroArch string value"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Settings(BTreeMap<String, Value>);

pub(crate) fn deserialize_parameters<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, Value>, D::Error> {
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = BTreeMap<String, Value>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("unique RetroArch settings")
        }

        fn visit_map<M: serde::de::MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
            let mut values = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, Value>()? {
                if values.insert(key.clone(), value).is_some() {
                    return Err(serde::de::Error::custom(format!("duplicate RetroArch setting: {key}")));
                }
            }
            Ok(values)
        }
    }
    deserializer.deserialize_map(Visitor)
}

pub(crate) fn setting_line(key: &str, value: &Value, axis_suffix: bool) -> io::Result<String> {
    let name = if axis_suffix { key.trim_end_matches(['+', '-']) } else { key };
    if name.is_empty()
        || key.len() > 128
        || key.len() - name.len() > 1
        || !name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(io::Error::other(format!("invalid RetroArch setting name: {key}")));
    }
    value.validate()?;
    Ok(format!("{key} = \"{}\"\n", value.text()))
}

impl<'de> Deserialize<'de> for Settings {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let settings = Self(deserialize_parameters(deserializer)?);
        settings.config().map_err(serde::de::Error::custom)?;
        Ok(settings)
    }
}

impl Settings {
    pub fn config(&self) -> io::Result<String> {
        self.config_with_remap(false, &[])
    }

    pub(crate) fn config_with_remap(&self, remap: bool, managed_keys: &[&str]) -> io::Result<String> {
        let mut config = String::new();
        for (key, value) in &self.0 {
            let line = setting_line(key, value, false)?;
            let manager_owned = crate::runtime::paths::RETROARCH_POLICY.lines().any(|line| {
                line.split_once(" = ").is_some_and(|(owned, _)| owned == key)
            }) || managed_keys.contains(&key.as_str()) || (remap && matches!(key.as_str(),
                "input_remap_binds_enable" | "input_remap_sort_by_controller_enable"));
            if !manager_owned {
                config.push_str(&line);
            }
        }
        // Only our selected RAM remap may enable loading. Keep implicit writes
        // disabled and do not let a controller-specific lookup bypass this file.
        for line in crate::runtime::paths::RETROARCH_POLICY.lines() {
            if remap && line.starts_with("auto_remaps_enable = ") {
                config.push_str("auto_remaps_enable = \"true\"\n");
            } else {
                config.push_str(line);
                config.push('\n');
            }
        }
        if remap {
            config.push_str("input_remap_binds_enable = \"true\"\n");
            config.push_str("input_remap_sort_by_controller_enable = \"false\"\n");
        }
        Ok(config)
    }

    pub fn video_context_driver(&self) -> Option<&str> {
        match self.0.get("video_context_driver") {
            Some(Value::Text(value)) => Some(value),
            _ => None,
        }
    }
}
