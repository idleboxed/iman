//! Synthetic executables, not RetroArch, real ROMs, or hardware tests.
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use tempfile::TempDir;

#[path = "common/bundle.rs"]
mod bundle_fixture;
#[path = "common/settings.rs"]
mod settings_fixture;
#[path = "common/python.rs"]
mod python_fixture;

fn write_executable(path: &Path, source: &str) {
    fs::write(path, format!("#!{}\n{source}", python_fixture::find_python().display())).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn fixture(mode: &str) -> TempDir {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    bundle_fixture::create_bundle_layout(root);
    fs::write(
        root.join("games/images/NES/ab/Game ' one.nes"),
        b"synthetic ROM",
    )
    .unwrap();
    fs::write(root.join("system/retroarch/cores/test.so"), b"synthetic core").unwrap();
    fs::write(root.join("mode"), mode).unwrap();
    write_executable(
        &root.join("system/igui/igui"),
        &format!("{}\n{}", include_str!("common/ipc.py"), include_str!("common/lifecycle_gui.py")),
    );
    write_executable(
        &root.join("system/retroarch/retroarch"),
        include_str!("common/lifecycle_game.py"),
    );
    let component = |path: &str| json!({"path": path});
    fs::write(
        root.join("system/iman/emulation.json"),
        serde_json::to_vec(&json!({
            "version": 1, "architecture": std::env::consts::ARCH,
            "retroarch": component("retroarch/retroarch"), "cores": {"NES": component("retroarch/cores/test.so")},
            "settings": {"video_context_driver": "kms"}
        }))
        .unwrap(),
    )
    .unwrap();
    temp
}

fn spawn(root: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_iman"))
        .args(["--root", root.to_str().unwrap()])
        .args(["--", "--headless"])
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap()
}

fn wait(child: &mut Child) -> ExitStatus {
    // Diagnostic builds also observe a failed GUI before finalizing its report.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("manager did not finish within 30 seconds");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn trace(root: &Path) -> Vec<Value> {
    fs::read_to_string(root.join("trace"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn gui_exits_before_game_and_returns_with_state() {
    let root = fixture("success");

    let status = wait(&mut spawn(root.path()));

    assert!(status.success());
    let events = trace(root.path());
    assert_eq!(
        events
            .iter()
            .map(|e| e["event"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["gui", "game", "gui"]
    );
    assert_ne!(events[0]["pid"], events[2]["pid"]);
    assert_eq!(
        events[2]["bootstrap"]["state"]["selected_hash"],
        "ab".repeat(32)
    );
    assert_eq!(events[2]["bootstrap"]["state"]["platform"], "NES");
    assert_eq!(events[2]["args"][0], "--managed");
    assert_eq!(events[2]["args"][1], "--ipc-socket");
    assert_eq!(events[2]["args"].as_array().unwrap().len(), 4);
    assert_eq!(events[2]["args"][3], "--headless");
    assert!(events[2]["bootstrap"]["notice"].is_null());
    assert!(!Path::new(events[1]["runtime"].as_str().unwrap()).exists());
}

#[test]
fn emulator_error_returns_to_gui_with_notice_and_cleans_runtime() {
    let root = fixture("game-error");

    assert!(wait(&mut spawn(root.path())).success());

    let events = trace(root.path());
    assert_eq!(events.len(), 3);
    assert_eq!(
        events[2]["bootstrap"]["notice"],
        format!("Game session failed.\nROM: {}\nEmulator status: exit status: 7\nCheck ROM, core and BIOS.",
            root.path().join("games/images/NES/ab/Game ' one.nes").display())
    );
    assert!(!Path::new(events[1]["runtime"].as_str().unwrap()).exists());
}

#[test]
fn prepare_and_handoff_failures_report_rom_path_without_starting_emulator() {
    for mode in ["success", "handoff-error"] {
        let root = fixture(mode);
        let rom = root.path().join("games/images/NES/ab/Game ' one.nes");
        if mode == "success" {
            fs::remove_file(&rom).unwrap();
        }

        assert!(wait(&mut spawn(root.path())).success());

        let events = trace(root.path());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["event"], "gui");
        assert_eq!(fs::read_to_string(root.path().join("preflight-error")).unwrap(), "launch_refused");
        assert_eq!(fs::read_to_string(root.path().join("preflight-detail")).unwrap(),
            format!("ROM: {}\nNo such file or directory (os error 2)", rom.display()));
    }
}

#[test]
fn failed_or_malformed_gui_never_launches_game_or_respawns() {
    for mode in ["gui-error", "oversized"] {
        let root = fixture(mode);

        assert!(!wait(&mut spawn(root.path())).success());

        let events = trace(root.path());
        assert_eq!(events.len(), 1);
        assert!(!Path::new(&format!("/proc/{}", events[0]["pid"])).exists());
    }
}

#[test]
fn cancellation_reaps_emulator_and_removes_temporary_saves() {
    let root = fixture("cancel");
    let mut child = spawn(root.path());
    let deadline = Instant::now() + Duration::from_secs(5);
    while trace(root.path()).len() < 2 {
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("emulator did not start");
        }
        thread::sleep(Duration::from_millis(20));
    }

    // SAFETY: only signal the manager process spawned by this test.
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    assert!(!wait(&mut child).success());

    let events = trace(root.path());
    assert_eq!(events.len(), 2);
    assert!(!Path::new(events[1]["runtime"].as_str().unwrap()).exists());
    assert!(!Path::new(&format!("/proc/{}", events[1]["pid"])).exists());
}

#[test]
fn observation_hooks_follow_gui_game_gui_without_changing_session_behavior() {
    let root = fixture("success");
    let mut phases = Vec::new();

    iman::session::run_observed(root.path(), &["--headless".into()], &mut |phase| phases.push(phase)).unwrap();

    let transitions: Vec<_> = phases
        .into_iter()
        .filter(|p| p.ends_with("_start") || p.ends_with("_exit"))
        .collect();
    assert_eq!(
        transitions,
        [
            "gui_start",
            "gui_exit",
            "game_start",
            "game_exit",
            "gui_start",
            "gui_exit"
        ]
    );
    assert_eq!(trace(root.path()).len(), 3);
}

#[test]
fn full_diagnostic_stream_does_not_abort_manager_or_gui_game_gui() {
    use std::os::unix::process::CommandExt;
    for bounded_file in [false, true] {
        let root = fixture("success");
        let stderr = if bounded_file {
            let log = root.path().join("full.log");
            fs::write(&log, vec![b'x'; 65536]).unwrap();
            fs::OpenOptions::new().append(true).open(log).unwrap()
        } else {
            fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .unwrap()
        };
        let mut command = Command::new(env!("CARGO_BIN_EXE_iman"));
        command
            .args(["--root", root.path().to_str().unwrap()])
            .args(["--", "--headless"])
            .stderr(stderr);
        // Only the spawned fixture manager and its children receive this limit.
        // Their ordinary trace/save files are far smaller than the full log.
        unsafe {
            command.pre_exec(|| {
                let limit = libc::rlimit {
                    rlim_cur: 65536,
                    rlim_max: 65536,
                };
                if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let status = wait(&mut command.spawn().unwrap());

        assert!(status.success());
        assert_eq!(trace(root.path()).len(), 3);
        if bounded_file {
            assert_eq!(
                fs::metadata(root.path().join("full.log")).unwrap().len(),
                65536
            );
        }
    }
}

#[test]
fn application_startup_uses_compiled_collectors_regardless_of_unknown_arguments() {
    let gui_args = ["--headless", "--future-gui", "opaque", "--root", "child-root", "--box2-diagnostic"];
    for extra_args in [&[][..], &["--future-mode", "opaque-a,opaque-b", "-Z", "--future=off"][..]] {
        let root = fixture("success");
        let output = Command::new(env!("CARGO_BIN_EXE_iman"))
            .args(extra_args)
            .args(["--root", root.path().to_str().unwrap(), "--"])
            .args(gui_args)
            .current_dir("/").output().unwrap();

        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let events = trace(root.path());
        assert_eq!(events.len(), 3);
        assert_eq!(&events[0]["args"].as_array().unwrap()[3..], json!(gui_args).as_array().unwrap());
        assert_eq!(String::from_utf8_lossy(&output.stderr).contains("ignoring unrecognized"), !extra_args.is_empty());
        assert_eq!(events[0]["bootstrap"]["diag_display"], cfg!(feature = "diag-display"));
        assert_eq!(events[0]["bootstrap"]["diag_input"], cfg!(feature = "diag-input"));
        #[cfg(feature = "diag-cycle")]
        {
            if iman::diagnostics::cycle::compiled_modules().is_empty() {
                assert!(!root.path().join("iman-reports").exists());
                continue;
            }
            let mut entries = fs::read_dir(root.path().join("iman-reports")).unwrap()
                .map(|entry| entry.unwrap().path()).collect::<Vec<_>>();
            entries.sort();
            assert_eq!(entries.len(), 2);
            for (i, entry) in entries.iter().enumerate() {
                let manifest: Value = serde_json::from_slice(&fs::read(entry.join("report.json")).unwrap()).unwrap();
                assert_eq!(manifest["metadata"]["compiled_modules"], json!(iman::diagnostics::cycle::compiled_modules()));
                assert_eq!(manifest["metadata"]["segment"]["sequence"], i + 1);
                for (file, compiled) in [
                    ("observation.json", cfg!(feature = "io-monitor")),
                    ("diag_perf.json", cfg!(feature = "diag-perf") && i == 0),
                    ("diag_memory.json", cfg!(feature = "diag-memory")),
                    ("diag_klog.json", cfg!(feature = "diag-klog")),
                    ("diag_display.json", cfg!(feature = "diag-display")),
                    ("diag_input.json", cfg!(feature = "diag-input")),
                ] { assert_eq!(entry.join(file).exists(), compiled, "{file}"); }
                let metadata = &manifest["metadata"];
                assert_eq!(metadata["clock_kind"], "monotonic");
                assert_eq!(metadata["clock_unit"], "milliseconds");
                let gui = &metadata["segment"]["gui_result"];
                assert!(gui["spawn_observed_ms"].is_u64());
                assert!(gui["bootstrap_received_ms"].is_u64());
                assert!(gui["exit_observed_ms"].is_u64());
                assert!(gui["spawn_observed_ms"].as_u64() <= gui["bootstrap_received_ms"].as_u64());
                assert!(gui["bootstrap_received_ms"].as_u64() <= gui["exit_observed_ms"].as_u64());
                assert_eq!(metadata["segment"]["gui_output"]["pid"], gui["pid"]);
                assert_eq!(metadata["segment"]["gui_output"]["timing"]["clock"], "iman_session");
                if cfg!(feature = "io-monitor") {
                    let context = &metadata["io_context"];
                    assert_eq!(context["clock"], "iman_session");
                    assert_eq!(context["scope"], "session");
                    assert!(context["samples"].as_u64().unwrap() > 0);
                    assert!(context["environment"]["ms"].is_u64());
                    assert!(!context["history"].as_array().unwrap().is_empty());
                    assert!(!entry.join("io_context.json").exists());
                } else {
                    assert!(metadata.get("io_context").is_none());
                }
                if !cfg!(feature = "io-monitor") {
                    assert_eq!(manifest["metadata"]["io_state"], "not_compiled");
                }
            }
        }
        #[cfg(not(feature = "diag-cycle"))]
        assert!(!root.path().join("iman-reports").exists());
    }
}

#[test]
fn relative_cli_root_is_canonical_in_gui_bootstrap() {
    let root = fixture("success");
    let mut child = Command::new(env!("CARGO_BIN_EXE_iman"))
        .arg("--root")
        .arg(root.path().file_name().unwrap())
        .args(["--", "--headless"])
        .current_dir(root.path().parent().unwrap())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();

    assert!(wait(&mut child).success());

    let events = trace(root.path());
    assert_eq!(events.len(), 3);
    assert_eq!(
        events[0]["bootstrap"]["root"],
        root.path().to_str().unwrap()
    );
}

#[test]
fn unknown_profile_keys_do_not_override_emulation_settings() {
    let root = fixture("success");
    let runtime = tempfile::tempdir_in("/dev/shm").unwrap();
    fs::write(
        root.path().join("runtime-parent"),
        runtime.path().to_str().unwrap(),
    )
    .unwrap();
    let profile = root.path().join("iman.json");
    fs::write(&profile, serde_json::to_vec(&json!({
        "version":1,"runtime_dir":runtime.path(),
        "gui":{"display":{"backend":"headless"}},
        "retroarch":{"video":"sdl2","audio":"alsa","input":"udev","joypad":"linuxraw","fullscreen":true}
    })).unwrap()).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_iman"))
        .arg("--root")
        .arg(root.path())
        .arg("--config")
        .arg(&profile)
        .current_dir("/")
        .spawn()
        .unwrap();

    assert!(wait(&mut child).success());

    let events = trace(root.path());
    assert_eq!(events.len(), 3);
    assert!(events[0]["args"].as_array().unwrap().contains(&json!("--headless")));
    let config = events[1]["config"].as_str().unwrap();
    assert!(config.contains("video_context_driver = \"kms\""));
    assert!(!config.contains("video_driver = \"sdl2\""));
    assert!(!config.contains("audio_driver = \"alsa\""));
    assert!(!config.contains("input_driver = \"udev\""));
    assert!(config.contains("config_save_on_exit = \"false\""));
    assert!(config.contains("video_context_driver"));
    assert_eq!(events[2]["bootstrap"]["state"]["platform"], "NES");
    assert_eq!(fs::read_dir(runtime.path()).unwrap().count(), 0);
}

struct LeaseTrace {
    root: std::path::PathBuf,
    calls: Vec<&'static str>,
    reclaim_count: usize,
    fail_reclaim: Option<usize>,
    fail_lend: bool,
    fail_finish: bool,
    _resource: fs::File,
}

impl iman::display::Lease for LeaseTrace {
    fn lend(&mut self) -> std::io::Result<()> {
        self.calls.push("lend");
        if self.fail_lend {
            Err(std::io::Error::other("lend refused"))
        } else {
            Ok(())
        }
    }
    fn reclaim(&mut self) -> std::io::Result<()> {
        self.calls.push("reclaim");
        self.reclaim_count += 1;
        // Even failing children must be reaped before the manager touches DRM.
        for event in trace(&self.root) {
            let pid = event["pid"].as_u64().unwrap() as libc::pid_t;
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        }
        if self.fail_reclaim == Some(self.reclaim_count) {
            Err(std::io::Error::other("foreign scanout"))
        } else {
            Ok(())
        }
    }
    fn finish(&mut self) -> std::io::Result<()> {
        self.calls.push("finish");
        if self.fail_finish {
            Err(std::io::Error::other("cleanup refused"))
        } else {
            Ok(())
        }
    }
    fn monitor_index(&self) -> Option<u32> {
        Some(2)
    }
}

#[test]
fn retained_session_lends_to_gui_game_gui_and_reclaims_only_after_reap() {
    use iman::{
        session::control::Control,
        session::{run_session_with_display, GameEnvironment},
    };
    use imanlib::igui::{GuiState, Handoff, LaunchRequest};
    for mode in [
        "success",
        "game-error",
        "exec-error",
        "gui-error",
        "gui-cancel",
        "lend-error",
        "reclaim-gui-error",
        "reclaim-game-error",
        "cleanup-error",
    ] {
        let root = fixture(if mode == "game-error" {
            "game-error"
        } else {
            "success"
        });
        fs::write(
            root.path().join("manager-resource"),
            b"synthetic retained FD",
        )
        .unwrap();
        let mut lease = LeaseTrace {
            root: root.path().to_path_buf(),
            calls: Vec::new(),
            reclaim_count: 0,
            fail_reclaim: match mode {
                "reclaim-gui-error" => Some(1),
                "reclaim-game-error" => Some(2),
                _ => None,
            },
            fail_lend: mode == "lend-error",
            fail_finish: mode == "cleanup-error",
            _resource: fs::File::open(root.path().join("manager-resource")).unwrap(),
        };
        // Test the error path after yield without weakening component checks.
        if mode == "exec-error" {
            fs::set_permissions(
                root.path().join("system/retroarch/retroarch"),
                fs::Permissions::from_mode(0o644),
            )
            .unwrap();
        }
        let mut control = Control::bind(root.path(), Path::new("/dev/shm")).unwrap();
        let mut visits = 0;

        let result = run_session_with_display(
            root.path(),
            GameEnvironment::Inherited, &mut control,
            &mut |_| {},
            &mut lease,
            |state, notice, _, _| {
                visits += 1;
                if visits > 1 {
                    assert_eq!(state.selected_hash, Some("ab".repeat(32)));
                    assert_eq!(
                        notice.is_some(),
                        matches!(mode, "game-error" | "exec-error")
                    );
                    if let Some(message) = notice {
                        assert!(message.contains(&format!("ROM: {}\n",
                            root.path().join("games/images/NES/ab/Game ' one.nes").display())));
                        assert!(!message.contains("/content/"));
                    }
                    return Ok(None);
                }
                if mode == "gui-error" {
                    return Err(std::io::Error::other("GUI failed"));
                }
                if mode == "gui-cancel" {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "cancelled",
                    ));
                }
                let mut child = Command::new("/bin/true").spawn().unwrap();
                let pid = child.id();
                assert!(child.wait().unwrap().success());
                fs::write(root.path().join("gui-pid"), pid.to_string()).unwrap();
                Ok(Some(Handoff {
                    state: GuiState {
                        selected_hash: Some("ab".repeat(32)),
                        ..GuiState::default()
                    },
                    game: LaunchRequest {
                        hash: "ab".repeat(32),
                        platform: "NES".into(),
                        image: "NES/ab/Game ' one.nes".into(),
                    },
                }))
            },
        );

        assert_eq!(
            result.is_ok(),
            matches!(mode, "success" | "game-error" | "exec-error"),
            "{mode}: {result:?}"
        );
        let expected = match mode {
            "lend-error" => vec!["lend", "finish"],
            "gui-error" | "gui-cancel" | "reclaim-gui-error" => vec!["lend", "reclaim", "finish"],
            "reclaim-game-error" => vec!["lend", "reclaim", "lend", "reclaim", "finish"],
            _ => vec![
                "lend", "reclaim", "lend", "reclaim", "lend", "reclaim", "finish",
            ],
        };
        assert_eq!(lease.calls, expected, "{mode}");
        if mode == "gui-cancel" {
            assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Interrupted);
        }
        for game in trace(root.path()) {
            assert!(game["config"]
                .as_str()
                .unwrap()
                .contains("video_monitor_index = \"2\""));
            assert!(!Path::new(game["runtime"].as_str().unwrap()).exists());
        }
        if matches!(
            mode,
            "lend-error" | "reclaim-gui-error" | "gui-error" | "gui-cancel"
        ) {
            assert!(trace(root.path()).is_empty());
        }
    }
}

#[test]
fn diagnostic_log_limit_does_not_cap_game_files_or_expand_parent_logs() {
    // Set process limits only in a dedicated test worker, never the test runner.
    const WORKER: &str = "IMAN_DIAGNOSTIC_LIMIT_FIXTURE";
    if std::env::var_os(WORKER).is_none() {
        let variants = ["compiled"];
        for variant in variants {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "diagnostic_log_limit_does_not_cap_game_files_or_expand_parent_logs",
                    "--nocapture",
                ])
                .env(WORKER, variant)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.stdout.len() < 4096);
            let log = String::from_utf8_lossy(&output.stderr);
            assert!(log.contains("captured=16384"));
            assert!(log.contains("fixture stdout"));
            assert!(log.contains("fixture stderr"));
            assert!(log.contains("RetroArch output end"));
            let metrics = log.lines().find_map(|s| s.strip_prefix("IMAN perf: "));
            assert_eq!(metrics.is_some(), cfg!(feature = "diag-perf"));
            if let Some(json) = metrics {
                assert!(json.len() <= 4096);
                let r: Value = serde_json::from_str(json).unwrap();
                assert_eq!(r["finished"], true);
                assert!(r["error"].is_null(), "{r}");
                assert!(r["samples"].as_u64().unwrap() >= 1);
                assert!(r["clock_ticks_per_second"].as_u64().unwrap() > 0);
            }
            for (name, compiled, limit) in [
                ("diag_memory", cfg!(feature = "diag-memory"), 8192),
                ("diag_klog", cfg!(feature = "diag-klog"), 32 * 1024),
                ("diag_display", cfg!(feature = "diag-display"), 8192),
                ("diag_input", cfg!(feature = "diag-input"), 8192),
            ] {
                let prefix = format!("IMAN {name}: ");
                let lines: Vec<_> = log
                    .lines()
                    .filter_map(|s| s.strip_prefix(&prefix))
                    .collect();
                assert_eq!(lines.len(), usize::from(compiled));
                if let Some(text) = lines.first() {
                    assert!(text.len() <= limit);
                    let report: Value = serde_json::from_str(text).unwrap();
                    assert_eq!(report["stopped"], true);
                    if let Some(samples) = report.get("samples") {
                        assert!(samples.as_u64().unwrap() > 0);
                    } else {
                        assert!(report["received"].is_u64());
                    }
                }
            }
            // The independent USB archive has a bounded additional RAM-log budget.
            let usb_budget = if cfg!(feature = "diag-klog") { 28 * 1024 } else { 0 };
            assert!(output.stderr.len() < 24000 + usb_budget);
        }
        return;
    }
    use iman::{
        session::control::Control,
        session::{self, GameEnvironment},
    };
    use imanlib::igui::{GuiState, Handoff, LaunchRequest};
    let root = fixture("success");
    let runtime = tempfile::tempdir_in("/dev/shm").unwrap();
    write_executable(
        &root.path().join("system/retroarch/retroarch"),
        include_str!("common/large_game.py"),
    );
    // Simulate iman startup's soft limit while retaining an inherited larger hard limit.
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_FSIZE, &mut limit) },
        0
    );
    assert!(limit.rlim_max >= 131072);
    limit.rlim_cur = 65536;
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_FSIZE, &limit) }, 0);
    session::install_signal_handlers().unwrap();
    let mut control = Control::bind(root.path(), runtime.path()).unwrap();
    #[cfg(any(feature = "diag-memory", feature = "diag-klog", feature = "diag-display", feature = "diag-input"))]
    control.enable_diagnostics().unwrap();
    #[cfg(feature = "io-monitor")]
    control.enable_monitor(vec!["iman_missing_perf_fixture".into()]);
    let mut rounds = 0;

    session::run_session(
        root.path(),
        GameEnvironment::DiagnosticParent, &mut control,
        &mut |_| {},
        |state, notice, _, _| {
            rounds += 1;
            assert!(notice.is_none(), "{notice:?}");
            if rounds == 2 {
                assert_eq!(
                    state.selected_hash.as_deref(),
                    Some("ab".repeat(32).as_str())
                );
                return Ok(None);
            }
            Ok(Some(Handoff {
                state: GuiState {
                    selected_hash: Some("ab".repeat(32)),
                    ..GuiState::default()
                },
                game: LaunchRequest {
                    hash: "ab".repeat(32),
                    platform: "NES".into(),
                    image: "NES/ab/Game ' one.nes".into(),
                },
            }))
        },
    )
    .unwrap();

    assert_eq!(rounds, 2);
    assert_eq!(
        fs::read_to_string(root.path().join("large-file-size")).unwrap(),
        "131072"
    );
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_FSIZE, &mut limit) },
        0
    );
    assert_eq!(limit.rlim_cur, 65536);
    control.finish(None, false).unwrap();
    control.finish(None, false).unwrap(); // Reports are emitted once, not per finish call.
    drop(control);
    assert_eq!(fs::read_dir(runtime.path()).unwrap().count(), 0);
}

