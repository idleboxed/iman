// SPDX-License-Identifier: BSD-3-Clause
//! Synthetic evdev sources only; no hardware or persistent mappings.
#![cfg(feature = "diag-input")]
use {
    imanlib::input_probe::linux::Pad,
    imanlib::input_probe::linux::Source,
};
use imanlib::evdev::{DeviceInfo, Event};
use iman::{display::Lease, input_test::{InputTest, Stage, View, run_with_source, DISCOVERY_MS, PREPARE_MS, guide::STEP_MS}};
use std::{collections::BTreeMap, io, path::{Path, PathBuf}};

fn connected(count: usize) -> InputTest {
    let mut test = InputTest::default();
    for index in 0..count {
        let slot = test.probe.connect(DeviceInfo {
            path: format!("/synthetic/event{index}"), name: "Synthetic pad".into(),
            key_codes: vec![28, 103], axis_codes: vec![0],
            abs_info: BTreeMap::from([(0, [128, 0, 255, 0, 15, 0])]), identity: BTreeMap::new(),
        }, 0).unwrap();
        test.probe.baseline(slot, &[], 0).unwrap();
    }
    test
}

fn sample(test: &mut InputTest, ms: u64) {
    test.probe.sampled(ms);
    test.refresh(ms);
}

fn tap(test: &mut InputTest, slot: usize, ms: u64) {
    test.probe.event(slot, Event { kind: 1, code: 28, value: 1 }, ms);
    test.probe.event(slot, Event { kind: 1, code: 28, value: 0 }, ms);
    sample(test, ms);
}

#[test]
fn only_responding_controllers_get_twenty_five_steps_not_four_fixed_passes() {
    for count in [0, 1, 2, 4] {
        let mut test = connected(4);
        for slot in 0..count { tap(&mut test, slot, 100 + slot as u64); }
        sample(&mut test, DISCOVERY_MS);
        assert_eq!(test.view(DISCOVERY_MS).count, count);
        assert_eq!(test.done(), count == 0);
        for index in 0..count {
            let ms = DISCOVERY_MS + index as u64 * (PREPARE_MS + 25 * STEP_MS);
            assert_eq!(test.view(ms).stage, Stage::Prepare { number: index + 1 });
            assert_eq!(test.view(ms).seconds, 4);
            sample(&mut test, ms + PREPARE_MS);
            assert_eq!(test.view(ms + PREPARE_MS).stage, Stage::Controller { number: index + 1, step: 0, release: false });
            assert_eq!(test.view(ms + PREPARE_MS).seconds, 5);
            sample(&mut test, ms + PREPARE_MS + 25 * STEP_MS);
        }
        assert!(test.done());
        let report = test.finish(DISCOVERY_MS + count as u64 * (PREPARE_MS + 25 * STEP_MS), "completed");
        assert_eq!(report["controllers"].as_array().unwrap().len(), count);
        assert_eq!(report["unconfirmed"].as_array().unwrap().len(), 4 - count);
        for controller in report["controllers"].as_array().unwrap() {
            assert_eq!(controller["steps"].as_array().unwrap().len(), 25);
            assert!(controller["steps"].as_array().unwrap().iter().all(|s| s["outcome"] == "no_response"));
        }
        assert_eq!(report["mapping_written"], false);
        assert_eq!(report["physical_players_verified"], false);
        assert_eq!(report["prepare_ms"], PREPARE_MS);
    }
}

#[test]
fn axis_noise_repeat_initial_hold_and_incomplete_packets_do_not_select_a_pad() {
    let mut test = connected(4);
    test.probe.baseline(0, &[28], 0).unwrap();
    for value in [1, 2, 0] {
        test.probe.event(0, Event { kind: 1, code: 28, value }, 100);
    }
    test.probe.event(1, Event { kind: 3, code: 0, value: 255 }, 100);
    sample(&mut test, 100);
    test.probe.event(2, Event { kind: 1, code: 28, value: 1 }, 200);
    test.probe.cancel_sample();
    test.refresh(200);
    sample(&mut test, 300);
    assert_eq!(test.view(300).count, 0);
    tap(&mut test, 0, 400);
    tap(&mut test, 0, 500);
    assert_eq!(test.view(500).count, 1);
    sample(&mut test, DISCOVERY_MS);
    tap(&mut test, 3, DISCOVERY_MS + 100);
    assert_eq!(test.view(DISCOVERY_MS + 100).count, 1);
}

