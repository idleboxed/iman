// SPDX-License-Identifier: BSD-3-Clause
//! Input owner reports, not another evdev/HID reader or an emulator input hook.
pub use crate::diagnostics::child::{Clock, Entry, Report, HISTORY_LIMIT, REPORT_LIMIT, SAMPLE_LIMIT};
pub use imanlib::input_metrics::Sample;
pub type Monitor = crate::diagnostics::child::Monitor<Sample>;
impl crate::diagnostics::child::Payload for Sample {
    fn valid(&self) -> bool {
        self.validate()
    }
}
