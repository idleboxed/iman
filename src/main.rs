// SPDX-License-Identifier: BSD-3-Clause
use std::{error::Error, path::PathBuf};

fn run() -> Result<(), Box<dyn Error>> {
    if std::env::args_os().nth(1).is_some_and(|argument| argument == "ctl") {
        let cli: Vec<_> = std::env::args().skip(1).collect();
        if !(3..=4).contains(&cli.len()) {
            return Err("Usage: iman ctl SOCKET MODULE.COMMAND [JSON_PARAMS]".into());
        }
        let params = cli
            .get(3)
            .map(|text| serde_json::from_str(text))
            .transpose()?
            .unwrap_or_else(|| serde_json::json!({}));
        let response = imanlib::client::Client::connect(std::path::Path::new(&cli[1]))?
            .call(&cli[2], params)?;
        println!("{}", serde_json::to_string(&response)?);
        if let imanlib::protocol::Outcome::Error { error } = response.outcome {
            return Err(error.into());
        }
        return Ok(());
    }
    let mut root = None;
    let mut config = None;
    let mut gui_args = Vec::new();
    let mut warned_unknown = false;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            println!("iman\nClient: iman ctl SOCKET MODULE.COMMAND [JSON_PARAMS]\nUsage: iman [--root SD_ROOT] [--config PROFILE.json] [-- IGUI_OPTIONS]\nDefault root: installation containing system/iman/iman. The local iman.json is loaded automatically unless raw GUI options are supplied.\nIGUI control uses the session IPC socket. Esc in RetroArch returns to IGUI.\nPassive diagnostics and automatic reports are selected by Cargo features, not runtime switches. Reports go to ROOT/iman-reports; unavailable sources are reported without selecting another disk.\nLinux sessions use checked installation files and private tmpfs logs; no CPU-specific launch flag is required.\nSave State persists in games/states/. SRAM loads and saves in games/saves/: packaged profiles check changes every 60 seconds and save on normal exit. No mount changes or durability guarantee. Configured sessions: explicit RetroArch drivers, no board-based game prohibition.");
            println!("The pc build feature enables the manager window; -- --headless disables it.");
            println!("Unknown startup arguments before -- are ignored with one warning. Known options retain validation; arguments after -- are passed to IGUI unchanged.");
            return Ok(());
        } else if arg == "--root" {
            if root.is_some() {
                return Err("duplicate option: --root".into());
            }
            root = Some(PathBuf::from(
                args.next().ok_or("--root needs a directory")?,
            ));
        } else if arg == "--config" {
            if config.is_some() {
                return Err("duplicate option: --config".into());
            }
            config = Some(PathBuf::from(args.next().ok_or("--config needs a file")?));
        } else if arg == "--" {
            gui_args.extend(args);
            break;
        } else if !warned_unknown {
            // Skip one token, without guessing an unknown option's value count.
            // Do not echo arbitrary values: they may contain secrets or control bytes.
            iman::session::diagnostic(format_args!(
                "iman: warning: ignoring unrecognized startup arguments before --"
            ));
            warned_unknown = true;
        }
    }
    if config.is_some() && !gui_args.is_empty() {
        return Err("--config cannot be combined with raw IGUI options".into());
    }
    if config.is_none() && gui_args.iter().any(|arg| arg == "--drm") {
        return Err("DRM sessions require --config with explicit platform settings".into());
    }
    let root = match root {
        Some(root) => root,
        None => iman::runtime::paths::root_from_executable(&std::env::current_exe()?)?,
    };
    let held_root = iman::runtime::bundle::Root::open(&root)?;
    let root = held_root.path().to_path_buf();
    let profile = match config {
        Some(path) => Some(iman::profile::Profile::load(&path)?),
        None if gui_args.is_empty() => iman::profile::Profile::load_default(&held_root)?,
        None => None,
    };
    let parent = profile.as_ref().map_or(
        std::path::Path::new(iman::runtime::paths::RUNTIME_ROOT), |profile| profile.runtime_dir(),
    );
    iman::runtime::startup::prepare_runtime(parent)?;
    let log = iman::runtime::startup::RamLog::temporary(parent)?;
    // SAFETY: standalone entry, before threads, IPC and child processes. All
    // user-relative paths have been resolved before changing cwd or stdio.
    if unsafe { log.activate() }.is_err() {
        std::process::exit(70);
    }
    iman::session::install_signal_handlers()?;
    let mut control = iman::session::control::Control::bind(&root, parent)?;
    control.attach_launch_log(log);
    control.attach_root(held_root)?;
    control.start_diagnostics()?;
    let result = if let Some(profile) = &profile {
        iman::session::run_configured(&root, profile, &mut control, &mut |_| {})
    } else {
        iman::session::run_controlled(&root, &gui_args, &mut control, &mut |_| {})
    };
    control.finish(
        result.as_ref().err().map(ToString::to_string),
        result
            .as_ref()
            .is_err_and(|error| error.kind() == std::io::ErrorKind::Interrupted),
    )?;

    result?;
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        iman::session::diagnostic(format_args!("iman: {error}"));
        std::process::exit(1);
    }
}
