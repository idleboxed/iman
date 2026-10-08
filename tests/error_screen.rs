// SPDX-License-Identifier: BSD-3-Clause
//! Synthetic lifecycle/display failures; no DRM, SD or real IGUI execution.
use iman::{
    session::control::Control,
    display::{leaving_seconds, Lease},
    session::{self, GameEnvironment},
    session::timing::Timing,
};
use imanlib::{client::Client, protocol::Outcome};
use serde_json::json;
use std::{io, path::{Path, PathBuf}, time::Duration};
#[cfg(feature = "diag-input")]
use std::time::Instant;

#[path = "common/timing.rs"]
mod test_timing;
use test_timing::ManualTiming;

#[derive(Default)]
struct Screen {
    events: Vec<String>,
    path: Option<PathBuf>,
    reason: Option<String>,
    visible: bool,
    failure: Option<&'static str>,
}

impl Screen {
    fn call(&mut self, operation: &'static str) -> io::Result<()> {
        self.events.push(operation.into());
        if self.failure == Some(operation) {
            Err(io::Error::other(operation))
        } else {
            Ok(())
        }
    }
}

impl Lease for Screen {
    fn starting_gui(&mut self) -> io::Result<()> { self.call("starting") }
    fn lend(&mut self) -> io::Result<()> { self.call("lend") }
    fn reclaim(&mut self) -> io::Result<()> { self.call("reclaim") }
    fn finish(&mut self) -> io::Result<()> { self.call("finish") }
    fn gui_error(&mut self, path: &Path, error: &io::Error) -> io::Result<bool> {
        self.call("error")?;
        self.path = Some(path.into());
        self.reason = Some(error.to_string());
        Ok(self.visible)
    }
    fn error_tick(&mut self, remaining: u8) -> io::Result<()> {
        self.events.push(format!("tick:{remaining}"));
        self.call("tick")
    }
    fn diagnostic_status(&mut self, message: &str, _elapsed_seconds: u64) -> io::Result<()> {
        self.events.push(message.into());
        self.call("diagnostics")
    }
}

#[test]
fn countdown_uses_elapsed_seconds_then_leaves_one_second_for_the_farewell() {
    for seconds in 0..=10 {
        for millis in [0, 999] {
            assert_eq!(leaving_seconds(Duration::from_millis(seconds * 1000 + millis)), Some(10 - seconds as u8));
        }
    }
    assert_eq!(leaving_seconds(Duration::from_secs(11)), None);
    assert_eq!(leaving_seconds(Duration::MAX), None);
}

#[test]
fn failure_screen_requires_reclaim_and_never_delays_cancellation_or_unmanaged_sessions() {
    for failure in [None, Some("starting"), Some("reclaim"), Some("error"), Some("cancel")] {
        let root = tempfile::tempdir_in("/dev/shm").unwrap();
        let mut control = Control::bind(root.path(), root.path()).unwrap();
        let mut screen = Screen { failure, ..Screen::default() };
        let mut launches = 0;

        let error = session::run_session_with_display(
            root.path(), GameEnvironment::Inherited, &mut control,
            &mut |_| panic!("invisible/cancelled error must not wait"), &mut screen,
            |_, _, _, _| {
                launches += 1;
                Err(io::Error::new(
                    if failure == Some("cancel") { io::ErrorKind::Interrupted } else { io::ErrorKind::NotFound },
                    "synthetic missing GUI",
                ))
            },
        ).unwrap_err();

        let expected = match failure {
            Some("starting") => vec!["starting", "finish"],
            Some("reclaim" | "cancel") => vec!["starting", "lend", "reclaim", "finish"],
            _ => vec!["starting", "lend", "reclaim", "error", "finish"],
        };
        assert_eq!(screen.events, expected);
        assert_eq!(launches, usize::from(failure != Some("starting")));
        if failure.is_none() {
            assert_eq!(screen.path, Some(root.path().join("system/igui/igui")));
            assert_eq!(error.kind(), io::ErrorKind::NotFound);
            assert_eq!(screen.reason.as_deref(), Some("synthetic missing GUI"));
        }
        if failure == Some("error") {
            assert!(error.to_string().contains("synthetic missing GUI; error screen: error"));
        }
    }
}