#[test]
fn other_sources_cannot_complete_a_step_and_reconnect_cannot_replace_a_selected_generation() {
    let mut test = connected(2);
    tap(&mut test, 0, 100);
    tap(&mut test, 1, 200);
    sample(&mut test, DISCOVERY_MS);
    let started = DISCOVERY_MS + PREPARE_MS;
    sample(&mut test, started);
    sample(&mut test, started + 100);
    tap(&mut test, 1, started + 200);
    assert_eq!(test.guide.index, 0);
    tap(&mut test, 0, started + 300);
    assert_eq!(test.guide.index, 1);
    let info = test.probe.streams[0].as_ref().unwrap().info.clone();
    test.probe.disconnect(0, started + 400, "synthetic disconnect");
    let slot = test.probe.connect(info, started + 400).unwrap();
    test.probe.baseline(slot, &[], started + 400).unwrap();
    sample(&mut test, started + 400);
    assert_eq!(test.view(started + 400).stage, Stage::Prepare { number: 2 });
    assert_eq!(test.view(started + 400).seconds, 4);
    let report = test.finish(started + 500, "cancelled");
    assert_eq!(report["controllers"][0]["status"], "disconnected");
    assert_eq!(report["controllers"][0]["steps"][0]["outcome"], "completed");
    assert_eq!(report["controllers"][0]["steps"][1]["outcome"], "disconnected");
    assert_eq!(report["controllers"][1]["status"], "cancelled");
    assert_eq!(report["unconfirmed"][0]["generation"], 3);
}

#[test]
fn four_pad_outcomes_survive_a_noisy_raw_journal() {
    let mut test = connected(5);
    for slot in 0..5 { tap(&mut test, slot, 100); }
    assert_eq!(test.view(100).count, 4);
    for ms in 200..1_200 { tap(&mut test, 4, ms); }
    let mut ticks = std::iter::once(DISCOVERY_MS).chain((0..4).flat_map(|index| {
        let ms = DISCOVERY_MS + index * (PREPARE_MS + 25 * STEP_MS);
        [ms + PREPARE_MS, ms + PREPARE_MS + 25 * STEP_MS]
    }));
    let elapsed = iman::input_test::run_with_inventory(&mut test, EmptySource,
        &mut Screen { visible: true, ..Screen::default() },
        &mut || Ok(ticks.next().expect("test must finish within the synthetic schedule")),
        &mut || serde_json::json!({"synthetic": "x".repeat(3500)})).unwrap();
    let report = test.finish(elapsed, "completed");
    assert!(report["records_omitted"].as_u64().unwrap() > 0);
    assert_eq!(report["inventory"].as_array().unwrap().len(), 2);
    assert_eq!(report["controllers"].as_array().unwrap().iter()
        .map(|c| c["steps"].as_array().unwrap().len()).sum::<usize>(), 100);
    assert!(serde_json::to_vec(&report).unwrap().len() < 64 * 1024);
    assert!(report["input_stats"]["evdev"]["events"].as_u64().unwrap() > 2_000);
}

#[test]
fn resync_does_not_confirm_a_candidate_from_unknown_axis_state() {
    let mut test = connected(1);
    tap(&mut test, 0, 100);
    sample(&mut test, DISCOVERY_MS);
    let started = DISCOVERY_MS + PREPARE_MS;
    sample(&mut test, started);
    sample(&mut test, started + 100);
    test.probe.event(0, Event { kind: 1, code: 28, value: 1 }, started + 200);
    sample(&mut test, started + 200);
    test.probe.resync(0, started + 300, "SYN_DROPPED");
    test.probe.baseline(0, &[], started + 300).unwrap();
    sample(&mut test, started + 300);
    tap(&mut test, 0, started + 400);
    assert_eq!(test.guide.index, 0);
    sample(&mut test, started + STEP_MS);
    assert_eq!(test.guide.results[0]["outcome"], "ambiguous");
}

