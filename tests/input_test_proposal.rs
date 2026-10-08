// SPDX-License-Identifier: BSD-3-Clause
//! Synthetic capabilities and prompts, not an Xbox hardware measurement.
#![cfg(feature = "diag-input")]
use imanlib::gamepad::profile::Profile;
use imanlib::evdev::{DeviceInfo, Event};
use iman::input_test::{proposal::{generate, Participant, Proposals}, guide::ACTIONS, InputTest, DISCOVERY_MS, PREPARE_MS};
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn info() -> DeviceInfo {
    let mut abs_info = BTreeMap::new();
    for code in [0, 1, 3, 4] { abs_info.insert(code, [0, -32768, 32767, 0, 0, 0]); }
    for code in [2, 5] { abs_info.insert(code, [0, 0, 255, 0, 0, 0]); }
    for code in [16, 17] { abs_info.insert(code, [0, -1, 1, 0, 0, 0]); }
    DeviceInfo { path: "/synthetic/event0".into(), name: "Synthetic Xbox 360 replica".into(),
        key_codes: vec![304, 305, 307, 308, 310, 311, 314, 315, 316, 317, 318],
        axis_codes: abs_info.keys().copied().collect(), abs_info,
        identity: BTreeMap::from([("id/bustype".into(), "0003".into()), ("id/vendor".into(), "1234".into()),
            ("id/product".into(), "5678".into())]) }
}

fn steps(info: &DeviceInfo, generation: u32) -> Vec<Value> {
    let keys = [304, 305, 307, 308, 314, 315, 310, 311, 0, 0, 317, 318, 316];
    let mut steps: Vec<_> = ACTIONS.iter().enumerate().map(|(step, action)|
        json!({"step":step,"action":action,"outcome":"no_response","control":null})).collect();
    for (offset, key) in keys.into_iter().enumerate().filter(|(_, key)| *key != 0) {
        steps[offset + 4]["outcome"] = json!("completed");
        steps[offset + 4]["control"] = json!([generation, 1, key]);
    }
    for (step, code, positive) in [(0,17,false),(1,17,true),(2,16,false),(3,16,true),(12,2,true),(13,5,true),
        (17,1,false),(18,1,true),(19,0,false),(20,0,true),(21,4,false),(22,4,true),(23,3,false),(24,3,true)] {
        set_axis(&mut steps, info, generation, step, code, positive);
    }
    steps
}

fn set_axis(steps: &mut [Value], info: &DeviceInfo, generation: u32, step: usize, code: u8, positive: bool) {
    let range = info.abs_info.get(&code).unwrap();
    steps[step]["outcome"] = json!("completed");
    steps[step]["control"] = json!([generation, 3, code]);
    steps[step]["axis"] = json!({"baseline":range[0],"extreme":range[if positive {2} else {1}],"direction":if positive {1} else {-1}});
}

fn proposals(info: &DeviceInfo, steps: &[Value]) -> Proposals {
    generate(&[Participant { number: 1, generation: 1, info, steps }])
}

fn cfg(output: &Proposals) -> String {
    String::from_utf8(output.files.iter().find(|(name, _)| name.ends_with(".cfg")).unwrap().1.clone()).unwrap()
}

