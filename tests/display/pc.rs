// SPDX-License-Identifier: BSD-3-Clause
//! Fake window and headless CLI checks; no live X11, DRM or input devices.
#![cfg(feature = "pc")]
use std::{cell::RefCell, io, path::Path, process::Command, rc::Rc};
use iman::{session::control::Control, display::{pc::{WindowBackend, WindowDisplay}, splash, Lease}, session};

#[derive(Default)]
struct State {
    open: bool,
    events: Vec<&'static str>,
    pixels: Vec<u32>,
    fail_present: bool,
    polls: usize,
    cancel_at: usize,
}

struct Fake(Rc<RefCell<State>>);
impl WindowBackend for Fake {
    fn open(&mut self, width: usize, height: usize) -> io::Result<()> {
        assert_eq!((width, height), (1280, 720));
        let mut state = self.0.borrow_mut();
        assert!(!state.open);
        state.open = true;
        state.events.push("open");
        Ok(())
    }
    fn present(&mut self, pixels: &[u32], width: usize, height: usize) -> io::Result<()> {
        let mut state = self.0.borrow_mut();
        assert!(state.open);
        assert_eq!(pixels.len(), width * height);
        state.events.push("present");
        if state.fail_present {
            return Err(io::Error::other("synthetic window submission failed"));
        }
        state.pixels.clear();
        state.pixels.extend_from_slice(pixels);
        Ok(())
    }
    fn poll_events(&mut self) -> io::Result<()> {
        let mut state = self.0.borrow_mut();
        if state.open {
            state.polls += 1;
            if state.polls == state.cancel_at {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "synthetic window closed"));
            }
        }
        Ok(())
    }
    fn close(&mut self) {
        let mut state = self.0.borrow_mut();
        if state.open {
            state.events.push("close");
            state.open = false;
            state.pixels.clear();
        }
    }
}

#[test]
fn pc_uses_shared_frames_for_initialization_error_diagnostics_and_countdown() {
    let state = Rc::new(RefCell::new(State::default()));
    let mut display = WindowDisplay::open(Fake(state.clone()), false).unwrap();
    assert_eq!(state.borrow().pixels, splash::frame(splash::INITIALIZING, false).pixels());

    display.starting_gui().unwrap();
    assert_eq!(state.borrow().pixels, splash::frame(splash::STARTING, false).pixels());
    let error = io::Error::other("IGUI exited with signal: 25 (SIGXFSZ)");
    display.gui_error(Path::new("/missing/system/igui/igui"), &error).unwrap();
    let message = iman::display::gui_error_message(Path::new("/missing/system/igui/igui"), &error);
    let mut expected = splash::frame(&message, true);
    assert_eq!(state.borrow().pixels, expected.pixels());
    display.diagnostic_status("Diagnostics 3/10 s\nRAM: 2 samples, 0 read errors", 3).unwrap();
    splash::draw_diagnostics(&mut expected, "Diagnostics 3/10 s\nRAM: 2 samples, 0 read errors", 3).unwrap();
    assert_eq!(state.borrow().pixels, expected.pixels());

    display.error_tick(10).unwrap();
    splash::draw(&mut expected, &message, true).unwrap();
    splash::countdown(&mut expected, 10).unwrap();
    assert_eq!(state.borrow().pixels, expected.pixels());
    display.error_tick(0).unwrap();
    splash::countdown(&mut expected, 0).unwrap();
    assert_eq!(state.borrow().pixels, expected.pixels());
    assert!(display.take_diagnostics().is_none());
}

#[test]
fn window_is_closed_while_lent_and_released_on_finish_or_drop() {
    let state = Rc::new(RefCell::new(State::default()));
    let mut display = WindowDisplay::open(Fake(state.clone()), false).unwrap();
    display.lend().unwrap();
    assert!(!state.borrow().open);
    assert!(state.borrow().pixels.is_empty());
    display.poll_events().unwrap();
    assert_eq!(state.borrow().polls, 0);

    display.reclaim().unwrap();
    display.starting_gui().unwrap();
    display.finish().unwrap();
    drop(display);

    assert_eq!(state.borrow().events, ["open", "present", "close", "open", "present", "close"]);
    let display = WindowDisplay::open(Fake(state.clone()), false).unwrap();
    drop(display);
    assert!(!state.borrow().open);
}

