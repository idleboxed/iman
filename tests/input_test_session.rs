// SPDX-License-Identifier: BSD-3-Clause
//! Synthetic GUI/display/exporter; never opens actual input or writes to SD.
#![cfg(feature = "diag-input")]
use iman::{session::control::Control, display::Lease, session::{self, GameEnvironment}};
use iman::session::timing::Timing;
use imanlib::{client::Client, protocol::Outcome};
use serde_json::json;
use std::{fs, io, os::unix::process::ExitStatusExt, path::Path,
    sync::{Arc, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};

#[path = "common/timing.rs"]
mod test_timing;
use test_timing::ManualTiming;

struct Screen {
    events: Vec<&'static str>,
    saved: Arc<AtomicBool>,
    receipt: Option<Duration>,
    timing: ManualTiming,
}
impl Lease for Screen {
    fn lend(&mut self) -> io::Result<()> { self.events.push("lend"); Ok(()) }
    fn reclaim(&mut self) -> io::Result<()> { self.events.push("reclaim"); Ok(()) }
    fn finish(&mut self) -> io::Result<()> {
        assert_eq!(self.timing.now() - self.receipt.unwrap(), Duration::from_secs(5));
        self.events.push("finish"); Ok(())
    }
    fn input_test_frame(&mut self, _: &iman::input_test::View) -> io::Result<bool> {
        assert_eq!(self.events.last(), Some(&"reclaim"));
        self.events.push("test");
        Ok(false) // Refuse acquisition: this fixture must never discover real input devices.
    }
    fn departure(&mut self, _: &str) -> io::Result<bool> { self.events.push("departure"); Ok(true) }
    fn diagnostic_status(&mut self, _: &str, _: u64) -> io::Result<()> {
        self.events.push("diagnostics"); Ok(())
    }
    fn error_tick(&mut self, _: u8) -> io::Result<()> { self.events.push("countdown"); Ok(()) }
    fn report_saved(&mut self, path: &Path) -> io::Result<bool> {
        assert!(self.saved.load(Ordering::Acquire));
        assert_eq!(path, Path::new("/synthetic/synchronized/report.json"));
        self.receipt = Some(self.timing.now());
        self.events.push("receipt");
        Ok(true)
    }
}

#[test]
fn reaped_gui_never_restarts_and_receipt_follows_diagnostics_farewell_and_sync_for_five_seconds() {
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    let directory = root.path().join("system/igui/controls");
    fs::create_dir_all(&directory).unwrap();
    // Valid navigation profile only; this fixture never discovers real devices.
    let profile = json!({
        "name": "Synthetic session controller",
        "backend": "evdev",
        "access": "read-only",
        "match": {"bus": "usb", "vendor": "0001", "product": "0002"},
        "buttons": {"west": 1, "east": 2, "north": 3, "south": 4, "up": 5, "down": 6, "left": 7, "right": 8},
        "axes": {},
    });
    fs::write(directory.join("profile.json"), serde_json::to_vec(&profile).unwrap()).unwrap();
    let mut control = Control::bind(root.path(), root.path()).unwrap();
    control.enable_diagnostics().unwrap();
    let saved = Arc::new(AtomicBool::new(false));
    let completed = saved.clone();
    control.enable_cycle_exports(Arc::new(move |_, bundle| {
        bundle.manifest()?;
        let report: serde_json::Value = serde_json::from_slice(&bundle.files["diag_input_test.json"])?;
        assert_eq!(report["owner"], "iman");
        assert!(report["error"].as_str().unwrap().contains("No managed input test display"), "{report}");
        completed.store(true, Ordering::Release);
        Ok("/synthetic/synchronized/report.json".into())
    }), Duration::ZERO).unwrap();
    let mut timing = ManualTiming::default();
    let mut screen = Screen { events: Vec::new(), saved, receipt: None, timing: timing.clone() };
    let mut launches = 0;

    session::run_session_with_timing(root.path(), GameEnvironment::Inherited, &mut control,
        &mut |_| {}, &mut screen, &mut timing,
        |_, _, control, _| {
            launches += 1;
            control.begin_gui(std::process::id(), Default::default(), None);
            let mut client = Client::connect(control.path())?;
            let began = Instant::now();
            for command in ["igui.bootstrap", "igui.input_test"] {
                client.send(command, json!({}))?;
                loop {
                    control.poll("gui");
                    if let Some(response) = client.poll()? {
                        assert!(matches!(response.outcome, Outcome::Done { .. })); break;
                    }
                    assert!(began.elapsed() < Duration::from_secs(2));
                    std::thread::yield_now();
                }
            }
            control.end_gui(std::process::ExitStatus::from_raw(0))
        }).unwrap();

    assert_eq!(launches, 1);
    assert!(timing.now() >= Duration::from_secs(26));
    assert_eq!(&screen.events[..4], &["lend", "reclaim", "test", "departure"]);
    assert_eq!(screen.events.iter().filter(|&&event| event == "diagnostics").count(), 10);
    assert_eq!(screen.events.iter().filter(|&&event| event == "countdown").count(), 11);
    assert_eq!(&screen.events[screen.events.len() - 2..], &["receipt", "finish"]);
    assert!(!root.path().join("iman-reports").exists());
}