struct EmptySource;
impl Source for EmptySource {
    fn discover(&mut self) -> io::Result<Vec<PathBuf>> { Ok(Vec::new()) }
    fn open(&mut self, _: &Path) -> io::Result<Box<dyn Pad>> { panic!("no devices") }
}
#[derive(Default)]
struct Screen { frames: Vec<View>, visible: bool }
impl Lease for Screen {
    fn lend(&mut self) -> io::Result<()> { Ok(()) }
    fn reclaim(&mut self) -> io::Result<()> { Ok(()) }
    fn finish(&mut self) -> io::Result<()> { Ok(()) }
    fn input_test_frame(&mut self, view: &View) -> io::Result<bool> {
        self.frames.push(view.clone()); Ok(self.visible)
    }
}

#[test]
fn idle_polling_does_not_present_identical_frames() {
    let mut test = InputTest::default();
    let mut screen = Screen { visible: true, ..Screen::default() };
    let mut ms = 0;
    let elapsed = run_with_source(&mut test, EmptySource, &mut screen, &mut || { ms += 20; Ok(ms) }).unwrap();
    assert_eq!(elapsed, DISCOVERY_MS);
    assert_eq!(screen.frames.len(), (DISCOVERY_MS / 1000 + 1) as usize);
    assert_eq!(screen.frames.last().unwrap().stage, Stage::Finished);
}

#[test]
fn preparation_is_shown_once_per_second_before_the_first_controller() {
    let mut test = connected(1);
    tap(&mut test, 0, 100);
    let mut screen = Screen { visible: true, ..Screen::default() };
    let mut ms = 100;

    let elapsed = run_with_source(&mut test, EmptySource, &mut screen, &mut || { ms += 20; Ok(ms) }).unwrap();

    assert_eq!(elapsed, DISCOVERY_MS + PREPARE_MS + 25 * STEP_MS);
    assert_eq!(screen.frames.iter().filter(|view| view.stage == Stage::Prepare { number: 1 })
        .map(|view| view.seconds).collect::<Vec<_>>(), vec![4, 3, 2, 1]);
}

#[test]
fn preparation_and_boundary_taps_do_not_answer_the_first_prompt_or_spend_its_time() {
    let mut test = connected(1);
    tap(&mut test, 0, 100);
    sample(&mut test, DISCOVERY_MS);
    assert_eq!(PREPARE_MS, 4_000);
    for (offset, seconds) in [(0, 4), (999, 4), (1000, 3), (2000, 2), (3000, 1), (3999, 1)] {
        tap(&mut test, 0, DISCOVERY_MS + offset);
        assert_eq!(test.view(DISCOVERY_MS + offset).stage, Stage::Prepare { number: 1 });
        assert_eq!(test.view(DISCOVERY_MS + offset).seconds, seconds);
        assert!(test.guide.results.is_empty());
        assert_eq!(test.view(DISCOVERY_MS + offset).input, "Input: -");
    }
    let started = DISCOVERY_MS + PREPARE_MS;

    tap(&mut test, 0, started);

    assert_eq!(test.view(started).stage, Stage::Controller { number: 1, step: 0, release: false });
    assert_eq!(test.view(started).seconds, 5);
    sample(&mut test, started + STEP_MS - 1);
    assert_eq!(test.guide.index, 0);
    sample(&mut test, started + STEP_MS);
    assert_eq!(test.guide.results[0]["outcome"], "no_response");
    assert_eq!(test.guide.results[0]["started_ms"], started);
    let report = test.finish(started + STEP_MS, "cancelled");
    for record in report["records"].as_array().unwrap().iter().filter(|r| r["phase"] == "prepare") {
        assert_eq!(record["controller"], 1);
        assert!(record.get("step").is_none());
    }
    assert!(report["records"].as_array().unwrap().iter().any(|r| r["phase"] == "prepare" && r["ms"] == started));
}

