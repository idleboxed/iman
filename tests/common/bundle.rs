// SPDX-License-Identifier: BSD-3-Clause
//! Shared layout only; each scenario supplies its own components and manifest.
use std::{fs, path::Path};

pub fn create_bundle_layout(root: &Path) {
    for directory in [
        "games/states",
        "games/saves",
        "games/images/NES/ab",
        "system/retroarch/cores",
        "system/iman",
        "system/igui",
    ] {
        fs::create_dir_all(root.join(directory)).unwrap();
    }
}
