// SPDX-License-Identifier: BSD-3-Clause
//! Completed manager-owned input report and proposals; no GUI journal or SD writes.
use std::collections::BTreeMap;
use serde_json::Value;

#[derive(Default)]
pub struct Journal {
    manager: Option<Value>,
    proposal_files: BTreeMap<String, Vec<u8>>,
}

impl Journal {
    /// The manager moves in one completed report; no GUI IPC copy or queue.
    pub fn complete_manager(&mut self, report: Value) {
        self.manager = Some(report);
        self.proposal_files.clear();
    }

    pub fn complete_with_proposals(&mut self, completion: crate::input_test::Completion) {
        self.complete_manager(completion.report);
        self.proposal_files = completion.files;
    }

    /// Borrow the same segment's candidates; export takes an immutable copy.
    pub fn proposal_files(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.proposal_files
    }

    pub fn report(&self) -> Value {
        self.manager.clone().unwrap_or(Value::Null)
    }

    /// Reset only after the previous immutable segment was successfully exported.
    pub fn next_segment(&mut self) {
        self.manager = None;
        self.proposal_files.clear();
    }
}
