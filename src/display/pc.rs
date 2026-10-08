// SPDX-License-Identifier: BSD-3-Clause
//! Optional PC presentation only; session policy and drawing stay shared with DRM.
use std::{io, path::Path};
#[cfg(feature = "diag-display")]
use std::time::{Duration, Instant};

use imanlib::surface::Surface;
use super::{gui_error_message, splash, Lease};

/// Window operations are replaceable in offline lifecycle tests.
pub trait WindowBackend {
    fn open(&mut self, width: usize, height: usize) -> io::Result<()>;
    fn present(&mut self, pixels: &[u32], width: usize, height: usize) -> io::Result<()>;
    fn poll_events(&mut self) -> io::Result<()>;
    fn close(&mut self);
}

#[derive(Default)]
pub struct X11 {
    window: Option<minifb::Window>,
}

impl WindowBackend for X11 {
    fn open(&mut self, width: usize, height: usize) -> io::Result<()> {
        let mut window = minifb::Window::new(
            "IMAN - Esc to close", width, height, minifb::WindowOptions::default(),
        ).map_err(io::Error::other)?;
        // The shared session sleeps between events. Don't include an FPS limiter
        // in measured window submission time.
        window.set_target_fps(0);
        self.window = Some(window);
        Ok(())
    }

    fn present(&mut self, pixels: &[u32], width: usize, height: usize) -> io::Result<()> {
        self.window.as_mut().ok_or_else(|| io::Error::other("PC window is closed"))?
            .update_with_buffer(pixels, width, height).map_err(io::Error::other)
    }

    fn poll_events(&mut self) -> io::Result<()> {
        if let Some(window) = &mut self.window {
            window.update();
            if !window.is_open() || window.is_key_down(minifb::Key::Escape) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "PC window closed"));
            }
        }
        Ok(())
    }

    fn close(&mut self) {
        self.window = None;
    }
}

pub struct WindowDisplay<B: WindowBackend> {
    backend: B,
    frame: Option<Surface>,
    error: Option<(String, bool)>,
    diagnostic_screen: splash::DiagnosticScreen,
    #[cfg(feature = "diag-input")]
    input_screen: crate::input_test::render::Screen,
    #[cfg(feature = "diag-display")]
    measurements: Option<crate::diagnostics::display::Measurements>,
    #[cfg(feature = "diag-display")]
    cleanup_ok: Option<bool>,
}

impl<B: WindowBackend> WindowDisplay<B> {
    pub fn open(mut backend: B, observe: bool) -> io::Result<Self> {
        backend.open(splash::WIDTH as usize, splash::HEIGHT as usize)?;
        let mut display = Self {
            backend, frame: None, error: None, diagnostic_screen: Default::default(),
            #[cfg(feature = "diag-input")]
            input_screen: Default::default(),
            #[cfg(feature = "diag-display")]
            measurements: None,
            #[cfg(feature = "diag-display")]
            cleanup_ok: None,
        };
        display.observe_display(observe);
        display.draw(|frame| splash::draw(frame, splash::INITIALIZING, false).unwrap(), false)?;
        Ok(display)
    }

    fn draw(&mut self, draw: impl FnOnce(&mut Surface), footer: bool) -> io::Result<()> {
        self.diagnostic_screen = Default::default();
        #[cfg(feature = "diag-input")]
        { self.input_screen = Default::default(); }
        self.draw_update(|frame| { draw(frame); splash::Redraw::Full }, footer)
    }

    fn draw_update(&mut self, draw: impl FnOnce(&mut Surface) -> splash::Redraw, _footer: bool) -> io::Result<()> {
        #[cfg(feature = "diag-display")]
        let began = self.measurements.as_ref().map(|_| Instant::now());
        let frame = self.frame.get_or_insert_with(|| Surface::new(splash::WIDTH, splash::HEIGHT));
        let redraw = draw(frame);
        if redraw == splash::Redraw::Unchanged { return Ok(()); }
        #[cfg(feature = "diag-display")]
        if let Some((m, began)) = self.measurements.as_mut().zip(began) {
            if redraw == splash::Redraw::Partial { m.render_partial(began.elapsed()); }
            else { m.render(began.elapsed(), _footer); }
        }
        #[cfg(feature = "diag-display")]
        let began = self.measurements.as_ref().map(|_| Instant::now());
        let result = self.backend.present(frame.pixels(), frame.width() as usize, frame.height() as usize);
        #[cfg(feature = "diag-display")]
        if let Some((m, began)) = self.measurements.as_mut().zip(began) {
            m.present(began.elapsed(), result.is_ok());
        }
        result
    }
}

