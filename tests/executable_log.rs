// SPDX-License-Identifier: BSD-3-Clause
//! Pinned host ELF and tmpfs fixtures, never SD, vendor ELF or board guards.
use std::{ffi::CString, fs, path::Path, process::Command};

#[path = "common/python.rs"]
mod python_fixture;

#[test]
fn repeated_pinned_gui_survives_full_logs_disabled_exports_and_writer_failure() {
    const WORKER: &str = "IMAN_GUI_LOG_WORKER";
    let Some(parent) = std::env::var_os(WORKER) else {
        for mode in ["disabled", "success", "failure"] {
            if mode != "disabled" && !cfg!(feature = "diag-cycle") { continue; }
            let root = tempfile::tempdir_in("/dev/shm").unwrap();
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "repeated_pinned_gui_survives_full_logs_disabled_exports_and_writer_failure", "--nocapture"])
                .env(WORKER, root.path()).env("IMAN_GUI_LOG_MODE", mode).output().unwrap();

            assert!(output.status.success(), "{mode}: {}: {}\n{}", output.status, String::from_utf8_lossy(&output.stderr),
                fs::read_to_string(root.path().join("failure")).unwrap_or_default());
            assert!(fs::metadata(root.path().join("box2/igui.log")).unwrap().len() <= 65536);
            assert_eq!(fs::metadata(root.path().join("preferences")).unwrap().len(), 131072);
            assert_eq!(fs::read_to_string(root.path().join("completed")).unwrap(), "20");
        }
        return;
    };
    let parent = Path::new(&parent);
    let mode = std::env::var("IMAN_GUI_LOG_MODE").unwrap();
    let failure = parent.join("failure");
    std::panic::set_hook(Box::new(move |info| { let _ = fs::write(&failure, info.to_string()); }));
    iman::session::install_signal_handlers().unwrap();
    let mut log = iman::runtime::startup::RamLog::create(parent, "box2").unwrap();
    unsafe { log.activate().unwrap(); }
    log.record("synthetic startup\n").unwrap();
    // Exactly the former bug: IPC is in parent, the owned log in parent/box2.
    let mut control = iman::session::control::Control::bind(parent, parent).unwrap();
    control.attach_launch_log(log);
    #[cfg(feature = "diag-cycle")]
    let exports = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    #[cfg(feature = "diag-cycle")]
    if mode != "disabled" {
        let exports = exports.clone();
        let fail = mode == "failure";
        control.enable_cycle_exports(std::sync::Arc::new(move |run, bundle| {
            let text = String::from_utf8_lossy(&bundle.files["launch.log"]);
            assert!(text.contains("gui-head"));
            assert!(text.contains("gui-tail"));
            assert!(text.contains("bytes omitted"));
            assert!(bundle.metadata["launch_log"]["bytes"].as_u64().unwrap() > 0);
            let output = &bundle.metadata["segment"]["gui_output"];
            assert!(output["pid"].as_u64().unwrap() > 0);
            assert_eq!(output["timing"]["clock"], "iman_session");
            assert_eq!(output["timing"]["meaning"], "receipt_not_emission");
            assert!(output["timing"]["chunks_dropped"].as_u64().unwrap() > 0);
            let chunks = output["timing"]["chunks"].as_array().unwrap();
            assert_eq!(chunks.len(), 64);
            assert_eq!(chunks[0]["stream_offset"], 0);
            let last = chunks.last().unwrap();
            assert_eq!(last["stream_offset"].as_u64().unwrap() + last["bytes"].as_u64().unwrap(), output["received_bytes"]);
            assert!(chunks.windows(2).all(|pair| pair[0]["ms"].as_u64() <= pair[1]["ms"].as_u64()));
            let result = &bundle.metadata["segment"]["gui_result"];
            assert!(result["spawn_observed_ms"].as_u64() <= result["bootstrap_received_ms"].as_u64());
            assert!(result["bootstrap_received_ms"].as_u64() <= result["exit_observed_ms"].as_u64());
            exports.lock().unwrap().push(bundle.metadata["segment"]["sequence"].as_u64().unwrap());
            if fail { return Err(std::io::Error::other("synthetic export failure")); }
            Ok(run.into())
        }), std::time::Duration::ZERO).unwrap();
    }
    if mode == "disabled" {
        // The manager continues after EFBIG; GUI gets a socket, never this file.
        iman::session::diagnostic(format_args!("{}", "x".repeat(70000)));
    }
    let gui = CString::new(format!(
        "{}\n{}",
        include_str!("common/ipc.py"),
        include_str!("common/log_gui.py"),
    )).unwrap();
    for _ in 0..20 {
        let executable = fs::File::open(python_fixture::find_python()).unwrap();
        let arguments = vec![c"python3.12".to_owned(), c"-c".to_owned(), gui.clone(),
            CString::new(control.path().as_os_str().as_encoded_bytes()).unwrap(),
            CString::new(parent.join("preferences").as_os_str().as_encoded_bytes()).unwrap()];

        iman::runtime::executable::supervise_session(executable.into(), arguments, &mut control, &mut |_| {}).unwrap();
        #[cfg(feature = "diag-cycle")]
        if mode != "disabled" {
            control.export_segment(None, || {}).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while control.cycle_pending() {
                assert!(std::time::Instant::now() < deadline, "export did not finish");
                control.poll("diagnostic_export");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            control.next_segment();
        }
    }
    #[cfg(feature = "diag-cycle")]
    {
        let exports = exports.lock().unwrap();
        match mode.as_str() {
            "success" => {
                assert_eq!(*exports, (1..=20).collect::<Vec<u64>>());
                assert!(control.cycle_error().is_none());
            }
            "failure" => {
                assert_eq!(*exports, vec![1]);
                assert!(control.cycle_error().unwrap().contains("synthetic export failure"));
            }
            _ => assert!(exports.is_empty()),
        }
    }
    let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_FSIZE, limit.as_mut_ptr()) }, 0);
    assert_eq!(unsafe { limit.assume_init() }.rlim_cur, 65536);
    fs::write(parent.join("completed"), "20").unwrap();
    drop(control);
    // libtest itself uses panicking stdout writes; do not send its summary into
    // the deliberately full diagnostic file after all assertions passed.
    std::process::exit(0);
}
