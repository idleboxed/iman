// SPDX-License-Identifier: BSD-3-Clause
//! Pure, bounded candidate generation. Never installs a profile or assigns ports.
use super::guide::{axis_tolerance, ACTIONS};
use imanlib::gamepad::profile::Profile;
use imanlib::evdev::{find_retroarch_button_index, DeviceInfo};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub struct Participant<'a> {
    pub number: usize,
    pub generation: u32,
    pub info: &'a DeviceInfo,
    pub steps: &'a [Value],
}

#[derive(Default)]
pub struct Proposals {
    pub files: BTreeMap<String, Vec<u8>>,
    pub summary: Value,
}

#[derive(Clone, Copy)]
enum Binding {
    Key(u16),
    Axis {
        code: u8,
        baseline: i32,
        extreme: i32,
        direction: i8,
    },
}
impl Binding {
    fn token(self) -> (u16, u16, i8) {
        match self {
            Self::Key(code) => (1, code, 0),
            Self::Axis { code, direction, .. } => (3, u16::from(code), direction),
        }
    }
}

fn parse_identity(info: &DeviceInfo) -> Option<(u16, u16, String)> {
    let id = |field| u16::from_str_radix(info.identity.get(field)?, 16).ok();
    if info.validate().is_err() || id("id/bustype")? != 3 || info.name.is_empty()
        || info.name.chars().any(char::is_control)
    {
        return None;
    }
    Some((id("id/vendor")?, id("id/product")?, info.name.clone()))
}

