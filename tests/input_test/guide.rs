// SPDX-License-Identifier: BSD-3-Clause
#![cfg(feature = "diag-input")]
use iman::input_test::guide::{axis_tolerance, Axes, AxisState, TimedGuide, ACTIONS};
use serde_json::{json, Value};

fn raw(kind: u16, code: u16, value: i32) -> Value {
    json!({"event":"raw","generation":1,"kind":kind,"code":code,"value":value})
}

#[test]
fn no_device_finishes_all_25_steps_by_125_seconds_without_start_confirmation() {
    let mut guide = TimedGuide::default();
    let records = guide.sample(125_000, &[], false, true, &Axes::new());
    assert!(guide.done());
    assert_eq!(guide.results.len(), 25);
    assert_eq!(guide.results.iter().map(|value| value["action"].as_str().unwrap()).collect::<Vec<_>>(), ACTIONS);
    assert!(guide.results.iter().all(|record| record["outcome"] == "no_response"));
    assert_eq!(records.iter().filter(|record| record["event"] == "step_result").count(), 25);
}

#[test]
fn key_tap_completes_but_repeat_held_and_multiple_controls_do_not() {
    let mut guide = TimedGuide::default();
    guide.sample(100, &[], true, true, &Axes::new());
    guide.sample(200, &[raw(1, 103, 1), raw(1, 103, 2), raw(1, 103, 0)], true, true, &Axes::new());
    assert_eq!(guide.index, 1);
    assert_eq!(guide.results[0]["outcome"], "completed");
    guide.sample(300, &[], true, true, &Axes::new());
    guide.sample(400, &[raw(1, 103, 1), raw(1, 105, 1)], true, false, &Axes::new());
    guide.sample(5_200, &[], true, false, &Axes::new());
    assert_eq!(guide.results[1]["outcome"], "ambiguous");
    guide.sample(5_400, &[raw(1, 103, 0), raw(1, 105, 0)], true, true, &Axes::new());
    assert_eq!(guide.index, 2);
}

#[test]
fn axis_noise_and_near_neutral_return_use_flat_fuzz_without_rebaselining_held_axis() {
    assert_eq!(axis_tolerance(&[0, -1, 1, 10, 10, 0]), 0);
    assert_eq!(axis_tolerance(&[0, -100, 100, 1, 3, 0]), 3);
    let mut axes = Axes::from([((1, 0), AxisState { value: 0, baseline: 0, tolerance: 3 })]);
    let mut guide = TimedGuide::default();
    guide.sample(100, &[], true, true, &axes);
    axes.get_mut(&(1, 0)).unwrap().value = 2;
    guide.sample(200, &[raw(3, 0, 2)], true, true, &axes);
    assert_eq!(guide.index, 0);
    axes.get_mut(&(1, 0)).unwrap().value = 80;
    guide.sample(300, &[raw(3, 0, 80)], true, true, &axes);
    axes.get_mut(&(1, 0)).unwrap().value = -2;
    guide.sample(400, &[raw(3, 0, -2)], true, true, &axes);
    assert_eq!(guide.results[0]["observed_kind"], "axis");
    assert_eq!(guide.results[0]["outcome"], "completed");
    guide.sample(500, &[], true, true, &axes);
    axes.get_mut(&(1, 0)).unwrap().value = 80;
    guide.sample(600, &[raw(3, 0, 80)], true, true, &axes);
    guide.sample(5_400, &[], true, true, &axes);
    assert_eq!(guide.results[1]["outcome"], "held");
    assert!(!guide.armed);
    guide.sample(5_600, &[], true, true, &axes);
    assert!(!guide.armed);
    axes.get_mut(&(1, 0)).unwrap().value = 0;
    guide.sample(5_700, &[raw(3, 0, 0)], true, true, &axes);
    assert_eq!(guide.index, 2);
    guide.sample(5_800, &[], true, true, &axes);
    assert!(guide.armed);
}

#[test]
fn continuous_neutral_jitter_does_not_prevent_arming_or_key_taps() {
    let mut guide = TimedGuide::default();
    let mut axes = Axes::from([((1, 0), AxisState {value: 0, baseline: 0, tolerance: 3})]);
    for ms in (0..=120).step_by(20) {
        axes.get_mut(&(1, 0)).unwrap().value = 1;
        guide.sample(ms, &[raw(3, 0, 1)], true, true, &axes);
    }
    assert!(guide.armed);
    guide.sample(140, &[raw(1, 103, 1), raw(3, 0, 1), raw(1, 103, 0)], true, true, &axes);
    assert_eq!(guide.results[0]["outcome"], "completed");
}

