// SPDX-License-Identifier: BSD-3-Clause
#![cfg(feature = "drm")]
use {
    imanlib::kms::Backend,
    imanlib::surface::Surface,
};
use iman::display::owner::Owner as Display;
use std::{cell::RefCell, io, rc::Rc};

#[derive(Default)]
struct State {
    calls: Vec<&'static str>,
    failures: Vec<&'static str>,
    master_errno: Option<i32>,
}
struct Fake(Rc<RefCell<State>>);
impl Fake {
    fn call(&self, name: &'static str) -> io::Result<()> {
        let mut state = self.0.borrow_mut();
        state.calls.push(name);
        if state.failures.contains(&name) {
            Err(io::Error::other(name))
        } else {
            Ok(())
        }
    }
}
impl Backend for Fake {
    fn acquire_master(&mut self) -> io::Result<()> {
        self.call("acquire")?;
        self.0
            .borrow()
            .master_errno
            .map_or(Ok(()), |e| Err(io::Error::from_raw_os_error(e)))
    }
    fn release_master(&mut self) -> io::Result<()> {
        self.call("release")
    }
    fn select_output(&mut self) -> io::Result<()> {
        self.call("select")
    }
    fn create_buffer(&mut self) -> io::Result<()> {
        self.call("create")
    }
    fn add_framebuffer(&mut self) -> io::Result<()> {
        self.call("add")
    }
    fn write_frame(&mut self, _: &Surface) -> io::Result<()> {
        self.call("write")
    }
    fn enable(&mut self) -> io::Result<()> {
        self.call("enable")
    }
    fn verify(&mut self) -> io::Result<()> {
        self.call("verify")
    }
    fn restore(&mut self) -> io::Result<()> {
        self.call("restore")
    }
    fn remove_framebuffer(&mut self) -> io::Result<()> {
        self.call("remove")
    }
    fn destroy_buffer(&mut self) -> io::Result<()> {
        self.call("destroy")
    }
}

#[test]
fn yield_keeps_resources_and_reclaim_verifies_before_drawing() {
    let state = Rc::new(RefCell::new(State::default()));
    let frame = Surface::new(2, 2);
    let mut display = Display::start(Fake(state.clone()), &frame, false).unwrap();
    state.borrow_mut().calls.clear();

    display.yield_display().unwrap();
    assert!(!display.master_acquired());
    assert!(display.present(&frame).is_err());
    assert!(display.yield_display().is_err());
    assert_eq!(state.borrow().calls, ["verify", "release", "verify"]);
    display.reclaim().unwrap();
    assert!(display.master_acquired());
    display.present(&frame).unwrap();
    display.finish().unwrap();
    display.finish().unwrap();
    assert!(display.present(&frame).is_err());
    assert!(display.reclaim().is_err());
    drop(display);

    assert_eq!(
        state.borrow().calls,
        [
            "verify", "release", "verify", "acquire", "verify", "verify", "write", "restore",
            "remove", "destroy", "release"
        ]
    );
}

#[test]
fn vendor_einval_never_claims_or_drops_master() {
    let state = Rc::new(RefCell::new(State {
        master_errno: Some(libc::EINVAL),
        ..State::default()
    }));
    let mut display = Display::start(Fake(state.clone()), &Surface::new(1, 1), true).unwrap();

    assert!(!display.master_acquired());
    display.yield_display().unwrap();
    display.reclaim().unwrap();
    display.finish().unwrap();

    assert!(!display.master_acquired());
    assert!(!state.borrow().calls.contains(&"release"));
}

#[test]
fn strict_and_permission_errors_never_start_a_modeset() {
    for (errno, vendor) in [
        (libc::EINVAL, false),
        (libc::EPERM, true),
        (libc::EACCES, true),
        (libc::EBUSY, true),
    ] {
        let state = Rc::new(RefCell::new(State {
            master_errno: Some(errno),
            ..State::default()
        }));

        assert!(Display::start(Fake(state.clone()), &Surface::new(1, 1), vendor).is_err());

        assert_eq!(state.borrow().calls, ["acquire"]);
    }
}

#[test]
fn startup_failures_release_only_possibly_created_resources() {
    for operation in ["select", "create", "add", "write", "enable", "verify"] {
        let state = Rc::new(RefCell::new(State {
            failures: vec![operation],
            ..State::default()
        }));

        assert!(Display::start(Fake(state.clone()), &Surface::new(1, 1), false).is_err());

        let calls = &state.borrow().calls;
        assert_eq!(calls.last(), Some(&"release"));
        assert_eq!(calls.contains(&"destroy"), operation != "select");
        assert_eq!(
            calls.contains(&"remove"),
            !["select", "create"].contains(&operation)
        );
        assert_eq!(
            calls.contains(&"restore"),
            ["enable", "verify"].contains(&operation)
        );
    }
}

#[test]
fn failed_yield_does_not_claim_that_ownership_was_released() {
    let state = Rc::new(RefCell::new(State::default()));
    let mut display = Display::start(Fake(state.clone()), &Surface::new(1, 1), false).unwrap();
    state.borrow_mut().failures.push("release");

    assert!(display.yield_display().is_err());
    assert!(display.master_acquired());
    assert!(display.reclaim().is_err());
    state.borrow_mut().failures.clear();
    display.finish().unwrap();
}

#[test]
fn cleanup_collects_failures_and_never_retries_on_drop() {
    let state = Rc::new(RefCell::new(State::default()));
    let mut display = Display::start(Fake(state.clone()), &Surface::new(1, 1), false).unwrap();
    state.borrow_mut().calls.clear();
    state.borrow_mut().failures = vec!["restore", "remove", "destroy", "release"];

    let error = display.finish().unwrap_err().to_string();
    drop(display);

    for name in ["restore", "remove-fb", "destroy-buffer", "release-master"] {
        assert!(error.contains(name));
    }
    assert_eq!(
        state.borrow().calls,
        ["restore", "remove", "destroy", "release"]
    );
}