fn parse_binding(participant: &Participant<'_>, index: usize) -> Result<Binding, &'static str> {
    let step = participant.steps.get(index).ok_or("missing_result")?;
    if step["step"].as_u64() != Some(index as u64) || step["action"] != ACTIONS[index] {
        return Err("invalid_step_result");
    }
    if step["outcome"] != "completed" {
        return Err("not_completed");
    }
    let control = step["control"].as_array().filter(|values| values.len() == 3).ok_or("invalid_control")?;
    if control[0].as_u64() != Some(u64::from(participant.generation)) {
        return Err("wrong_generation");
    }
    let code = control[2].as_u64().and_then(|value| u16::try_from(value).ok()).ok_or("invalid_code")?;
    match control[1].as_u64() {
        Some(1) if code != 0 && participant.info.key_codes.contains(&code) => Ok(Binding::Key(code)),
        Some(3) if code < 64 => {
            let code = code as u8;
            let info = participant.info.abs_info.get(&code).ok_or("missing_axis_range")?;
            let integer = |field| step["axis"][field].as_i64().and_then(|value| i32::try_from(value).ok());
            let baseline = integer("baseline").ok_or("missing_axis_evidence")?;
            let extreme = integer("extreme").ok_or("missing_axis_evidence")?;
            let direction = integer("direction").ok_or("missing_axis_evidence")?;
            let delta = i64::from(extreme) - i64::from(baseline);
            if info[1] >= info[2] || baseline != info[0] || !(info[1]..=info[2]).contains(&baseline)
                || !(info[1]..=info[2]).contains(&extreme) || !matches!(direction, -1 | 1)
                || delta.signum() != i64::from(direction) || delta.abs() <= i64::from(axis_tolerance(info)) {
                return Err("invalid_axis_evidence");
            }
            Ok(Binding::Axis { code, baseline, extreme, direction: direction as i8 })
        }
        _ => Err("unsupported_control"),
    }
}

fn has_same_capabilities(expected: &DeviceInfo, observed: &DeviceInfo) -> bool {
    expected.key_codes == observed.key_codes && expected.axis_codes == observed.axis_codes
        && expected.abs_info.len() == observed.abs_info.len()
        && expected.abs_info.iter().all(|(code, range)| {
            observed.abs_info.get(code).is_some_and(|other| range[1..] == other[1..])
        })
}

fn normalize_axis_value(info: &[i32; 6], value: i32) -> Option<i64> {
    let span = i64::from(info[2]) - i64::from(info[1]);
    // Upstream stores the range in a signed int. Do not propose overflow cases.
    if span <= 0 || span > i64::from(i32::MAX) {
        return None;
    }
    Some(((i64::from(value) - i64::from(info[1])) * 65535 / span - 32767).clamp(-32767, 32767))
}

fn map_retroarch_binding(info: &DeviceInfo, binding: Binding) -> Result<(&'static str, String), &'static str> {
    match binding {
        Binding::Key(code) => find_retroarch_button_index(&info.key_codes, code)
            .map(|index| ("btn", index.to_string())).ok_or("retroarch_button_unavailable"),
        Binding::Axis { code, baseline, extreme, .. } if (16..24).contains(&code) => {
            let range = &info.abs_info[&code];
            if (range[1], range[2], baseline) != (-1, 1, 0) || !matches!(extreme, -1 | 1) {
                return Err("unsupported_hat_range_or_neutral");
            }
            let direction = match (code % 2, extreme) {
                (0, -1) => "left", (0, 1) => "right", (1, -1) => "up", _ => "down",
            };
            Ok(("btn", format!("h{}{direction}", (code - 16) / 2)))
        }
        Binding::Axis { code, baseline, extreme, .. } => {
            if info.axis_codes.iter().any(|code| !info.abs_info.contains_key(code)) {
                return Err("incomplete_axis_metadata");
            }
            // RA 1.22.2 udev enumerates valid ABS codes below ABS_MISC, skipping
            // all hats. These are compact indices, NOT the Linux ABS codes.
            let index = (0..40u8).filter(|axis_code| !(16..24).contains(axis_code))
                .filter(|axis_code| info.abs_info.get(axis_code).is_some_and(|range| range[2] > range[1]))
                .take(32).position(|axis_code| axis_code == code).ok_or("retroarch_axis_unavailable")?;
            let scaled = |value| -> Option<i64> {
                let mut value = normalize_axis_value(&info.abs_info[&code], value)?;
                // Upstream initializes neg_trigger by raw code, but queries it
                // by compact index, and only for indices 2/5. Mirror that exact
                // interface; reject layouts that would stay active at rest.
                if matches!(index, 2 | 5) && info.abs_info.get(&(index as u8))
                    .and_then(|range| normalize_axis_value(range, range[0]))
                    .is_some_and(|value| value < -1300)
                {
                    value = (value + 32767) / 2;
                }
                Some(value)
            };
            let neutral = scaled(baseline).ok_or("unsupported_axis_range")?;
            let peak = scaled(extreme).ok_or("unsupported_axis_range")?;
            if neutral.abs() > 1300 || peak.abs() <= 16383 {
                return Err("retroarch_axis_not_neutral_or_insufficient_excursion");
            }
            Ok(("axis", format!("{}{index}", if peak < 0 { "-" } else { "+" })))
        }
    }
}

// A pair's negative/positive prompts must identify opposite halves of one axis.
fn find_axis_pair(bindings: &[Option<Binding>], negative: usize, positive: usize) -> Option<(u8, bool)> {
    match (bindings[negative]?, bindings[positive]?) {
        (
            Binding::Axis { code: negative_code, direction: negative_direction, .. },
            Binding::Axis { code: positive_code, direction: positive_direction, .. },
        ) if negative_code == positive_code && negative_direction == -positive_direction => {
            Some((negative_code, negative_direction > 0))
        }
        _ => None,
    }
}

fn build_igui_profile(info: &DeviceInfo, vendor: u16, product: u16, bindings: &[Option<Binding>]) -> Value {
    let mut buttons = BTreeMap::new();
    for (index, name) in [(4, "south"), (5, "east"), (6, "west"), (7, "north"), (8, "back"), (9, "start")] {
        if let Some(Binding::Key(code)) = bindings[index] {
            buttons.insert(name, code);
        }
    }
    if (0..4).all(|index| matches!(bindings[index], Some(Binding::Key(_)))) {
        for (index, name) in ["up", "down", "left", "right"].into_iter().enumerate() {
            if let Some(Binding::Key(code)) = bindings[index] {
                buttons.insert(name, code);
            }
        }
    }
    let mut axes = BTreeMap::new();
    for (x_role, y_role, x_pair, y_pair, hat) in [
        ("left_stick_x", "left_stick_y", find_axis_pair(bindings, 19, 20), find_axis_pair(bindings, 17, 18), false),
        ("dpad_x", "dpad_y", find_axis_pair(bindings, 2, 3), find_axis_pair(bindings, 0, 1), true),
    ] {
        if let (Some((x_code, x_invert)), Some((y_code, y_invert))) = (x_pair, y_pair) {
            let valid = [x_code, y_code].into_iter().all(|code| {
                let range = &info.abs_info[&code];
                if hat {
                    (range[1], range[2], range[0]) == (-1, 1, 0)
                } else {
                    normalize_axis_value(range, range[0]).is_some_and(|value| value.abs() <= 1300)
                }
            });
            if valid {
                axes.insert(x_role, json!({"code":x_code,"invert":x_invert}));
                axes.insert(y_role, json!({"code":y_code,"invert":y_invert}));
            }
        }
    }
    json!({"name":info.name,"backend":"evdev","access":"read-only",
        "match":{"bus":"usb","vendor":format!("{vendor:04x}"),"product":format!("{product:04x}")},
        "buttons":buttons,"axes":axes,"proposal":{"candidate_only":true,"physical_labels_verified":false}})
}

fn serialize_json(value: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).expect("JSON values are serializable");
    bytes.push(b'\n');
    bytes
}

pub fn generate(participants: &[Participant<'_>]) -> Proposals {
    let mut output = Proposals { summary: json!({"candidate_only":true,"applied":false,
        "physical_labels_verified":false,"port_assignment":false,"groups":[],"rejected":[],
        "retroarch_driver":"udev","retroarch_version":"1.22.2",
        "neutral_assumption":"opening snapshot; physical neutral not verified"}), ..Proposals::default() };
    if participants.len() > super::MAX_CONTROLLERS {
        output.summary["error"] = json!("participant_limit_exceeded");
        return output;
    }
    let mut groups: BTreeMap<_, Vec<&Participant<'_>>> = BTreeMap::new();
    for participant in participants {
        if participant.number == 0 || participant.number > super::MAX_CONTROLLERS
            || participant.generation == 0 || participant.steps.len() > ACTIONS.len() {
            output.summary["rejected"].as_array_mut().unwrap().push(json!({"number":participant.number,"reason":"invalid_participant"}));
        } else if let Some(identity) = parse_identity(participant.info) {
            groups.entry(identity).or_default().push(participant);
        } else {
            output.summary["rejected"].as_array_mut().unwrap().push(json!({"number":participant.number,"reason":"invalid_usb_identity_or_metadata"}));
        }
    }
    let mut basenames = BTreeSet::new();
    for ((vendor, product, name), group) in groups {
        let digest = format!("{:x}", Sha256::digest(name.as_bytes()));
        let basename = format!("{vendor:04x}-{product:04x}-{}", &digest[..16]);
        let info = group[0].info;
        let mut summary = json!({"vendor":format!("{vendor:04x}"),"product":format!("{product:04x}"),
            "name":name,"basename":basename,"contributors":group.iter().map(|participant|
                json!({"number":participant.number,"generation":participant.generation})).collect::<Vec<_>>(),
            "files":[],"skipped":[]});
        if !basenames.insert(basename.clone()) {
            summary["reason"] = json!("filename_hash_collision");
            output.summary["groups"].as_array_mut().unwrap().push(summary);
            continue;
        }
        if group.iter().any(|participant| !has_same_capabilities(info, participant.info)) {
            summary["reason"] = json!("inconsistent_capabilities");
            output.summary["groups"].as_array_mut().unwrap().push(summary);
            continue;
        }
        let observations: Vec<Vec<_>> = (0..ACTIONS.len()).map(|index| {
            group.iter().filter_map(|participant| {
                parse_binding(participant, index).ok().map(|binding| (*participant, binding))
            }).collect()
        }).collect();
        let mut bindings: Vec<_> = observations.iter().map(|observations| {
            observations.first().map(|(_, binding)| *binding)
        }).collect();
        let mut reasons = BTreeMap::new();
        for (index, observed) in observations.iter().enumerate() {
            if observed.is_empty() {
                reasons.insert(index, "no_confirmed_control");
            } else if observed.iter().any(|(_, binding)| binding.token() != bindings[index].unwrap().token()) {
                bindings[index] = None;
                reasons.insert(index, "conflicting_observations");
            }
        }
        let mut aliases = BTreeMap::new();
        for (index, binding) in bindings.iter().enumerate() {
            if let Some(binding) = binding {
                aliases.entry(binding.token()).or_insert_with(Vec::new).push(index);
            }
        }
        for indices in aliases.values().filter(|indices| indices.len() > 1) {
            for &index in indices {
                bindings[index] = None;
                reasons.insert(index, "aliased_control");
            }
        }
        // Do not create half of a stick pair or infer its missing opposite half.
        for (negative, positive) in [(17, 18), (19, 20), (21, 22), (23, 24)] {
            if find_axis_pair(&bindings, negative, positive).is_none() {
                for index in [negative, positive] {
                    bindings[index] = None;
                    reasons.entry(index).or_insert("unconfirmed_axis_pair");
                }
            }
        }
        let mut igui_bindings = bindings.clone();
        for (index, observed) in observations.iter().enumerate() {
            if matches!(igui_bindings[index], Some(Binding::Axis { .. })) && observed.iter().any(|(participant, binding)| match *binding {
                Binding::Axis { code, baseline, extreme, .. } if !(16..24).contains(&code) => {
                    let range = &participant.info.abs_info[&code];
                    normalize_axis_value(range, baseline).is_none_or(|value| value.abs() > 1300)
                        || normalize_axis_value(range, extreme).is_none_or(|value| value.abs() <= 16383)
                }
                _ => false,
            }) {
                igui_bindings[index] = None;
            }
        }
        let document = build_igui_profile(info, vendor, product, &igui_bindings);
        let bytes = serialize_json(&document);
        match Profile::parse(&bytes) {
            Ok(_) => {
                let path = format!("pads-proposal/{basename}.json");
                summary["files"].as_array_mut().unwrap().push(json!(path));
                output.files.insert(path, bytes);
            }
            Err(error) => summary["igui_unavailable"] = json!(error.to_string()),
        }
        // Quoting/backslash behavior is not assumed for arbitrary kernel names.
        if name.contains(['"', '\\']) || name.len() >= 128 {
            summary["retroarch_unavailable"] = json!("unsafe_or_oversized_device_name");
        } else {
            let mut cfg = format!("# SPDX-License-Identifier: BSD-3-Clause\n# Candidate only; physical labels and neutral not verified. Not applied.\n\
                input_driver = \"udev\"\ninput_device = \"{name}\"\ninput_vendor_id = {vendor}\ninput_product_id = {product}\n");
            let mut count = 0;
            let names = ["up", "down", "left", "right", "b", "a", "y", "x", "select", "start", "l", "r",
                "l2", "r2", "l3", "r3", "menu_toggle", "l_y_minus", "l_y_plus", "l_x_minus", "l_x_plus",
                "r_y_minus", "r_y_plus", "r_x_minus", "r_x_plus"];
            for (index, binding) in bindings.iter().enumerate() {
                if binding.is_none() {
                    continue;
                }
                let mapped: Result<BTreeSet<_>, _> = observations[index].iter()
                    .map(|(participant, binding)| map_retroarch_binding(participant.info, *binding)).collect();
                match mapped {
                    Ok(mapped) if mapped.len() == 1 => {
                        let (kind, value) = mapped.into_iter().next().unwrap();
                        if index >= 17 && kind != "axis" {
                            reasons.insert(index, "stick_requires_axis");
                            continue;
                        }
                        cfg.push_str(&format!("input_{}_{kind} = \"{value}\"\n", names[index]));
                        count += 1;
                    }
                    Ok(_) => {
                        reasons.insert(index, "conflicting_retroarch_bindings");
                    }
                    Err(reason) => {
                        reasons.insert(index, reason);
                    }
                }
            }
            if count > 0 {
                let path = format!("pads-proposal/{basename}.cfg");
                summary["files"].as_array_mut().unwrap().push(json!(path));
                output.files.insert(path, cfg.into_bytes());
            } else {
                summary["retroarch_unavailable"] = json!("no_supported_confirmed_bindings");
            }
            summary["retroarch_bindings"] = json!(count);
        }
        summary["skipped"] = json!(reasons.into_iter().map(|(step, reason)|
            json!({"step":step,"action":ACTIONS[step],"reason":reason})).collect::<Vec<_>>());
        output.summary["groups"].as_array_mut().unwrap().push(summary);
    }
    if !participants.is_empty() {
        output.files.insert("pads-proposal/summary.json".into(), serialize_json(&output.summary));
    }
    output
}
