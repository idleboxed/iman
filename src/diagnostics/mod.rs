// SPDX-License-Identifier: BSD-3-Clause
//! Feature-selected collectors and bounded session reports.
#[cfg(any(feature = "diag-display", feature = "diag-input"))]
mod child;
#[cfg(any(
    feature = "diag-memory",
    feature = "diag-klog",
    feature = "diag-uart",
    feature = "diag-display",
    feature = "diag-input"
))]
pub mod collectors;
#[cfg(feature = "diag-cycle")]
pub mod cycle;
#[cfg(feature = "diag-display")]
pub mod display;
#[cfg(feature = "diag-cycle")]
pub mod export;
#[cfg(feature = "diag-input")]
pub mod input;
#[cfg(feature = "io-monitor")]
pub mod io;
#[cfg(feature = "io-monitor")]
pub mod io_context;
#[cfg(feature = "diag-klog")]
pub mod klog;
#[cfg(feature = "diag-memory")]
pub mod memory;
#[cfg(feature = "diag-perf")]
pub mod perf;
#[cfg(feature = "diag-uart")]
pub mod uart;