#[test]
fn visible_error_counts_down_once_then_restores_without_retrying_gui() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let mut control = Control::bind(root.path(), root.path()).unwrap();
    let mut client = Client::connect(control.path()).unwrap();
    client.send("core.status", json!({})).unwrap();
    let mut status_checked = false;
    let mut screen = Screen { visible: true, ..Screen::default() };
    let mut launches = 0;
    let mut timing = ManualTiming::default();
    let observed_timing = timing.clone();

    let error = session::run_session_with_timing(
        root.path(), GameEnvironment::Inherited, &mut control,
        &mut |phase| {
            assert_eq!(phase, "gui_error");
            assert!(observed_timing.now() < Duration::from_secs(20));
            if let Some(response) = client.poll().unwrap() {
                let Outcome::Done { result } = response.outcome else { panic!("status failed") };
                assert_eq!(result["phase"], "gui_error");
                assert!(result["child_pid"].is_null());
                status_checked = true;
            }
        },
        &mut screen,
        &mut timing,
        |_, _, control, _| {
            launches += 1;
            // A timed-out GUI may have left its registration behind after reap.
            control.begin_gui(u32::MAX, Default::default(), None);
            Err(io::Error::new(io::ErrorKind::NotFound, "synthetic missing GUI"))
        },
    ).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert_eq!(launches, 1);
    assert!(status_checked);
    assert_eq!(timing.now(), Duration::from_secs(11));
    let ticks: Vec<_> = screen.events.iter().filter(|event| event.starts_with("tick:")).cloned().collect();
    assert_eq!(ticks, (0..=10).rev().map(|seconds| format!("tick:{seconds}")).collect::<Vec<_>>());
    assert_eq!(screen.events.last().unwrap(), "finish");
}

#[test]
fn countdown_render_failure_stops_without_retry_and_preserves_original_error() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let mut control = Control::bind(root.path(), root.path()).unwrap();
    let mut screen = Screen { visible: true, failure: Some("tick"), ..Screen::default() };

    let error = session::run_session_with_display(
        root.path(), GameEnvironment::Inherited, &mut control, &mut |_| {}, &mut screen,
        |_, _, _, _| Err(io::Error::other("synthetic launch failure")),
    ).unwrap_err();

    assert!(error.to_string().contains("synthetic launch failure; error countdown: tick"));
    assert_eq!(screen.events, ["starting", "lend", "reclaim", "error", "tick:10", "tick", "finish"]);
}

#[test]
fn signal_cancels_countdown_and_restores_display_without_waiting_ten_seconds() {
    const WORKER: &str = "IMAN_SPLASH_SIGNAL_FIXTURE";
    if std::env::var_os(WORKER).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "signal_cancels_countdown_and_restores_display_without_waiting_ten_seconds"])
            .env(WORKER, "1").status().unwrap();
        assert!(status.success());
        return;
    }
    session::install_signal_handlers().unwrap();
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let mut control = Control::bind(root.path(), root.path()).unwrap();
    let mut screen = Screen { visible: true, ..Screen::default() };

    let error = session::run_session_with_display(
        root.path(), GameEnvironment::Inherited, &mut control,
        &mut |_| { assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0); }, &mut screen,
        |_, _, _, _| Err(io::Error::other("synthetic launch failure")),
    ).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(screen.events, ["starting", "lend", "reclaim", "error", "tick:10", "tick", "finish"]);
}