#[test]
fn initial_presentation_failure_releases_the_window() {
    let state = Rc::new(RefCell::new(State { fail_present: true, ..State::default() }));

    let result = WindowDisplay::open(Fake(state.clone()), false);

    assert!(result.err().unwrap().to_string().contains("synthetic window submission failed"));
    assert_eq!(state.borrow().events, ["open", "present", "close"]);
}

#[test]
fn window_close_cancels_the_real_missing_gui_session_and_cleans_up() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let mut control = Control::bind(root.path(), root.path()).unwrap();
    let state = Rc::new(RefCell::new(State { cancel_at: 2, ..State::default() }));
    let mut display = WindowDisplay::open(Fake(state.clone()), false).unwrap();

    let error = session::run_controlled_with_display(
        root.path(), &[], &mut control, &mut |_| {}, &mut display,
    ).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(state.borrow().polls, 2);
    assert!(!state.borrow().open);
    assert!(!root.path().join("system").exists());
}

#[test]
#[cfg(feature = "diag-display")]
fn pc_metrics_are_opt_in_desktop_only_and_record_real_submission_failures() {
    let state = Rc::new(RefCell::new(State::default()));
    let mut display = WindowDisplay::open(Fake(state.clone()), true).unwrap();
    let (_, sample) = display.take_diagnostics().unwrap();
    assert_eq!(sample.backend, imanlib::display_metrics::Backend::Desktop);
    assert!(sample.kms.is_none());
    assert_eq!(sample.render.unwrap().requests, 1);
    assert_eq!(sample.present.unwrap().count, 1);

    state.borrow_mut().fail_present = true;
    assert!(display.starting_gui().is_err());
    display.finish().unwrap();
    let (_, sample) = display.take_diagnostics().unwrap();
    assert_eq!(sample.operation_ok, Some(false));
    assert_eq!(sample.cleanup_ok, Some(true));
    display.observe_display(false);
    assert!(display.take_diagnostics().is_none());
}

#[test]
#[cfg(feature = "diag-display")]
fn window_events_remain_responsive_during_diagnostics_before_countdown() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let mut control = Control::bind(root.path(), root.path()).unwrap();
    control.enable_diagnostics().unwrap();
    let state = Rc::new(RefCell::new(State { cancel_at: 3, ..State::default() }));
    let mut display = WindowDisplay::open(Fake(state.clone()), true).unwrap();
    let mut phases = Vec::new();

    let error = session::run_controlled_with_display(
        root.path(), &[], &mut control, &mut |phase| phases.push(phase), &mut display,
    ).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert!(phases.contains(&"gui_diagnostics"));
    assert!(!phases.contains(&"gui_error"));
    assert!(!state.borrow().open);
}

#[test]
fn normal_cli_selects_pc_for_desktop_but_headless_and_help_do_not_open_a_window() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let profile = root.path().join("profile.json");
    std::fs::write(&profile, br#"{"version":1}"#).unwrap();
    for configured in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_iman"));
        command.arg("--root").arg(root.path()).env("DISPLAY", "");
        if configured {
            command.arg("--config").arg(&profile);
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Failed to create window"));
    }
    for args in [vec!["--", "--headless"], vec!["--help"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_iman"))
            .arg("--root").arg(root.path()).args(&args).env("DISPLAY", "").output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("Failed to create window"));
        assert_eq!(output.status.success(), args == ["--help"]);
        if args != ["--help"] {
            assert!(stderr.contains("GUI failed before display reclaim"));
        }
    }
}

#[test]
fn repeated_diagnostics_skip_present_and_partial_state_is_reset_after_other_screens() {
    let state = Rc::new(RefCell::new(State::default()));
    let mut display = WindowDisplay::open(Fake(state.clone()), true).unwrap();
    display.departure("Complete").unwrap();
    display.diagnostic_status("Diagnostics\nRAM: 100", 0).unwrap();
    let before = state.borrow().events.len();

    display.diagnostic_status("Diagnostics\nRAM: 100", 4).unwrap();
    assert_eq!(state.borrow().events.len(), before);
    display.diagnostic_status("Diagnostics\nRAM: 2", 5).unwrap();
    let mut expected = imanlib::surface::Surface::new(splash::WIDTH, splash::HEIGHT);
    splash::draw_diagnostics(&mut expected, "Diagnostics\nRAM: 2", 5).unwrap();
    assert_eq!(state.borrow().pixels, expected.pixels());
    #[cfg(feature = "diag-display")]
    {
        let (_, sample) = display.take_diagnostics().unwrap();
        assert_eq!(sample.render.unwrap().partial_cost.count, 1);
    }
    display.error_tick(10).unwrap();
    display.diagnostic_status("Diagnostics\nRAM: 2", 5).unwrap();
    assert_eq!(state.borrow().pixels, expected.pixels());
}