#[test]
fn candidates_have_stable_id_and_name_hash_and_physical_retro_pad_positions() {
    let info = info();
    let steps = steps(&info, 1);

    let output = proposals(&info, &steps);

    assert_eq!(output.files.len(), 3);
    let profile_path = output.files.keys().find(|name| name.ends_with(".json") && !name.ends_with("summary.json")).unwrap();
    assert!(profile_path.starts_with("pads-proposal/1234-5678-"));
    assert_eq!(profile_path.strip_prefix("pads-proposal/").unwrap().len(), 31);
    let bytes = &output.files[profile_path];
    assert!(Profile::parse(bytes).unwrap().matches(3, 0x1234, 0x5678, &info.name));
    let document: Value = serde_json::from_slice(bytes).unwrap();
    assert_eq!(document["buttons"]["south"], 304);
    assert_eq!(document["buttons"]["west"], 307);
    assert_eq!(document["axes"]["left_stick_x"], json!({"code":0,"invert":false}));
    let cfg = cfg(&output);
    for line in ["input_device = \"Synthetic Xbox 360 replica\"", "input_vendor_id = 4660", "input_product_id = 22136",
        "input_b_btn = \"0\"", "input_a_btn = \"1\"", "input_y_btn = \"2\"", "input_x_btn = \"3\"",
        "input_up_btn = \"h0up\"", "input_right_btn = \"h0right\"", "input_l2_axis = \"+2\"", "input_r2_axis = \"+5\"",
        "input_l_y_minus_axis = \"-1\"", "input_r_x_plus_axis = \"+3\"", "input_menu_toggle_btn = \"8\""] {
        assert!(cfg.contains(line), "missing {line} in {cfg}");
    }
    assert!(!cfg.contains("player"));
    assert_eq!(output.summary["applied"], false);
    assert_eq!(output.summary["physical_labels_verified"], false);
    assert_eq!(proposals(&info, &steps).files, output.files);
}

#[test]
fn equal_copies_merge_but_distinct_names_always_have_distinct_suffixed_filenames() {
    let first = info();
    let mut second = first.clone();
    second.path = "/synthetic/event1".into();
    let steps1 = steps(&first, 1);
    let steps2 = steps(&second, 2);
    let merged = generate(&[Participant { number: 1, generation: 1, info: &first, steps: &steps1 },
        Participant { number: 2, generation: 2, info: &second, steps: &steps2 }]);
    second.name = "Another replica".into();
    let separate = generate(&[Participant { number: 1, generation: 1, info: &first, steps: &steps1 },
        Participant { number: 2, generation: 2, info: &second, steps: &steps2 }]);

    assert_eq!(merged.files.len(), 3);
    assert_eq!(merged.summary["groups"][0]["contributors"].as_array().unwrap().len(), 2);
    assert_eq!(separate.files.len(), 5);
    assert_eq!(separate.summary["groups"].as_array().unwrap().len(), 2);
    assert!(separate.files.keys().filter(|name| name.ends_with(".cfg")).all(|name|
        name.starts_with("pads-proposal/1234-5678-") && name.len() == "pads-proposal/".len() + 30));
}

#[test]
fn conflicting_bindings_and_aliased_home_start_are_not_guessed() {
    let info = info();
    let mut steps1 = steps(&info, 1);
    let mut steps2 = steps(&info, 2);
    steps2[4]["control"] = json!([2,1,305]);
    steps2[5]["control"] = json!([2,1,304]);
    steps1[16]["control"] = json!([1,1,315]);
    steps2[16]["control"] = json!([2,1,315]);

    let output = generate(&[Participant { number: 1, generation: 1, info: &info, steps: &steps1 },
        Participant { number: 2, generation: 2, info: &info, steps: &steps2 }]);

    assert_eq!(output.files.len(), 2);
    assert!(output.summary["groups"][0]["igui_unavailable"].is_string());
    let cfg = cfg(&output);
    for name in ["input_a_", "input_b_", "input_start_", "input_menu_toggle_"] { assert!(!cfg.contains(name)); }
    assert!(output.summary.to_string().contains("conflicting_observations"));
    assert!(output.summary.to_string().contains("aliased_control"));
}

#[test]
fn changed_capabilities_for_the_same_identity_refuse_both_candidate_profiles() {
    let first = info();
    let mut second = first.clone();
    second.key_codes.push(319);
    let steps1 = steps(&first, 1);
    let steps2 = steps(&second, 2);

    let output = generate(&[Participant { number: 1, generation: 1, info: &first, steps: &steps1 },
        Participant { number: 2, generation: 2, info: &second, steps: &steps2 }]);

    assert_eq!(output.files.len(), 1);
    assert_eq!(output.summary["groups"][0]["reason"], "inconsistent_capabilities");
}

