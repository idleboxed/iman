// SPDX-License-Identifier: BSD-3-Clause
//! Display ownership belongs to the manager, not to a short-lived frontend.
use std::{io, path::Path, time::Duration};

#[cfg(feature = "drm")]
pub mod owner;
#[cfg(feature = "drm")]
mod retained;
#[cfg(feature = "pc")]
pub mod pc;
#[cfg(any(feature = "drm", feature = "pc"))]
pub mod splash;

/// Result of updating a retained CPU frame, independent of the output backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Redraw { Unchanged, Full, Partial }

/// Both applications default to RGB565; each can select a format at build time.
#[cfg(feature = "drm")]
pub fn framebuffer_format() -> imanlib::kms::format::Format {
    use imanlib::kms::format::Format;
    #[cfg(feature = "rgb565")]
    {
        Format::Rgb565
    }
    #[cfg(not(feature = "rgb565"))]
    {
        Format::Xrgb8888
    }
}

/// The session only lends after the previous child has exited and been reaped.
/// A failed reclaim is fatal: never launch another frontend on an unknown route.
pub trait Lease {
    /// PC window events while the manager owns the screen; absent in board builds.
    #[cfg(feature = "pc")]
    fn poll_events(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn observe_display(&mut self, _enabled: bool) {}
    fn take_diagnostics(&mut self) -> Option<(Duration, imanlib::display_metrics::Sample)> {
        None
    }
    fn diagnostics_status(&self) -> Option<String> {
        None
    }
    fn starting_gui(&mut self) -> io::Result<()> {
        Ok(())
    }
    #[cfg(feature = "diag-input")]
    fn input_test_frame(&mut self, _view: &crate::input_test::View) -> io::Result<bool> {
        Ok(false)
    }
    /// Prepare a normal departure, using the same countdown as launch errors.
    fn departure(&mut self, _message: &str) -> io::Result<bool> {
        Ok(false)
    }
    fn report_saved(&mut self, path: &Path) -> io::Result<bool> {
        self.export_status(&format!("Report saved: {}", path.display()), false)
    }
    fn lend(&mut self) -> io::Result<()>;
    fn reclaim(&mut self) -> io::Result<()>;
    fn finish(&mut self) -> io::Result<()>;
    /// Called only after the failed GUI has been reaped and ownership verified.
    /// True permits the departure countdown after any requested diagnostics.
    fn gui_error(&mut self, _path: &Path, _error: &io::Error) -> io::Result<bool> {
        Ok(false)
    }
    /// Called only without a GUI child, after ownership has been verified.
    fn diagnostic_status(&mut self, _message: &str, _elapsed_seconds: u64) -> io::Result<()> {
        Ok(())
    }
    /// Zero displays the farewell for one second before final restoration.
    fn error_tick(&mut self, _remaining: u8) -> io::Result<()> {
        Ok(())
    }
    /// Return true only when a manager-owned status is actually presented.
    fn export_status(&mut self, _message: &str, _failed: bool) -> io::Result<bool> {
        Ok(false)
    }
    fn monitor_index(&self) -> Option<u32> {
        None
    }
}

pub fn gui_error_message(path: &Path, error: &io::Error) -> String {
    // Neither a filename nor a child error may inject display/terminal controls.
    format!("IGUI failed: {}. Executable: {}",
        error.to_string().escape_debug(), path.to_string_lossy().escape_debug())
}

/// Ten seconds of error display, then one second for the farewell.
pub fn leaving_seconds(elapsed: Duration) -> Option<u8> {
    match elapsed.as_secs() {
        seconds @ 0..=10 => Some(10 - seconds as u8),
        _ => None,
    }
}

/// Headless and desktop without pc leave output entirely to the child.
#[derive(Default)]
pub struct Unmanaged;

impl Lease for Unmanaged {
    fn lend(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn reclaim(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn finish(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Desktop opens a window only with pc; only explicit DRM profiles acquire a device.
pub fn open(
    display: &crate::profile::Display,
    video_context: Option<Option<&str>>,
) -> io::Result<Box<dyn Lease>> {
    open_observed(display, video_context, false)
}

/// Measurement follows the compiled collector and the current display owner phase.
pub fn open_observed(
    display: &crate::profile::Display,
    video_context: Option<Option<&str>>,
    observe: bool,
) -> io::Result<Box<dyn Lease>> {
    match display {
        crate::profile::Display::Headless => {
            Ok(Box::new(Unmanaged))
        }
        crate::profile::Display::Desktop => {
            #[cfg(feature = "pc")]
            { Ok(Box::new(pc::WindowDisplay::open(pc::X11::default(), observe)?)) }
            #[cfg(not(feature = "pc"))]
            { Ok(Box::new(Unmanaged)) }
        }
        crate::profile::Display::Drm { .. } => {
            #[cfg(feature = "drm")]
            {
                retained::open(display, video_context, observe)
            }
            #[cfg(not(feature = "drm"))]
            {
                let _ = (video_context, observe);
                Err(io::Error::other(
                    "DRM session requires the iman drm build feature",
                ))
            }
        }
    }
}