impl<B: WindowBackend> Lease for WindowDisplay<B> {
    fn poll_events(&mut self) -> io::Result<()> {
        self.backend.poll_events()
    }

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
        Some(std::mem::take(self.measurements.as_mut()?).finish_desktop(self.cleanup_ok))
    }

    #[cfg(feature = "diag-display")]
    fn diagnostics_status(&self) -> Option<String> {
        self.measurements.as_ref().map(|m| m.status(None))
    }

    fn starting_gui(&mut self) -> io::Result<()> {
        self.draw(|frame| splash::draw(frame, splash::STARTING, false).unwrap(), false)
    }

    fn lend(&mut self) -> io::Result<()> {
        // No second manager window, CPU frame or event pump during GUI/game.
        self.backend.close();
        self.frame = None;
        self.diagnostic_screen = Default::default();
        #[cfg(feature = "diag-input")]
        { self.input_screen = Default::default(); }
        self.error = None;
        Ok(())
    }

    fn reclaim(&mut self) -> io::Result<()> {
        self.backend.open(splash::WIDTH as usize, splash::HEIGHT as usize)
    }

    fn finish(&mut self) -> io::Result<()> {
        self.lend()?;
        #[cfg(feature = "diag-display")]
        { self.cleanup_ok = Some(true); }
        Ok(())
    }

    fn gui_error(&mut self, path: &Path, error: &io::Error) -> io::Result<bool> {
        let message = gui_error_message(path, error);
        self.draw(|frame| splash::draw(frame, &message, true).unwrap(), false)?;
        self.error = Some((message, true));
        Ok(true)
    }

    fn diagnostic_status(&mut self, message: &str, seconds: u64) -> io::Result<()> {
        #[cfg(feature = "diag-input")]
        { self.input_screen = Default::default(); }
        let mut screen = std::mem::take(&mut self.diagnostic_screen);
        let result = self.draw_update(|frame| screen.update(frame, message, seconds).unwrap(), false);
        self.diagnostic_screen = screen;
        result
    }

    fn export_status(&mut self, message: &str, failed: bool) -> io::Result<bool> {
        self.draw(|frame| splash::draw(frame, message, failed).unwrap(), false)?;
        Ok(true)
    }

    #[cfg(feature = "diag-input")]
    fn input_test_frame(&mut self, view: &crate::input_test::View) -> io::Result<bool> {
        self.diagnostic_screen = Default::default();
        let mut screen = std::mem::take(&mut self.input_screen);
        if self.frame.is_none() { screen = Default::default(); }
        let result = self.draw_update(|frame| screen.update(frame, view).unwrap(), false);
        if result.is_ok() { self.input_screen = screen; }
        result?;
        Ok(true)
    }

    fn departure(&mut self, message: &str) -> io::Result<bool> {
        self.draw(|frame| splash::draw(frame, message, false).unwrap(), false)?;
        self.error = Some((message.into(), false));
        Ok(true)
    }

    fn report_saved(&mut self, path: &Path) -> io::Result<bool> {
        self.draw(|frame| splash::draw_report_path(frame, &path.to_string_lossy()).unwrap(), false)?;
        Ok(true)
    }

    fn error_tick(&mut self, remaining: u8) -> io::Result<()> {
        let (error, failed) = self.error.clone().ok_or_else(|| io::Error::other("no error screen to update"))?;
        self.draw(|frame| {
            if remaining == 10 {
                splash::draw(frame, &error, failed).unwrap();
            }
            splash::countdown(frame, remaining).unwrap();
        }, remaining != 10)
    }
}

impl<B: WindowBackend> Drop for WindowDisplay<B> {
    fn drop(&mut self) {
        self.backend.close();
    }
}