#[test]
fn axis_indices_are_compact_and_do_not_include_hats_or_missing_axes() {
    let mut info = info();
    let mut steps = steps(&info, 1);
    info.abs_info.retain(|code, _| [0,3,16,17].contains(code));
    info.axis_codes = info.abs_info.keys().copied().collect();
    for step in [12,13,21,22,23,24] { steps[step]["outcome"] = json!("no_response"); }
    set_axis(&mut steps, &info, 1, 17, 3, false);
    set_axis(&mut steps, &info, 1, 18, 3, true);

    let output = proposals(&info, &steps);

    assert!(cfg(&output).contains("input_l_y_minus_axis = \"-1\""));
    assert!(!cfg(&output).contains("input_l_y_minus_axis = \"-3\""));
    assert!(cfg(&output).contains("input_up_btn = \"h0up\""));
    let bytes = output.files.iter().find(|(path, _)| path.ends_with(".json") && !path.ends_with("summary.json")).unwrap().1;
    assert!(Profile::parse(bytes).is_ok());
}

#[test]
fn unsupported_trigger_neutral_is_omitted_instead_of_being_active_at_rest() {
    let mut info = info();
    let mut steps = steps(&info, 1);
    info.abs_info.remove(&1);
    info.axis_codes.retain(|code| *code != 1);
    for index in [17,18] { steps[index]["outcome"] = json!("no_response"); }

    let output = proposals(&info, &steps);

    assert!(!cfg(&output).contains("input_l2_axis"));
    assert!(!cfg(&output).contains("input_r2_axis"));
    assert!(output.summary.to_string().contains("retroarch_axis_not_neutral"));
    steps[12]["outcome"] = json!("ambiguous");
    assert!(!cfg(&proposals(&info, &steps)).contains("input_l2_"));
}

#[test]
fn digital_navigation_without_axes_produces_a_valid_igui_candidate() {
    let mut info = info();
    let mut steps = steps(&info, 1);
    info.abs_info.clear();
    info.axis_codes.clear();
    info.key_codes.splice(0..0, [103,105,106,108]);
    for (step, code) in [103,108,105,106].into_iter().enumerate() { steps[step]["control"] = json!([1,1,code]); }
    for index in [12,13,17,18,19,20,21,22,23,24] { steps[index]["outcome"] = json!("no_response"); }

    let output = proposals(&info, &steps);
    let bytes = output.files.iter().find(|(path, _)| path.ends_with(".json") && !path.ends_with("summary.json")).unwrap().1;
    let profile: Value = serde_json::from_slice(bytes).unwrap();

    assert_eq!(profile["axes"], json!({}));
    assert!(Profile::parse(bytes).is_ok());
    assert!(cfg(&output).contains("input_up_btn = \"0\""));
}

#[test]
fn cancelled_wrong_generation_held_and_missing_axis_evidence_never_make_bindings() {
    let info = info();
    let mut steps = steps(&info, 1);
    steps[4]["outcome"] = json!("cancelled");
    steps[5]["control"][0] = json!(2);
    steps[6]["outcome"] = json!("held");
    steps[7]["outcome"] = json!("disconnected");
    steps[12].as_object_mut().unwrap().remove("axis");

    let output = proposals(&info, &steps);

    assert!(output.summary["groups"][0]["igui_unavailable"].is_string());
    for name in ["input_b_", "input_a_", "input_y_", "input_x_", "input_l2_"] { assert!(!cfg(&output).contains(name)); }
    assert_eq!(generate(&[]).files.len(), 0);
    let empty = proposals(&info, &[]);
    assert_eq!(empty.files.len(), 1);
}

#[test]
fn inverted_axes_are_preserved_and_unsafe_cfg_names_cannot_inject_settings() {
    let mut info = info();
    let mut steps = steps(&info, 1);
    set_axis(&mut steps, &info, 1, 19, 0, true);
    set_axis(&mut steps, &info, 1, 20, 0, false);
    info.name = "Replica \"bad quote".into();

    let output = proposals(&info, &steps);

    assert!(output.files.keys().all(|path| !path.ends_with(".cfg")));
    let bytes = output.files.iter().find(|(path, _)| path.ends_with(".json") && !path.ends_with("summary.json")).unwrap().1;
    assert_eq!(serde_json::from_slice::<Value>(bytes).unwrap()["axes"]["left_stick_x"]["invert"], true);
    assert!(Profile::parse(bytes).is_ok());
    assert_eq!(output.summary["groups"][0]["retroarch_unavailable"], "unsafe_or_oversized_device_name");
}

