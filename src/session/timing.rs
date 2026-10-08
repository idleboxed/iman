// SPDX-License-Identifier: BSD-3-Clause
//! Explicit timing for session intervals; process and IPC polling stay real.
use std::{
    thread,
    time::{Duration, Instant},
};

/// A monotonic clock and wait operation, replaceable together in offline tests.
pub trait Timing {
    fn now(&self) -> Duration;
    fn wait(&mut self, duration: Duration);
}

pub struct SystemTiming {
    started: Instant,
}

impl Default for SystemTiming {
    fn default() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl Timing for SystemTiming {
    fn now(&self) -> Duration {
        self.started.elapsed()
    }

    fn wait(&mut self, duration: Duration) {
        thread::sleep(duration);
    }
}
