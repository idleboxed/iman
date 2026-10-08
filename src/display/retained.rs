// SPDX-License-Identifier: BSD-3-Clause
use std::{fs, io, path::Path};
#[cfg(feature = "diag-display")]
use std::time::{Duration, Instant};

use drm::control::Mode;
use {
    imanlib::kms::output::Topology,
    imanlib::kms::LinuxDrm,
    imanlib::surface::Surface,
};

use super::{owner::{verify_presented_route, Owner}, splash, Lease};
use crate::{profile::Display, session::diagnostic};

struct Retained {
    owner: Owner<LinuxDrm>,
    baseline: Topology<Mode>,
    monitor: u32,
    crtc: u32,
    starting: bool,
    error_frame: Option<(Surface, String, bool)>,
    diagnostic_screen: splash::DiagnosticScreen,
    #[cfg(feature = "diag-input")]
    input_screen: crate::input_test::render::Screen,
    #[cfg(feature = "diag-display")]
    measurements: Option<crate::diagnostics::display::Measurements>,
    #[cfg(feature = "diag-display")]
    cleanup_ok: Option<bool>,
}

impl Retained {
    fn present(&mut self, frame: &Surface) -> io::Result<()> {
        let result = (|| {
            self.baseline.verify_unchanged(&self.owner.backend().snapshot()?)?;
            #[cfg(feature = "diag-display")]
            let started = self.measurements.as_ref().map(|_| Instant::now());
            let presented = self.owner.present(frame);
            #[cfg(feature = "diag-display")]
            if let Some((m, started)) = self.measurements.as_mut().zip(started) {
                m.present(started.elapsed(), presented.is_ok());
            }
            presented?;
            let current = self.owner.with_backend(|backend| backend.snapshot())?;
            verify_presented_route(&self.baseline, &current, self.crtc)?;
            self.baseline = current;
            Ok(())
        })();
        #[cfg(feature = "diag-display")]
        if result.is_err() {
            if let Some(m) = &mut self.measurements {
                m.failed();
            }
        }
        result
    }

    fn frame(&mut self, message: &str, error: bool) -> Surface {
        self.diagnostic_screen = Default::default();
        #[cfg(feature = "diag-input")]
        { self.input_screen = Default::default(); }
        #[cfg(feature = "diag-display")]
        let started = self.measurements.as_ref().map(|_| Instant::now());
        let mut frame = self.error_frame.take().map(|(frame, _, _)| frame)
            .unwrap_or_else(|| Surface::new(splash::WIDTH, splash::HEIGHT));
        splash::draw(&mut frame, message, error).unwrap();
        #[cfg(feature = "diag-display")]
        if let Some((m, started)) = self.measurements.as_mut().zip(started) {
            m.render(started.elapsed(), false);
        }
        frame
    }
}

impl Lease for Retained {
    #[cfg(feature = "diag-display")]
    fn observe_display(&mut self, enabled: bool) {
        if enabled {
            self.measurements.get_or_insert_with(Default::default);
        } else {
            self.measurements = None;
        }
    }

    #[cfg(feature = "diag-display")]
    fn take_diagnostics(&mut self) -> Option<(Duration, imanlib::display_metrics::Sample)> {
        let kms = self.owner.backend().diagnostic_state(self.owner.master_acquired())?;
        let measurements = self.measurements.as_mut()?;
        Some(std::mem::take(measurements).finish(kms, self.cleanup_ok))
    }

    #[cfg(feature = "diag-display")]
    fn diagnostics_status(&self) -> Option<String> {
        let kms = self.owner.backend().diagnostic_state(self.owner.master_acquired())?;
        self.measurements.as_ref().map(|m| m.status(Some(&kms)))
    }

    fn starting_gui(&mut self) -> io::Result<()> {
        if !self.starting {
            let frame = self.frame(splash::STARTING, false);
            self.present(&frame)?;
            self.starting = true;
        }
        Ok(())
    }

    fn lend(&mut self) -> io::Result<()> {
        self.diagnostic_screen = Default::default();
        #[cfg(feature = "diag-input")]
        { self.input_screen = Default::default(); }
        self.baseline
            .verify_unchanged(&self.owner.backend().snapshot()?)?;
        self.owner.with_backend(|backend| backend.discard_hidden())?;
        self.baseline
            .verify_unchanged(&self.owner.backend().snapshot()?)?;
        self.owner.yield_display()?;
        diagnostic(format_args!(
            "iman: display lent; manager framebuffer retained"
        ));
        Ok(())
    }

