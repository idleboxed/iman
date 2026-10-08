// SPDX-License-Identifier: BSD-3-Clause
//! Synthetic manager reports only: no input readers, SD or hardware calibration.
#![cfg(feature = "diag-input")]
use iman::input_test::journal::Journal;
use serde_json::json;

#[test]
fn absent_manager_report_is_null_and_completion_preserves_the_report() {
    let mut journal = Journal::default();

    assert!(journal.report().is_null());
    assert!(journal.proposal_files().is_empty());
    let report = json!({"format":"IMAN-INPUT-TEST-2","owner":"iman","reason":"completed"});
    journal.complete_manager(report.clone());

    assert_eq!(journal.report(), report);
    assert!(journal.proposal_files().is_empty());
}

#[test]
fn candidates_belong_to_one_report_and_clear_with_the_segment_or_report_replacement() {
    let mut journal = Journal::default();
    let files = std::collections::BTreeMap::from([("pads-proposal/summary.json".into(), b"{}\n".to_vec())]);
    let report = json!({"owner":"iman"});
    journal.complete_with_proposals(iman::input_test::Completion { report: report.clone(), files: files.clone() });

    assert_eq!(journal.proposal_files(), &files);
    assert_eq!(journal.report(), report);
    journal.next_segment();

    assert!(journal.report().is_null());
    assert!(journal.proposal_files().is_empty());
    journal.complete_with_proposals(iman::input_test::Completion { report, files });
    journal.complete_manager(json!({"reason":"unavailable"}));

    assert_eq!(journal.report(), json!({"reason":"unavailable"}));
    assert!(journal.proposal_files().is_empty());
}