#[cfg(feature = "diag-input")]
fn enable_input(control: &mut Control) -> Client {
    let mut client = Client::connect(control.path()).unwrap();
    control.enable_diagnostics().unwrap();
    client.send("diag_input.report", json!({})).unwrap();
    let began = Instant::now();
    loop {
        control.poll("session_start");
        if let Some(response) = client.poll().unwrap() {
            assert!(matches!(response.outcome, Outcome::Done { .. }));
            return client;
        }
        assert!(began.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
#[cfg(feature = "diag-input")]
fn enabled_diagnostics_finish_ten_seconds_before_departure_without_gui_reports() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let mut control = Control::bind(root.path(), root.path()).unwrap();
    let _client = enable_input(&mut control);
    let mut screen = Screen { visible: true, ..Screen::default() };
    let mut timing = ManualTiming::default();
    let observed_timing = timing.clone();
    let mut departure = None;
    let mut launches = 0;

    let error = session::run_session_with_timing(
        root.path(), GameEnvironment::Inherited, &mut control,
        &mut |phase| {
            assert!(observed_timing.now() < Duration::from_secs(30));
            if phase == "gui_error" && departure.is_none() {
                departure = Some(observed_timing.now());
            }
        }, &mut screen, &mut timing,
        |_, _, _, _| {
            launches += 1;
            Err(io::Error::new(io::ErrorKind::NotFound, "synthetic missing GUI"))
        },
    ).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert_eq!(launches, 1);
    assert_eq!(departure.unwrap(), Duration::from_secs(10));
    assert_eq!(timing.now() - departure.unwrap(), Duration::from_secs(11));
    let first_tick = screen.events.iter().position(|event| event == "tick:10").unwrap();
    let statuses: Vec<_> = screen.events[..first_tick].iter()
        .filter(|event| event.starts_with("IGUI unavailable.")).collect();
    assert_eq!(statuses.len(), 10);
    for (seconds, status) in statuses.into_iter().enumerate() {
        assert!(status.starts_with(&format!("IGUI unavailable. Diagnostics {seconds}/10 s")));
        assert!(!status.contains("Input:"));
    }
    assert!(!screen.events[first_tick..].iter().any(|event| event == "diagnostics"));
    assert!(control.diagnostics_status().is_empty());
    assert_eq!(screen.events.last().unwrap(), "finish");
}

#[test]
fn normal_launch_adapter_handles_a_real_missing_igui_with_the_same_display_lifecycle() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let mut control = Control::bind(root.path(), root.path()).unwrap();
    let mut screen = Screen::default();

    let error = session::run_controlled_with_display(
        root.path(), &[], &mut control, &mut |_| {}, &mut screen,
    ).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert_eq!(screen.path, Some(root.path().join("system/igui/igui")));
    assert_eq!(screen.events, ["starting", "lend", "reclaim", "error", "finish"]);
    assert!(!root.path().join("system").exists());
}

#[test]
#[cfg(feature = "diag-input")]
fn diagnostic_screen_failure_restores_without_countdown_or_retry() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let mut control = Control::bind(root.path(), root.path()).unwrap();
    let _client = enable_input(&mut control);
    let mut screen = Screen { visible: true, failure: Some("diagnostics"), ..Screen::default() };

    let error = session::run_session_with_display(
        root.path(), GameEnvironment::Inherited, &mut control, &mut |_| {}, &mut screen,
        |_, _, _, _| Err(io::Error::new(io::ErrorKind::NotFound, "synthetic missing GUI")),
    ).unwrap_err();

    assert!(error.to_string().contains("synthetic missing GUI; diagnostic screen: diagnostics"));
    assert!(!screen.events.iter().any(|e| e.starts_with("tick:")));
    assert_eq!(screen.events.last().unwrap(), "finish");
}

#[test]
#[cfg(feature = "diag-input")]
fn signal_cancels_diagnostics_before_countdown() {
    const WORKER: &str = "IMAN_DIAGNOSTICS_SIGNAL_FIXTURE";
    if std::env::var_os(WORKER).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "signal_cancels_diagnostics_before_countdown"])
            .env(WORKER, "1").status().unwrap();
        assert!(status.success());
        return;
    }
    session::install_signal_handlers().unwrap();
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let mut control = Control::bind(root.path(), root.path()).unwrap();
    let _client = enable_input(&mut control);
    let mut screen = Screen { visible: true, ..Screen::default() };

    let error = session::run_session_with_display(
        root.path(), GameEnvironment::Inherited, &mut control,
        &mut |phase| {
            assert_eq!(phase, "gui_diagnostics");
            assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
        }, &mut screen,
        |_, _, _, _| Err(io::Error::new(io::ErrorKind::NotFound, "synthetic missing GUI")),
    ).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(screen.events, ["starting", "lend", "reclaim", "error", "finish"]);
}
