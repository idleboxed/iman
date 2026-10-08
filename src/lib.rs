// SPDX-License-Identifier: BSD-3-Clause
//! Guarded Linux session orchestration, independent of CPU and firmware.
pub mod diagnostics;
pub mod display;
pub mod emulation;
#[cfg(feature = "diag-input")]
pub mod input_test;
pub mod profile;
pub mod runtime;
pub mod session;
