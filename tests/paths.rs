// SPDX-License-Identifier: BSD-3-Clause
use std::{collections::BTreeMap, path::Path};

use iman::{emulation::launcher, runtime::paths, session};

#[test]
fn runtime_creation_config_and_command_share_the_same_layout() {
    let runtime = session::runtime_directory(Path::new(paths::RUNTIME_ROOT)).unwrap();
    let plan = launcher::LaunchPlan {
        root: Path::new("/synthetic-sd").into(),
        retroarch: Path::new("/synthetic-frontend").into(),
        core: Path::new("/synthetic-core").into(),
        rom: Path::new("/synthetic-rom").into(),
        state: Path::new("/synthetic-sd/games/states/hash.state").into(),
        bios: None,
        core_options: Default::default(),
        remap: None,
        shader_preset: None,
        settings: serde_json::from_value(serde_json::json!({})).unwrap(),
    };

    let config = launcher::config(&plan, runtime.path()).unwrap();
    let command = launcher::command(&plan, runtime.path());

    for name in paths::RUNTIME_DIRS {
        assert!(runtime.path().join(name).is_dir());
    }
    for name in [paths::CACHE_DIR, paths::REMAPS_DIR] {
        assert!(config.contains(runtime.path().join(name).to_str().unwrap()));
    }
    let args: Vec<_> = command.get_args().collect();
    assert_eq!(args[1], runtime.path().join(paths::RETROARCH_CONFIG_FILE));
    let env: BTreeMap<_, _> = command
        .get_envs()
        .map(|(k, v)| (k.to_str().unwrap(), v.unwrap()))
        .collect();
    assert_eq!(
        env["XDG_CONFIG_HOME"],
        runtime.path().join(paths::CONFIG_DIR)
    );
    assert_eq!(env["XDG_CACHE_HOME"], runtime.path().join(paths::CACHE_DIR));
    assert_eq!(env["XDG_DATA_HOME"], runtime.path().join(paths::DATA_DIR));
    assert_eq!(env["TMPDIR"], runtime.path().join(paths::CACHE_DIR));
    runtime.close().unwrap();
}