    fn reclaim(&mut self) -> io::Result<()> {
        self.owner.reclaim()?;
        // Check the entire route, not just the CRTC framebuffer ID.
        self.baseline
            .verify_unchanged(&self.owner.backend().snapshot()?)?;
        diagnostic(format_args!(
            "iman: child reaped; manager display restored and verified"
        ));
        Ok(())
    }

    fn finish(&mut self) -> io::Result<()> {
        let result = self.owner.finish();
        #[cfg(feature = "diag-display")]
        {
            self.cleanup_ok = Some(result.is_ok());
        }
        diagnostic(format_args!("iman: display final restore: {result:?}"));
        result
    }

    fn export_status(&mut self, message: &str, failed: bool) -> io::Result<bool> {
        self.error_frame = None;
        let frame = self.frame(message, failed);
        self.present(&frame)?;
        self.starting = false;
        Ok(true)
    }

    #[cfg(feature = "diag-input")]
    fn input_test_frame(&mut self, view: &crate::input_test::View) -> io::Result<bool> {
        self.diagnostic_screen = Default::default();
        if self.error_frame.is_none() { self.input_screen = Default::default(); }
        #[cfg(feature = "diag-display")]
        let started = self.measurements.as_ref().map(|_| Instant::now());
        let mut frame = self.error_frame.take().map(|(frame, _, _)| frame)
            .unwrap_or_else(|| Surface::new(splash::WIDTH, splash::HEIGHT));
        let redraw = self.input_screen.update(&mut frame, view).unwrap();
        if redraw != super::Redraw::Unchanged {
            #[cfg(feature = "diag-display")]
            if let Some((m, started)) = self.measurements.as_mut().zip(started) {
                if redraw == super::Redraw::Partial { m.render_partial(started.elapsed()); }
                else { m.render(started.elapsed(), false); }
            }
            self.present(&frame)?;
        }
        self.error_frame = Some((frame, "Controller test complete".into(), false));
        Ok(true)
    }

    fn departure(&mut self, message: &str) -> io::Result<bool> {
        let frame = self.frame(message, false);
        self.present(&frame)?;
        self.error_frame = Some((frame, message.into(), false));
        Ok(true)
    }

    fn report_saved(&mut self, path: &Path) -> io::Result<bool> {
        self.diagnostic_screen = Default::default();
        #[cfg(feature = "diag-input")]
        { self.input_screen = Default::default(); }
        self.error_frame = None;
        let mut frame = Surface::new(splash::WIDTH, splash::HEIGHT);
        splash::draw_report_path(&mut frame, &path.to_string_lossy()).unwrap();
        self.present(&frame)?;
        Ok(true)
    }

    fn monitor_index(&self) -> Option<u32> {
        Some(self.monitor)
    }

    fn gui_error(&mut self, path: &Path, error: &io::Error) -> io::Result<bool> {
        let message = super::gui_error_message(path, error);
        diagnostic(format_args!("iman: {message}"));
        let frame = self.frame(&message, true);
        self.present(&frame)?;
        self.error_frame = Some((frame, message, true));
        Ok(true)
    }

    fn diagnostic_status(&mut self, message: &str, elapsed_seconds: u64) -> io::Result<()> {
        #[cfg(feature = "diag-input")]
        { self.input_screen = Default::default(); }
        let (mut frame, error, failed) = self.error_frame.take()
            .ok_or_else(|| io::Error::other("no error screen to update"))?;
        #[cfg(feature = "diag-display")]
        let started = self.measurements.as_ref().map(|_| Instant::now());
        let redraw = self.diagnostic_screen.update(&mut frame, message, elapsed_seconds).unwrap();
        if redraw == splash::Redraw::Unchanged {
            self.error_frame = Some((frame, error, failed));
            return Ok(());
        }
        #[cfg(feature = "diag-display")]
        if let Some((m, started)) = self.measurements.as_mut().zip(started) {
            if redraw == splash::Redraw::Partial { m.render_partial(started.elapsed()); }
            else { m.render(started.elapsed(), false); }
        }
        self.present(&frame)?;
        self.error_frame = Some((frame, error, failed));
        Ok(())
    }

