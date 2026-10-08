// SPDX-License-Identifier: BSD-3-Clause
//! Retained framebuffer owner shared by the session manager and hardware probe.
//! Backend operations are the same checked legacy-KMS operations used by IGUI.

use {
    imanlib::kms::output::DisplayMode,
    imanlib::kms::output::Topology,
    imanlib::kms::Backend,
    imanlib::surface::Surface,
};
use std::io;

/// A verified page flip may change only our CRTC's framebuffer in the route.
/// The caller must independently verify that the new framebuffer is its own.
pub fn verify_presented_route<M: DisplayMode>(
    before: &Topology<M>,
    after: &Topology<M>,
    crtc: u32,
) -> io::Result<()> {
    let framebuffer = after.crtcs.iter().find(|c| c.id == crtc)
        .and_then(|c| c.framebuffer)
        .filter(|id| *id != 0)
        .ok_or_else(|| io::Error::other("presented CRTC has no framebuffer"))?;
    let mut expected = before.clone();
    let selected = expected.crtcs.iter_mut().find(|c| c.id == crtc)
        .ok_or_else(|| io::Error::other("presented CRTC is not in the original route"))?;
    selected.framebuffer = Some(framebuffer);
    expected.verify_unchanged(after)
}

pub struct Owner<B: Backend> {
    backend: B,
    allow_vendor_einval: bool,
    master: bool,
    yielded: bool,
    buffer: bool,
    framebuffer: bool,
    modeset: bool,
    closed: bool,
}

impl<B: Backend> Owner<B> {
    pub fn start(backend: B, frame: &Surface, allow_vendor_einval: bool) -> io::Result<Self> {
        let mut display = Self {
            backend,
            allow_vendor_einval,
            master: false,
            yielded: false,
            buffer: false,
            framebuffer: false,
            modeset: false,
            closed: false,
        };
        display.acquire()?;
        display.backend.select_output()?;
        display.buffer = true;
        display.backend.create_buffer()?;
        display.framebuffer = true;
        display.backend.add_framebuffer()?;
        display.backend.write_frame(frame)?;
        display.modeset = true;
        display.backend.enable()?;
        display.backend.verify()?;
        Ok(display)
    }

    fn acquire(&mut self) -> io::Result<()> {
        match self.backend.acquire_master() {
            Ok(()) => {
                self.master = true;
                log(format_args!("DISPLAY SET_MASTER ok; acquired=true"));
                Ok(())
            }
            Err(e) if self.allow_vendor_einval && e.raw_os_error() == Some(libc::EINVAL) => {
                log(format_args!("DISPLAY SET_MASTER EINVAL; acquired=false; explicit vendor mode; NOT proof of exclusive ownership"));
                Ok(())
            }
            Err(e) => {
                log(format_args!("DISPLAY SET_MASTER failed: {e}"));
                Err(e)
            }
        }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn master_acquired(&self) -> bool {
        self.master
    }

    /// Guarded backend-specific diagnostics; never operate while a child owns it.
    pub fn with_backend<T>(
        &mut self,
        operation: impl FnOnce(&mut B) -> io::Result<T>,
    ) -> io::Result<T> {
        if self.closed || self.yielded {
            return Err(io::Error::other("display is closed or yielded"));
        }
        self.backend.verify()?;
        let result = operation(&mut self.backend)?;
        self.backend.verify()?;
        Ok(result)
    }

    pub fn present(&mut self, frame: &Surface) -> io::Result<()> {
        if self.closed || self.yielded {
            return Err(io::Error::other("display is closed or yielded"));
        }
        self.backend.verify()?;
        self.backend.write_frame(frame)
    }

    /// Keep the original FD, framebuffer and buffer alive. Never modeset here.
    pub fn yield_display(&mut self) -> io::Result<()> {
        if self.closed || self.yielded {
            return Err(io::Error::other("display is closed or already yielded"));
        }
        self.backend.verify()?;
        if self.master {
            self.backend.release_master()?;
            self.master = false;
            log(format_args!("DISPLAY DROP_MASTER ok"));
        } else {
            log(format_args!(
                "DISPLAY DROP_MASTER skipped: master was NOT acquired"
            ));
        }
        self.yielded = true;
        self.backend.verify()
    }

    /// Caller must reap the child first. Do not overwrite a missing/foreign FB:
    /// a failed restoration stops the session rather than guessing ownership.
    pub fn reclaim(&mut self) -> io::Result<()> {
        if self.closed || !self.yielded {
            return Err(io::Error::other("display was not yielded"));
        }
        self.yielded = false; // No implicit retry if acquisition fails.
        self.acquire()?;
        self.backend.verify()
    }

    /// Every release is attempted once, even after another cleanup step failed.
    pub fn finish(&mut self) -> io::Result<()> {
        if self.closed {
            return Ok(());
        }
        let mut errors = Vec::new();
        if self.yielded {
            if let Err(e) = self.reclaim() {
                errors.push(format!("reclaim: {e}"));
            }
        }
        self.closed = true;
        for (name, result) in [
            (
                "restore",
                if self.modeset {
                    self.backend.restore()
                } else {
                    Ok(())
                },
            ),
            (
                "remove-fb",
                if self.framebuffer {
                    self.backend.remove_framebuffer()
                } else {
                    Ok(())
                },
            ),
            (
                "destroy-buffer",
                if self.buffer {
                    self.backend.destroy_buffer()
                } else {
                    Ok(())
                },
            ),
            (
                "release-master",
                if self.master {
                    self.backend.release_master()
                } else {
                    Ok(())
                },
            ),
        ] {
            if let Err(e) = result {
                errors.push(format!("{name}: {e}"));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(io::Error::other(errors.join("; ")))
        }
    }
}

impl<B: Backend> Drop for Owner<B> {
    fn drop(&mut self) {
        if let Err(e) = self.finish() {
            log(format_args!("DISPLAY cleanup failed: {e}"));
        }
    }
}

pub fn log(message: std::fmt::Arguments<'_>) {
    use std::io::Write;
    // A full bounded RAM journal is an I/O error, not a panic on stderr.
    let _ = writeln!(io::stderr().lock(), "{message}");
}