#[test]
fn manager_keeps_compact_outcomes_and_selected_metadata_after_disconnect_and_raw_truncation() {
    let info = info();
    let evidence = steps(&info, 1);
    let mut test = InputTest::default();
    let slot = test.probe.connect(info, 0).unwrap();
    test.probe.baseline(slot, &[], 0).unwrap();
    let sample = |test: &mut InputTest, ms| { test.probe.sampled(ms); test.refresh(ms); };
    let key = |test: &mut InputTest, code, ms| {
        for value in [1,0] { test.probe.event(slot, Event { kind:1, code, value }, ms); }
        sample(test, ms);
    };
    key(&mut test, 304, 100);
    for ms in 200..1200 { key(&mut test, 304, ms); }
    sample(&mut test, DISCOVERY_MS);
    let mut ms = DISCOVERY_MS + PREPARE_MS;
    sample(&mut test, ms);
    for result in &evidence {
        ms += 100;
        sample(&mut test, ms);
        ms += 100;
        let kind = result["control"][1].as_u64().unwrap() as u16;
        let code = result["control"][2].as_u64().unwrap() as u16;
        let values = if kind == 1 { [1,0] } else {
            [result["axis"]["extreme"].as_i64().unwrap() as i32, result["axis"]["baseline"].as_i64().unwrap() as i32]
        };
        for value in values { test.probe.event(slot, Event { kind, code, value }, ms); }
        sample(&mut test, ms);
    }
    test.probe.disconnect(slot, ms + 1, "synthetic disconnect");
    sample(&mut test, ms + 1);

    let completion = test.finish_with_proposals(ms + 1, "completed");

    assert_eq!(completion.files.len(), 3);
    assert!(completion.report["records_omitted"].as_u64().unwrap() > 0);
    assert_eq!(completion.report["controllers"][0]["steps"].as_array().unwrap().len(), 25);
    assert!(completion.report["controllers"][0]["steps"].as_array().unwrap().iter().all(|result| result["outcome"] == "completed"));
    assert_eq!(completion.report["mapping_written"], false);
    assert_eq!(completion.report["proposals"]["applied"], false);
}

#[test]
fn four_distinct_models_fit_the_existing_file_count_and_proposal_byte_budget() {
    for confirmed in [false, true] {
        let infos: Vec<_> = (0..4).map(|index| {
            let mut info = info();
            info.name = format!("Synthetic replica {index}");
            info
        }).collect();
        let results: Vec<_> = infos.iter().enumerate().map(|(index, info)| {
            let mut steps = steps(info, index as u32 + 1);
            if !confirmed { for step in &mut steps { step["outcome"] = json!("no_response"); } }
            steps
        }).collect();

        let output = generate(&infos.iter().zip(&results).enumerate().map(|(index, (info, steps))|
            Participant { number: index + 1, generation: index as u32 + 1, info, steps }).collect::<Vec<_>>());
        let mut bundle = iman::diagnostics::export::Bundle::new(json!({}));
        bundle.files.insert("launch.log".into(), vec![b' '; iman::diagnostics::export::FILE_LIMIT]);
        bundle.json("diag_input_test.json", &json!({"owner":"iman"})).unwrap();
        bundle.files.extend(output.files.clone());

        assert_eq!(output.summary["groups"].as_array().unwrap().len(), 4);
        assert_eq!(output.files.len(), if confirmed { 9 } else { 1 });
        assert!(output.files.values().map(Vec::len).sum::<usize>() <= iman::diagnostics::export::PROPOSAL_LIMIT);
        assert!(bundle.manifest().is_ok());
    }
}