#[test]
fn emulation_file_drives_games_without_retroarch_section_in_manager_profile() {
    let root = fixture("success");
    let manifest = root.path().join("system/iman/emulation.json");
    let mut document: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    document["settings"] = settings_fixture::create_settings();
    fs::write(&manifest, serde_json::to_vec(&document).unwrap()).unwrap();
    let profile = root.path().join("iman.json");
    fs::write(&profile, br#"{"version":1,"gui":{"display":{"backend":"headless"}}}"#).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_iman"))
        .arg("--root").arg(root.path()).arg("--config").arg(&profile)
        .current_dir("/").spawn().unwrap();

    assert!(wait(&mut child).success());

    let events = trace(root.path());
    assert_eq!(events.len(), 3);
    let config = events[1]["config"].as_str().unwrap();
    assert!(config.contains("audio_driver = \"alsa\"\n"));
    assert!(config.contains("video_autoswitch_refresh_rate = \"2\"\n"));
    assert!(config.contains("config_save_on_exit = \"false\"\n"));
    assert_eq!(events[2]["event"], "gui");
}

#[cfg(feature = "diag-cycle")]
#[test]
fn automatic_segments_cover_repeated_games_and_tail_before_display_release() {
    use iman::{session::control::Control, display::Lease, session::{self, GameEnvironment}};
    use imanlib::igui::{GuiState, Handoff, LaunchRequest};
    use std::sync::{Arc, Mutex};
    for mode in ["success", "game-error", "exec-error", "write-error", "gui-only"] {
        let root = fixture(if mode == "game-error" { "game-error" } else { "success" });
        write_executable(&root.path().join("system/retroarch/retroarch"),
            &format!("import sys\nprint(\"synthetic game output\")\nsys.exit(7 if {mode:?} == \"game-error\" else 0)\n"));
        if mode == "exec-error" {
            fs::set_permissions(root.path().join("system/retroarch/retroarch"), fs::Permissions::from_mode(0o644)).unwrap();
        }
        let reports = tempfile::tempdir_in("/dev/shm").unwrap();
        let card = Arc::new(fs::File::open(reports.path()).unwrap());
        let events = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorded = events.clone();
        let writer: iman::diagnostics::cycle::Writer = Arc::new(move |run, bundle| {
            let mut events = recorded.lock().unwrap();
            assert_eq!(events.last().map(String::as_str), Some("Saving diagnostics..."));
            events.push("write".into());
            drop(events);
            assert!(!String::from_utf8_lossy(&bundle.files["launch.log"]).contains("synthetic game output"));
            if mode == "write-error" { return Err(std::io::Error::other("synthetic storage failure")); }
            iman::diagnostics::export::write_bundle(&card, run, bundle)
        });
        struct Screen(Arc<Mutex<Vec<String>>>);
        impl Lease for Screen {
            fn lend(&mut self) -> std::io::Result<()> { self.0.lock().unwrap().push("lend".into()); Ok(()) }
            fn reclaim(&mut self) -> std::io::Result<()> { self.0.lock().unwrap().push("reclaim".into()); Ok(()) }
            fn finish(&mut self) -> std::io::Result<()> { self.0.lock().unwrap().push("finish".into()); Ok(()) }
            fn export_status(&mut self, message: &str, _: bool) -> std::io::Result<bool> {
                self.0.lock().unwrap().push(message.into()); Ok(false)
            }
        }
        let mut screen = Screen(events.clone());
        let mut control = Control::bind(root.path(), Path::new("/dev/shm")).unwrap();
        control.enable_cycle_exports(writer, Duration::ZERO).unwrap();
        let mut visits = 0;

        session::run_session_with_display(root.path(), GameEnvironment::DiagnosticParent, &mut control,
            &mut |_| {}, &mut screen, |_, notice, _, _| {
                visits += 1;
                if visits > 1 {
                    assert_eq!(notice.is_some(), matches!(mode, "game-error" | "exec-error" | "write-error"));
                }
                if visits == 3 || mode == "gui-only" { return Ok(None); }
                Ok(Some(Handoff { state:GuiState::default(), game:LaunchRequest {
                    hash:"ab".repeat(32), platform:"NES".into(), image:"NES/ab/Game ' one.nes".into(),
                }}))
            }).unwrap();
        assert_eq!(control.finish(None, false).is_err(), mode == "write-error");

        let events = events.lock().unwrap();
        assert_eq!(events.last().map(String::as_str), Some("finish"));
        assert_eq!(events.iter().filter(|e| e.as_str() == "write").count(),
            if mode == "write-error" || mode == "gui-only" {1} else {3});
        if mode == "write-error" {
            assert!(!events.iter().any(|e| e == "Diagnostics synchronized."));
            assert_eq!(control.cycle_status()["failed_snapshot_retained"], true);
            continue;
        }
        let mut directories = fs::read_dir(reports.path().join("iman-reports")).unwrap()
            .map(|e| e.unwrap().path()).collect::<Vec<_>>();
        directories.sort();
        for (i, directory) in directories.iter().enumerate() {
            let manifest: Value = serde_json::from_slice(&fs::read(directory.join("report.json")).unwrap()).unwrap();
            let metadata = &manifest["metadata"];
            assert_eq!(metadata["segment"]["sequence"], i + 1);
            assert_eq!(metadata["previous_export"].is_null(), i == 0);
            let tail = i + 1 == directories.len();
            assert_eq!(metadata["segment"]["game"].is_null(), tail);
            for (name, compiled) in [
                ("diag_memory.json", cfg!(feature = "diag-memory")),
                ("diag_klog.json", cfg!(feature = "diag-klog")),
                ("diag_display.json", cfg!(feature = "diag-display")),
                ("diag_input.json", cfg!(feature = "diag-input")),
            ] { assert_eq!(directory.join(name).exists(), compiled); }
            if !tail {
                assert_eq!(directory.join("diag_perf.json").exists(), cfg!(feature = "diag-perf"));
                if mode != "exec-error" {
                    assert!(fs::read_to_string(directory.join("retroarch.log")).unwrap().contains("synthetic game output"));
                    assert_eq!(metadata["segment"]["game_result"]["exit_code"], if mode == "game-error" {7} else {0});
                    let result = &metadata["segment"]["game_result"];
                    let output = &metadata["segment"]["output"];
                    assert_eq!(output["pid"], result["pid"]);
                    assert_eq!(output["timing"]["clock"], "iman_session");
                    assert_eq!(output["timing"]["chunks_dropped"], 0);
                    let chunks = output["timing"]["chunks"].as_array().unwrap();
                    assert!(!chunks.is_empty());
                    assert!(result["spawn_observed_ms"].as_u64() <= chunks[0]["ms"].as_u64());
                    assert!(result["spawn_observed_ms"].as_u64() <= result["exit_observed_ms"].as_u64());
                } else {
                    assert!(metadata["segment"]["game_result"]["launch_or_cleanup_error"].as_str().unwrap().contains("executable"));
                }
            }
        }
    }
}

#[cfg(feature = "diag-cycle")]
#[test]
fn cancellation_waits_for_accepted_export_and_does_not_start_another_write() {
    const WORKER: &str = "IMAN_CYCLE_CANCEL_FIXTURE";
    if std::env::var_os(WORKER).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cancellation_waits_for_accepted_export_and_does_not_start_another_write", "--nocapture"])
            .env(WORKER, "1").output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        return;
    }
    use iman::{session::control::Control, session::{self, GameEnvironment}};
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    session::install_signal_handlers().unwrap();
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut control = Control::bind(root.path(), root.path()).unwrap();
    control.enable_cycle_exports(Arc::new(move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        unsafe { libc::kill(libc::getpid(), libc::SIGTERM); }
        thread::sleep(Duration::from_millis(40));
        Ok("synthetic synchronized snapshot".into())
    }), Duration::ZERO).unwrap();

    let error = session::run_session(root.path(), GameEnvironment::Inherited, &mut control,
        &mut |_| {}, |_, _, _, _| Ok(None)).unwrap_err();
    control.finish(Some(error.to_string()), true).unwrap();

    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!control.cycle_pending());
    assert_eq!(control.cycle_status()["saved"], 1);
}

