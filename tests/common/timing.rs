// SPDX-License-Identifier: BSD-3-Clause
//! Shared monotonic test time; advancing a timer never sleeps on the host.
use iman::session::timing::Timing;
use std::{cell::Cell, rc::Rc, time::Duration};

#[derive(Clone, Default)]
pub struct ManualTiming {
    elapsed: Rc<Cell<Duration>>,
}

impl Timing for ManualTiming {
    fn now(&self) -> Duration {
        self.elapsed.get()
    }

    fn wait(&mut self, duration: Duration) {
        let elapsed = self.now() + duration;
        assert!(elapsed < Duration::from_secs(3600), "session did not finish in test time");
        self.elapsed.set(elapsed);
        // A real export thread still needs scheduling; its I/O is not simulated.
        std::thread::yield_now();
    }
}