#[test]
fn controls_held_during_preparation_must_be_released_before_a_fresh_answer() {
    let mut test = connected(1);
    tap(&mut test, 0, 100);
    sample(&mut test, DISCOVERY_MS);
    let started = DISCOVERY_MS + PREPARE_MS;
    test.probe.event(0, Event { kind: 1, code: 28, value: 1 }, started - 500);
    sample(&mut test, started - 500);
    sample(&mut test, started);
    sample(&mut test, started + 100);
    assert!(!test.guide.armed);

    test.probe.event(0, Event { kind: 1, code: 28, value: 0 }, started + 200);
    sample(&mut test, started + 200);

    assert_eq!(test.guide.index, 0);
    tap(&mut test, 0, started + 300);
    assert_eq!(test.guide.results[0]["outcome"], "completed");
}

#[test]
fn cancellation_and_disconnect_during_preparation_never_confirm_a_prompt() {
    for disconnect in [false, true] {
        let mut test = connected(2);
        tap(&mut test, 0, 100);
        tap(&mut test, 1, 200);
        sample(&mut test, DISCOVERY_MS);
        let ms = DISCOVERY_MS + 2000;
        if disconnect {
            test.probe.disconnect(0, ms, "synthetic disconnect");
            sample(&mut test, ms);
            assert_eq!(test.view(ms).stage, Stage::Prepare { number: 2 });
            assert_eq!(test.view(ms).seconds, 4);
        }

        let report = test.finish(ms, "cancelled");

        let outcome = if disconnect { "disconnected" } else { "cancelled" };
        assert_eq!(report["controllers"][0]["status"], outcome);
        let steps = report["controllers"][0]["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 25);
        assert!(steps.iter().all(|step| step["outcome"] == outcome));
    }
}

#[test]
fn unavailable_display_never_starts_input_polling() {
    let error = run_with_source(&mut InputTest::default(), EmptySource, &mut Screen::default(),
        &mut || panic!("must not poll without a visible screen")).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
}

#[test]
fn all_prompts_render_with_a_small_step_label_and_separate_home_start_buttons() {
    use imanlib::surface::Surface;
    use iman::input_test::render;
    for step in 0..25 {
        for release in [false, true] {
            let mut frame = Surface::new(1280, 720);
            render::draw(&mut frame, &View {
                stage: Stage::Controller { number: 4, step, release }, count: 4, opened: 4,
                seconds: 5, input: "Input: KEY 28 = 0".into(), notice: String::new(),
            }).unwrap();
            let pixels = frame.pixels();
            let background = 0x0f151d;
            assert!(pixels[160 * 1280..210 * 1280].iter().all(|&p| p == background));
            assert!(pixels[137 * 1280..157 * 1280].iter().any(|&p| p != background));
            // HOME is above the central row; START ends before the X face button.
            assert_eq!(pixels[397 * 1280 + 639], 0x26333c);
            assert_eq!(pixels[421 * 1280 + 722], 0x26333c);
        }
    }
}

#[test]
fn empty_discovery_is_distinct_from_open_devices_without_fresh_presses() {
    let empty = InputTest::default();
    let held = connected(2);

    assert_eq!(empty.result_message(), "No matching input devices opened");
    assert_eq!(held.result_message(), "Input devices opened, but no fresh button presses");
    assert_eq!(held.view(0).opened, 2);
    assert_eq!(held.view(0).count, 0);
}

#[test]
fn inventory_is_not_read_without_visible_screen_and_is_bounded_on_cancellation() {
    use iman::input_test::run_with_inventory;
    let mut test = InputTest::default();
    let error = run_with_inventory(&mut test, EmptySource, &mut Screen::default(),
        &mut || panic!("no polling"), &mut || panic!("no inventory")).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert_eq!(test.finish(0, "unavailable")["inventory"], serde_json::json!([]));

    let mut test = InputTest::default();
    let mut captures = 0;
    let error = run_with_inventory(&mut test, EmptySource,
        &mut Screen { visible: true, ..Screen::default() },
        &mut || Err(io::Error::new(io::ErrorKind::Interrupted, "synthetic cancellation")),
        &mut || { captures += 1; serde_json::json!({"oversized":"x".repeat(5000)}) }).unwrap_err();
    let report = test.finish(0, "cancelled");

    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(captures, 2);
    assert_eq!(report["inventory"][0]["data"]["omitted"], true);
    assert_eq!(report["inventory"][1]["data"]["omitted"], true);
}

#[cfg(feature = "io-monitor")]
#[test]
fn input_loop_passes_real_io_and_segment_phase_validators_through_completion() {
    use iman::{diagnostics::cycle::Session, diagnostics::io::Recorder};
    use std::sync::Arc;
    let mut recorder = Recorder::new(vec!["mmcblk0".into()]).unwrap();
    let mut segment = Session::new("synthetic".into(), Arc::new(|_, _| panic!("no export")), 0);
    let counters = "179 0 mmcblk0 0 0 0 0 0 0 0 0 0 0 0\n";
    for (ms, phase) in [(0, "gui"), (10, "gui_exit"), (20, "display_reclaim")] {
        recorder.sample(ms, phase, counters).unwrap();
        segment.phase(ms, phase);
    }
    let mut test = InputTest::default();
    let mut ms = 0;

    run_with_source(&mut test, EmptySource, &mut Screen { visible: true, ..Screen::default() }, &mut || {
        ms += 20;
        recorder.sample(ms + 20, "input_test", counters)?;
        segment.phase(ms + 20, "input_test");
        Ok(ms)
    }).unwrap();
    for (ms, phase) in [(DISCOVERY_MS + 40, "gui_diagnostics"), (DISCOVERY_MS + 1000, "gui_error"), (DISCOVERY_MS + 2000, "session_end")] {
        recorder.sample(ms, phase, counters).unwrap();
        segment.phase(ms, phase);
    }
    let bundle = segment.freeze(DISCOVERY_MS + 2000, None).unwrap();

    assert!(recorder.report().error.is_none());
    assert_eq!(recorder.report().elapsed_ms, DISCOVERY_MS + 2000);
    assert!(recorder.report().phases.contains_key("input_test"));
    assert_eq!(bundle.metadata["segment"]["phases_ms"]["input_test"], DISCOVERY_MS);
    assert!(bundle.metadata["segment"]["phases_ms"].get("unknown").is_none());
    assert!(recorder.sample(DISCOVERY_MS + 3000, "arbitrary_phase", counters).is_err());
}

#[test]
fn discovery_accepts_late_presses_for_ten_seconds_but_each_following_step_stays_five() {
    let mut test = connected(3);
    assert_eq!(DISCOVERY_MS, 10_000);
    assert_eq!(STEP_MS, 5_000);
    assert_eq!(test.view(0).seconds, 10);
    tap(&mut test, 0, 7_500);
    tap(&mut test, 1, 9_999);
    assert_eq!(test.view(9_999).stage, Stage::Discover);
    assert_eq!(test.view(9_999).seconds, 1);

    tap(&mut test, 2, 10_000);

    let view = test.view(10_000);
    assert_eq!(view.count, 2);
    assert_eq!(view.stage, Stage::Prepare { number: 1 });
    assert_eq!(view.seconds, 4);
    sample(&mut test, 14_000);
    assert_eq!(test.view(14_000).stage, Stage::Controller { number: 1, step: 0, release: false });
    assert_eq!(test.view(14_000).seconds, 5);
    sample(&mut test, 19_000);
    assert_eq!(test.guide.index, 1);
    let report = test.finish(19_000, "cancelled");
    assert_eq!(report["discovery_ms"], 10_000);
    assert_eq!(report["unconfirmed"].as_array().unwrap().len(), 1);
}