#[cfg(feature = "diag-cycle")]
#[test]
fn failed_gui_reclaim_preserves_evidence_without_drawing_on_unknown_route() {
    use iman::{session::control::Control, display::Lease, session::{self, GameEnvironment}};
    use std::sync::{Arc, Mutex};
    struct LostDisplay;
    impl Lease for LostDisplay {
        fn lend(&mut self) -> std::io::Result<()> { Ok(()) }
        fn reclaim(&mut self) -> std::io::Result<()> { Err(std::io::Error::other("synthetic foreign route")) }
        fn finish(&mut self) -> std::io::Result<()> { Ok(()) }
        fn export_status(&mut self, _: &str, _: bool) -> std::io::Result<bool> { panic!("unknown display route"); }
    }
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let saved = Arc::new(Mutex::new(Vec::new()));
    let reports = saved.clone();
    let mut control = Control::bind(root.path(), root.path()).unwrap();
    control.enable_cycle_exports(Arc::new(move |_, bundle| {
        reports.lock().unwrap().push(bundle.metadata.clone()); Ok("synthetic".into())
    }), Duration::ZERO).unwrap();

    let error = session::run_session_with_display(root.path(), GameEnvironment::Inherited,
        &mut control, &mut |_| {}, &mut LostDisplay, |_, _, control, _| {
            control.begin_gui(std::process::id(), Default::default(), None);
            Err(std::io::Error::other("synthetic failed child already reaped"))
        }).unwrap_err();
    control.finish(Some(error.to_string()), false).unwrap();

    assert!(error.to_string().contains("foreign route"));
    let saved = saved.lock().unwrap();
    assert_eq!(saved.len(), 1);
    assert!(saved[0]["session_error"].as_str().unwrap().contains("foreign route"));
    assert!(saved[0]["segment"]["gui_result"]["launch_or_cleanup_error"].as_str().unwrap().contains("failed child"));
}