#[test]
fn conflicting_control_in_incomplete_sample_cannot_confirm_the_original_candidate() {
    let mut guide = TimedGuide::default();
    guide.sample(100, &[], true, true, &Axes::new());
    guide.sample(200, &[raw(1, 103, 1)], true, false, &Axes::new());
    guide.sample(250, &[raw(1, 105, 1), raw(1, 105, 0)], false, false, &Axes::new());
    guide.sample(300, &[raw(1, 103, 0)], true, true, &Axes::new());
    guide.sample(5_000, &[], true, true, &Axes::new());
    assert_eq!(guide.results[0]["outcome"], "ambiguous");
}

#[test]
fn sole_key_takes_precedence_over_compound_trigger_axis() {
    let mut axes = Axes::from([((1, 10), AxisState { value: 0, baseline: 0, tolerance: 2 })]);
    let mut guide = TimedGuide::default();
    guide.sample(100, &[], true, true, &axes);

    axes.get_mut(&(1, 10)).unwrap().value = 255;
    guide.sample(200, &[raw(3, 10, 255), raw(1, 44, 1)], true, false, &axes);
    axes.get_mut(&(1, 10)).unwrap().value = 0;
    guide.sample(300, &[raw(3, 10, 0), raw(1, 44, 0)], true, true, &axes);

    assert_eq!(guide.index, 1);
    assert_eq!(guide.results[0]["outcome"], "completed");
    assert_eq!(guide.results[0]["control"], json!([1, 1, 44]));
    assert_eq!(guide.results[0]["observed_kind"], "key");
}

#[test]
fn compound_axis_does_not_hide_two_distinct_key_presses() {
    let mut axes = Axes::from([((1, 10), AxisState { value: 0, baseline: 0, tolerance: 2 })]);
    let mut guide = TimedGuide::default();
    guide.sample(100, &[], true, true, &axes);

    axes.get_mut(&(1, 10)).unwrap().value = 255;
    guide.sample(200, &[raw(3, 10, 255), raw(1, 44, 1), raw(1, 46, 1)], true, false, &axes);
    axes.get_mut(&(1, 10)).unwrap().value = 0;
    guide.sample(300, &[raw(3, 10, 0), raw(1, 44, 0), raw(1, 46, 0)], true, true, &axes);
    guide.sample(5_000, &[], true, true, &axes);

    assert_eq!(guide.results[0]["outcome"], "ambiguous");
}

#[test]
fn axis_result_reserves_baseline_peak_and_direction_independently_of_raw_records() {
    let axes = Axes::from([((1, 0), AxisState { value: 128, baseline: 128, tolerance: 3 })]);
    let mut guide = TimedGuide::default();
    guide.sample(100, &[], true, true, &axes);

    guide.sample(200, &[raw(3, 0, 90), raw(3, 0, 0), raw(3, 0, 128)], true, true, &axes);

    assert_eq!(guide.results[0]["outcome"], "completed");
    assert_eq!(guide.results[0]["axis"], json!({"baseline":128,"extreme":0,"direction":-1}));
    guide.sample(300, &[], true, true, &axes);
    guide.sample(400, &[raw(1, 304, 1), raw(1, 304, 0)], true, true, &axes);
    assert!(guide.results[1]["axis"].is_null());
}

#[test]
fn opposite_axis_excursions_in_one_prompt_are_ambiguous_and_invalidated_evidence_is_cleared() {
    let axes = Axes::from([((1, 0), AxisState { value: 0, baseline: 0, tolerance: 3 })]);
    let mut guide = TimedGuide::default();
    guide.sample(100, &[], true, true, &axes);
    guide.sample(200, &[raw(3, 0, -100), raw(3, 0, 100), raw(3, 0, 0)], true, true, &axes);
    guide.sample(5_000, &[], true, true, &axes);
    assert_eq!(guide.results[0]["outcome"], "ambiguous");
    guide.sample(5_100, &[], true, true, &axes);
    guide.sample(5_200, &[raw(3, 0, 100)], true, false, &axes);

    guide.invalidate();
    guide.cancel_remaining(5_300, "cancelled");

    assert!(guide.results[1]["axis"].is_null());
    assert_eq!(guide.results[1]["outcome"], "cancelled");
}