#[cfg(feature = "diag-input")]
#[test]
fn input_preparation_countdown_and_controller_prompts_use_partial_presentations() {
    let state = Rc::new(RefCell::new(State::default()));
    let mut display = WindowDisplay::open(Fake(state.clone()), true).unwrap();
    let mut view = iman::input_test::InputTest::default().view(0);
    view.stage = iman::input_test::Stage::Prepare { number: 1 };
    view.count = 2;
    view.opened = 2;
    view.seconds = 4;
    display.input_test_frame(&view).unwrap();
    display.take_diagnostics();

    for seconds in [3, 2, 1] {
        view.seconds = seconds;
        display.input_test_frame(&view).unwrap();
        let presented = state.borrow().events.len();
        display.input_test_frame(&view).unwrap();
        assert_eq!(state.borrow().events.len(), presented);
    }

    #[cfg(feature = "diag-display")]
    {
        let (_, sample) = display.take_diagnostics().unwrap();
        let render = sample.render.unwrap();
        assert_eq!(render.full, 0);
        assert_eq!(render.partial_cost.count, 3);
        assert_eq!(sample.present.unwrap().count, 3);
    }
    let mut expected = imanlib::surface::Surface::new(1280, 720);
    iman::input_test::render::draw(&mut expected, &view).unwrap();
    assert!(state.borrow().pixels == expected.pixels());
    view.stage = iman::input_test::Stage::Controller { number: 1, step: 0, release: false };
    view.seconds = 5;
    display.input_test_frame(&view).unwrap();
    display.take_diagnostics();

    for (step, release) in [(0, true), (1, false)] {
        view.stage = iman::input_test::Stage::Controller { number: 1, step, release };
        display.input_test_frame(&view).unwrap();
        iman::input_test::render::draw(&mut expected, &view).unwrap();
        assert!(state.borrow().pixels == expected.pixels());
    }

    #[cfg(feature = "diag-display")]
    {
        let (_, sample) = display.take_diagnostics().unwrap();
        let render = sample.render.unwrap();
        assert_eq!(render.full, 0);
        assert_eq!(render.partial_cost.count, 2);
        assert_eq!(sample.present.unwrap().count, 2);
    }
}

#[cfg(feature = "diag-input")]
#[test]
fn input_test_updates_are_partial_skip_identical_frames_and_reset_after_other_screens() {
    let state = Rc::new(RefCell::new(State::default()));
    let mut display = WindowDisplay::open(Fake(state.clone()), true).unwrap();
    display.take_diagnostics();
    let mut view = iman::input_test::InputTest::default().view(0);
    display.input_test_frame(&view).unwrap();
    view.seconds = 4;
    display.input_test_frame(&view).unwrap();
    let presented = state.borrow().events.len();
    display.input_test_frame(&view).unwrap();
    assert_eq!(state.borrow().events.len(), presented);
    #[cfg(feature = "diag-display")]
    {
        let (_, sample) = display.take_diagnostics().unwrap();
        let render = sample.render.unwrap();
        assert_eq!(render.full, 1);
        assert_eq!(render.partial_cost.count, 1);
        assert_eq!(sample.present.unwrap().count, 2);
    }
    let mut expected = imanlib::surface::Surface::new(1280, 720);
    iman::input_test::render::draw(&mut expected, &view).unwrap();
    assert!(state.borrow().pixels == expected.pixels());
    for other in ["departure", "diagnostics", "receipt", "lend"] {
        match other {
            "departure" => { display.departure("Complete").unwrap(); }
            "diagnostics" => { display.diagnostic_status("Diagnostics", 0).unwrap(); }
            "receipt" => { display.report_saved(Path::new("/synthetic/report.json")).unwrap(); }
            "lend" => { display.lend().unwrap(); display.reclaim().unwrap(); }
            _ => unreachable!(),
        }
        display.input_test_frame(&view).unwrap();
        assert!(state.borrow().pixels == expected.pixels(), "{other}");
    }
    state.borrow_mut().fail_present = true;
    view.seconds = 3;
    assert!(display.input_test_frame(&view).is_err());
    state.borrow_mut().fail_present = false;
    display.input_test_frame(&view).unwrap();
    iman::input_test::render::draw(&mut expected, &view).unwrap();
    assert!(state.borrow().pixels == expected.pixels());
}