#[test]
fn failed_reclaim_does_not_retry_acquisition_during_cleanup() {
    let state = Rc::new(RefCell::new(State::default()));
    let mut display = Display::start(Fake(state.clone()), &Surface::new(1, 1), false).unwrap();
    display.yield_display().unwrap();
    state.borrow_mut().calls.clear();
    state.borrow_mut().master_errno = Some(libc::EBUSY);

    assert!(display.reclaim().is_err());
    display.finish().unwrap();

    assert_eq!(
        state.borrow().calls,
        ["acquire", "restore", "remove", "destroy"]
    );
}

#[test]
fn missing_parent_scanout_fails_reclaim_without_modesetting_over_it() {
    let state = Rc::new(RefCell::new(State::default()));
    let mut display = Display::start(Fake(state.clone()), &Surface::new(1, 1), false).unwrap();
    display.yield_display().unwrap();
    state.borrow_mut().calls.clear();
    state.borrow_mut().failures.push("verify");

    assert!(display.reclaim().is_err());

    assert_eq!(state.borrow().calls, ["acquire", "verify"]);
}

#[test]
fn dropping_a_yielded_display_reclaims_before_cleanup() {
    let state = Rc::new(RefCell::new(State::default()));
    let mut display = Display::start(Fake(state.clone()), &Surface::new(1, 1), false).unwrap();
    display.yield_display().unwrap();
    state.borrow_mut().calls.clear();

    drop(display);

    assert_eq!(
        state.borrow().calls,
        ["acquire", "verify", "restore", "remove", "destroy", "release"]
    );
}

#[test]
fn gui_game_gui_share_one_parent_buffer_until_final_restore() {
    let state = Rc::new(RefCell::new(State::default()));
    let mut display = Display::start(Fake(state.clone()), &Surface::new(1, 1), false).unwrap();
    for _ in 0..3 {
        display.yield_display().unwrap();
        display.reclaim().unwrap();
        assert!(!state.borrow().calls.contains(&"restore"));
        assert!(!state.borrow().calls.contains(&"remove"));
    }
    display.finish().unwrap();
    drop(display);

    for op in ["create", "add", "enable", "restore", "remove", "destroy"] {
        assert_eq!(
            state
                .borrow()
                .calls
                .iter()
                .filter(|name| **name == op)
                .count(),
            1
        );
    }
}

#[test]
fn backend_operations_are_bracketed_by_ownership_checks_and_blocked_after_yield_or_close() {
    let state = Rc::new(RefCell::new(State::default()));
    let mut display = Display::start(Fake(state.clone()), &Surface::new(1, 1), false).unwrap();
    state.borrow_mut().calls.clear();

    display
        .with_backend(|backend| backend.call("diagnostic"))
        .unwrap();

    assert_eq!(state.borrow().calls, ["verify", "diagnostic", "verify"]);
    display.yield_display().unwrap();
    assert!(display
        .with_backend(|_| -> io::Result<()> { panic!("yielded") })
        .is_err());
    display.reclaim().unwrap();
    state.borrow_mut().failures.push("verify");
    assert!(display
        .with_backend(|_| -> io::Result<()> { panic!("foreign scanout") })
        .is_err());
    state.borrow_mut().failures.clear();
    display.finish().unwrap();
    assert!(display
        .with_backend(|_| -> io::Result<()> { panic!("closed") })
        .is_err());
}

#[test]
fn page_flip_route_update_accepts_only_our_framebuffer_change() {
    use {
        imanlib::kms::output::Connector,
        imanlib::kms::output::Crtc,
        imanlib::kms::output::DisplayMode,
        imanlib::kms::output::Encoder,
        imanlib::kms::output::Timing,
        imanlib::kms::output::Topology,
    };
    use iman::display::owner::verify_presented_route;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Mode;
    impl DisplayMode for Mode {
        fn preferred(self) -> bool { true }
        fn timing(self) -> Timing {
            Timing {
                size: (1280, 720), clock: 74250, horizontal: (1390, 1430, 1650),
                vertical: (725, 730, 750), hskew: 0, vscan: 0, flags: 0,
            }
        }
    }
    let original = Topology {
        connectors: vec![Connector {
            id: 1, connected: true, hdmi: true, current_encoder: Some(2),
            encoders: vec![2], modes: vec![Mode],
        }],
        encoders: vec![Encoder { id: 2, current_crtc: Some(3), possible_crtcs: vec![3] }],
        crtcs: vec![
            Crtc { id: 3, framebuffer: Some(4), position: (0, 0), mode: Some(Mode) },
            Crtc { id: 5, framebuffer: None, position: (0, 0), mode: None },
        ],
    };
    let mut flipped = original.clone();
    flipped.crtcs[0].framebuffer = Some(6);

    verify_presented_route(&original, &flipped, 3).unwrap();
    assert!(verify_presented_route(&original, &flipped, 99).is_err());
    for change in 0..8 {
        let mut foreign = flipped.clone();
        match change {
            0 => foreign.connectors[0].connected = false,
            1 => foreign.connectors[0].current_encoder = None,
            2 => foreign.encoders[0].current_crtc = None,
            3 => foreign.crtcs[0].position = (1, 0),
            4 => foreign.crtcs[0].mode = None,
            5 => foreign.crtcs[1].framebuffer = Some(7),
            6 => foreign.crtcs[0].framebuffer = None,
            7 => foreign.crtcs[0].framebuffer = Some(0),
            _ => unreachable!(),
        }
        assert!(verify_presented_route(&original, &foreign, 3).is_err(), "change {change}");
    }
}