    fn error_tick(&mut self, remaining: u8) -> io::Result<()> {
        self.diagnostic_screen = Default::default();
        #[cfg(feature = "diag-input")]
        { self.input_screen = Default::default(); }
        let (mut frame, error, failed) = self.error_frame.take()
            .ok_or_else(|| io::Error::other("no error screen to update"))?;
        #[cfg(feature = "diag-display")]
        let started = self.measurements.as_ref().map(|_| Instant::now());
        if remaining == 10 {
            // Restore the launch error after diagnostics, reusing the same surface.
            splash::draw(&mut frame, &error, failed).unwrap();
        }
        splash::countdown(&mut frame, remaining).unwrap();
        #[cfg(feature = "diag-display")]
        if let Some((m, started)) = self.measurements.as_mut().zip(started) {
            m.render(started.elapsed(), remaining != 10);
        }
        self.present(&frame)?;
        self.error_frame = Some((frame, error, failed));
        Ok(())
    }
}

pub(super) fn open(
    display: &Display,
    video_context: Option<Option<&str>>,
    _observe: bool,
) -> io::Result<Box<dyn Lease>> {
    let Display::Drm {
        device,
        connector,
        allow_vendor_einval,
    } = display
    else {
        unreachable!("only explicit DRM profiles enter this adapter")
    };
    if let Some(video_context) = video_context {
        if video_context != Some("kms") {
            return Err(io::Error::other(
                "DRM session requires the RetroArch kms context",
            ));
        }
        // Upstream KMS scans GPUs and has no explicit DRM-node setting. Do not
        // silently run the child on a different GPU than its retained parent.
        let mut cards = Vec::new();
        for entry in fs::read_dir("/dev/dri")? {
            let entry = entry?;
            let name = entry.file_name();
            if name.to_str().is_some_and(|n| {
                n.strip_prefix("card")
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            }) {
                cards.push(entry.path());
            }
        }
        if cards.len() != 1 || cards[0] != *device || device.parent() != Some(Path::new("/dev/dri"))
        {
            return Err(io::Error::other("upstream RetroArch KMS requires a single matching /dev/dri/cardN for managed handoff"));
        }
    }
    let format = super::framebuffer_format();
    diagnostic(format_args!("iman: retained framebuffer format={format:?}"));
    let backend = LinuxDrm::open_with_format(device, *connector, format)?;
    // Only the kernel framebuffers survive the GUI. Drop the CPU-side image
    // before launching children: no catalogue, renderer or artwork stays resident.
    #[cfg(feature = "diag-display")]
    let mut measurements = _observe.then(crate::diagnostics::display::Measurements::default);
    #[cfg(feature = "diag-display")]
    let started = measurements.as_ref().map(|_| Instant::now());
    let frame = splash::frame(splash::INITIALIZING, false);
    #[cfg(feature = "diag-display")]
    if let Some((m, started)) = measurements.as_mut().zip(started) {
        m.render(started.elapsed(), false);
    }
    let owner = Owner::start(backend, &frame, *allow_vendor_einval)?;
    drop(frame);
    let baseline = owner.backend().snapshot()?;
    let output = baseline.select_connector(*connector)?;
    let monitor = baseline
        .connectors
        .iter()
        .filter(|c| c.connected && !c.modes.is_empty())
        .position(|c| c.id == output.connector)
        .ok_or_else(|| io::Error::other("manager connector has no RetroArch monitor index"))?
        as u32
        + 1;
    diagnostic(format_args!("iman: retained display connector={} encoder={} crtc={} fb={:?} monitor={monitor}; master_acquired={}",
        output.connector, output.encoder, output.previous.id, output.previous.framebuffer, owner.master_acquired()));
    Ok(Box::new(Retained {
        owner,
        baseline,
        monitor,
        crtc: output.previous.id,
        starting: false,
        error_frame: None,
        diagnostic_screen: Default::default(),
        #[cfg(feature = "diag-input")]
        input_screen: Default::default(),
        #[cfg(feature = "diag-display")]
        measurements,
        #[cfg(feature = "diag-display")]
        cleanup_ok: None,
    }))
}