#[cfg(feature = "diag-cycle")]
#[test]
fn cycle_output_keeps_original_head_and_late_failure_after_large_output() {
    use iman::{session::control::Control, session::{self, GameEnvironment}};
    use imanlib::igui::{Handoff, LaunchRequest};
    use std::sync::{Arc, Mutex};
    let root = fixture("success");
    write_executable(&root.path().join("system/retroarch/retroarch"),
        r#"import os
os.write(1, b"HEAD\n")
os.write(1, b"x" * 100000)
os.write(2, b"\nLATE FAILURE\n")
"#);
    let saved = Arc::new(Mutex::new(Vec::new()));
    let reports = saved.clone();
    let mut control = Control::bind(root.path(), Path::new("/dev/shm")).unwrap();
    control.enable_cycle_exports(Arc::new(move |_, bundle| {
        if let Some(bytes) = bundle.files.get("retroarch.log") {
            reports.lock().unwrap().push((bytes.clone(), bundle.metadata["segment"]["output"].clone()));
        }
        Ok("synthetic".into())
    }), Duration::ZERO).unwrap();
    let mut visits = 0;

    session::run_session(root.path(), GameEnvironment::DiagnosticParent, &mut control,
        &mut |_| {}, |_, _, _, _| {
            visits += 1;
            if visits > 1 { return Ok(None); }
            Ok(Some(Handoff {state:Default::default(),game:LaunchRequest {
                hash:"ab".repeat(32),platform:"NES".into(),image:"NES/ab/Game ' one.nes".into(),
            }}))
        }).unwrap();

    let saved = saved.lock().unwrap();
    assert_eq!(saved.len(), 1);
    assert!(saved[0].0.starts_with(b"HEAD\n"));
    assert!(saved[0].0.ends_with(b"LATE FAILURE\n"));
    assert!(saved[0].0.len() < 25 * 1024);
    assert!(saved[0].1["omitted_bytes"].as_u64().unwrap() > 0);
    assert_eq!(saved[0].1["head_bytes"], 16 * 1024);
    assert_eq!(saved[0].1["drain_complete"], true);
}

#[test]
fn installed_cli_infers_root_loads_profile_and_ignores_former_board_flag() {
    let root = fixture("success");
    let ram = tempfile::tempdir_in("/dev/shm").unwrap();
    let runtime = ram.path().join("runtime");
    let manager = root.path().join("system/iman/iman");
    fs::copy(env!("CARGO_BIN_EXE_iman"), &manager).unwrap();
    fs::write(root.path().join("runtime-parent"), runtime.to_str().unwrap()).unwrap();
    fs::write(root.path().join("system/iman/iman.json"), serde_json::to_vec(&json!({
        "runtime_dir":runtime, "gui":{"display":{"backend":"headless"}}
    })).unwrap()).unwrap();

    let output = Command::new(&manager).arg("--box2-diagnostic").current_dir("/").output().unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(trace(root.path()).len(), 3);
    assert!(String::from_utf8_lossy(&output.stderr).contains("ignoring unrecognized"));
    assert_eq!(fs::read_dir(&runtime).unwrap().count(), 0);
    assert!(trace(root.path())[0]["args"].as_array().unwrap().contains(&json!("--headless")));
}

#[test]
fn common_startup_survives_closed_standard_descriptors() {
    use std::os::unix::process::CommandExt;
    let root = fixture("success");
    let mut command = Command::new(env!("CARGO_BIN_EXE_iman"));
    command.arg("--root").arg(root.path()).args(["--", "--headless"]);
    unsafe {
        command.pre_exec(|| {
            for fd in [0, 1, 2] { libc::close(fd); }
            Ok(())
        });
    }

    let status = wait(&mut command.spawn().unwrap());

    assert!(status.success());
    assert_eq!(trace(root.path()).len(), 3);
}

#[test]
fn game_receives_selected_remap_before_exec_and_runtime_is_removed_after_exit() {
    let root = fixture("success");
    let manifest = root.path().join("system/iman/emulation.json");
    let mut document: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    document["cores"]["NES"]["library_name"] = json!("Synthetic Core");
    document["cores"]["NES"]["remap"] = json!({"input_player1_btn_x": 10});
    fs::write(&manifest, serde_json::to_vec(&document).unwrap()).unwrap();

    assert!(wait(&mut spawn(root.path())).success());

    let events = trace(root.path());
    let game = events.iter().find(|event| event["event"] == "game").unwrap();
    assert_eq!(game["remaps"], json!({"remaps/Synthetic Core/Synthetic Core.rmp": "input_player1_btn_x = \"10\"\n"}));
    assert!(game["config"].as_str().unwrap().contains("auto_remaps_enable = \"true\""));
    assert!(!Path::new(game["runtime"].as_str().unwrap()).exists());
    assert_eq!(fs::read_dir(root.path().join("games/states")).unwrap().count(), 0);
}

#[test]
fn synthetic_frontend_loads_sram_and_rtc_again_after_a_new_session_separately_from_states() {
    let root = fixture("success");
    write_executable(&root.path().join("system/retroarch/retroarch"), include_str!("common/sram_game.py"));

    assert!(wait(&mut spawn(root.path())).success());
    assert!(wait(&mut spawn(root.path())).success());

    let reads: Vec<Value> = fs::read_to_string(root.path().join("save-trace")).unwrap().lines()
        .map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(reads, vec![json!({"sram":"","rtc":""}),
        json!({"sram":"synthetic progress;","rtc":"synthetic clock;"})]);
    let save = root.path().join("games/saves").join(format!("{}.srm", "ab".repeat(32)));
    assert_eq!(fs::read(save).unwrap(), b"synthetic progress;synthetic progress;");
    assert_eq!(fs::read_dir(root.path().join("games/states")).unwrap().count(), 0);
    assert!(!root.path().join("saves").exists());
}
