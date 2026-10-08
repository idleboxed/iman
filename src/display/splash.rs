// SPDX-License-Identifier: BSD-3-Clause
//! Shared manager frames. No external artwork, locale files, timers or device access.
use embedded_graphics::{
    mono_font::{iso_8859_5::{FONT_10X20, FONT_7X13}, MonoTextStyle},
    pixelcolor::Rgb888,
    prelude::*,
    primitives::Rectangle,
    text::{Baseline, Text},
};
use imanlib::surface::Surface;

pub const INITIALIZING: &str = "Initializing ...";
pub const STARTING: &str = "...";
pub const BUILD_LABEL: &str = concat!(env!("CARGO_PKG_VERSION"), "-", env!("IMAN_BUILD_UTC"));
pub const WIDTH: u32 = imanlib::surface::WIDTH;
pub const HEIGHT: u32 = imanlib::surface::HEIGHT;

// Selected variant 1: connected wrist and a softened bottom, 32 x 40 pixels.
// 160 read-only bytes. Zero bits are transparent; no decoder or image allocation.
static FAREWELL: [u32; 40] = [
    0b00000000000000000000000000000000,
    0b00000000000000100000000000000000,
    0b00000000000001110000000000000000,
    0b00000000000011111000000000000000,
    0b00000000000011111100000000000000,
    0b00000000000111111100100000000000,
    0b00000000001111111001110000000000,
    0b00000000010001110001111000000000,
    0b00000000011000010001110000000000,
    0b00000000111110000011110011000000,
    0b00000001111111100011100111100000,
    0b00000001111111111111100111100000,
    0b00000011111111111111001111000000,
    0b00000111111111111111001111001100,
    0b00001111111111111110011110011110,
    0b00001111111001100110111100111100,
    0b00011111111100000000111001111000,
    0b00011111111111010000010011110000,
    0b00000111111111101111000011100000,
    0b00000011111111110111111000111000,
    0b00000001111111110111111011111000,
    0b00000000111111111011111011110000,
    0b00000000011111111011111011110000,
    0b00000000001111111101111011100000,
    0b00000000000111111100111011100000,
    0b00000000000111111110111011000000,
    0b00000000000111111110011111000000,
    0b00000000000111111111111110000000,
    0b00000000000111111111111110000000,
    0b00000000000111111111111100000000,
    0b00000000000111111111111100000000,
    0b00000000000111111111111100000000,
    0b00000000000111111111111100000000,
    0b00000000000111111111111100000000,
    0b00000000000111111111111100000000,
    0b00000000000111111111111100000000,
    0b00000000000111111111111000000000,
    0b00000000000011111111111000000000,
    0b00000000000001111111110000000000,
    0b00000000000000000000000000000000,
];

/// Render one frame; the caller can drop the CPU surface after uploading it.
pub fn frame(message: &str, error: bool) -> Surface {
    let mut surface = Surface::new(WIDTH, HEIGHT);
    draw(&mut surface, message, error).unwrap(); // Surface is infallible.
    surface
}

/// A receipt, not a progress promise. Caller supplies the path returned by syncfs.
pub fn draw_report_path<D: DrawTarget<Color = Rgb888>>(
    target: &mut D,
    path: &str,
) -> Result<(), D::Error> {
    let bounds = target.bounding_box();
    let center = bounds.center();
    target.clear(Rgb888::new(15, 21, 29))?;
    let columns = (bounds.size.width.saturating_sub(64) / 10).max(1) as usize;
    let path = path.escape_debug().to_string();
    let chars: Vec<_> = path.chars().collect();
    let lines: Vec<String> = chars.chunks(columns).map(|line| line.iter().collect()).collect();
    let top = center.y - (lines.len() as i32 * 26 + 46) / 2;
    let mut line = |value: &str, y: i32, color| {
        Text::with_baseline(value, Point::new(center.x - value.chars().count() as i32 * 5, y),
            MonoTextStyle::new(&FONT_10X20, color), Baseline::Top).draw(target).map(|_| ())
    };
    line("Report saved", top, Rgb888::new(119, 207, 180))?;
    for (index, value) in lines.iter().enumerate() {
        line(value, top + 46 + index as i32 * 26, Rgb888::new(230, 234, 231))?;
    }
    Text::with_baseline(BUILD_LABEL, Point::new(32, bounds.size.height as i32 - 40),
        MonoTextStyle::new(&FONT_7X13, Rgb888::new(151, 169, 174)), Baseline::Top).draw(target)?;
    Ok(())
}

/// Error severity changes only the message colour, not the bitmap font.
pub fn draw<D: DrawTarget<Color = Rgb888>>(
    target: &mut D,
    message: &str,
    error: bool,
) -> Result<(), D::Error> {
    draw_with_title(target, message, error, Rgb888::new(119, 207, 180))
}

/// Diagnostic heartbeat shares the status update, never requests another frame.
pub fn draw_diagnostics<D: DrawTarget<Color = Rgb888>>(
    target: &mut D,
    message: &str,
    elapsed_seconds: u64,
) -> Result<(), D::Error> {
    draw_with_title(target, message, false, diagnostic_color(elapsed_seconds))
}

fn diagnostic_color(seconds: u64) -> Rgb888 {
    [Rgb888::new(119, 207, 180), Rgb888::new(126, 183, 255),
        Rgb888::new(239, 197, 105), Rgb888::new(196, 157, 233)][(seconds % 4) as usize]
}

const BACKGROUND: Rgb888 = Rgb888::new(15, 21, 29);
const MUTED: Rgb888 = Rgb888::new(151, 169, 174);

pub use super::Redraw;

struct DiagnosticState {
    bounds: Rectangle,
    color: Rgb888,
    lines: Vec<String>,
}

/// Reuse only with the same retained target. Reset when another screen overwrites
/// its pixels; no second full-size surface or background copy is allocated.
#[derive(Default)]
pub struct DiagnosticScreen {
    previous: Option<DiagnosticState>,
}
impl DiagnosticScreen {
    pub fn update<D: DrawTarget<Color = Rgb888>>(
        &mut self, target: &mut D, message: &str, seconds: u64,
    ) -> Result<Redraw, D::Error> {
        let bounds = target.bounding_box();
        let next = DiagnosticState {
            bounds, color: diagnostic_color(seconds), lines: message_lines(bounds, message),
        };
        // On any draw failure, a subsequent call must rebuild the whole frame.
        let previous = self.previous.take();
        let full = previous.as_ref().is_none_or(|old| old.bounds != bounds)
            || bounds.size.width < 224 || bounds.size.height < 360;
        let mut redraw = Redraw::Unchanged;
        if full {
            draw_with_title(target, message, false, next.color)?;
            redraw = Redraw::Full;
        } else if let Some(old) = previous {
            if old.color != next.color {
                draw_title(target, bounds, next.color)?;
                redraw = Redraw::Partial;
            }
            for row in 0..old.lines.len().max(next.lines.len()) {
                if old.lines.get(row) == next.lines.get(row) { continue; }
                let y = bounds.center().y + 64 + row as i32 * 17;
                target.fill_solid(&Rectangle::new(Point::new(bounds.top_left.x, y),
                    Size::new(bounds.size.width, 13)), BACKGROUND)?;
                if let Some(line) = next.lines.get(row) { draw_line(target, bounds, line, y, MUTED)?; }
                redraw = Redraw::Partial;
            }
        }
        self.previous = Some(next);
        Ok(redraw)
    }
}

fn draw_title<D: DrawTarget<Color = Rgb888>>(
    target: &mut D, bounds: Rectangle, color: Rgb888,
) -> Result<(), D::Error> {
    let mut logo = Surface::new(40, 20);
    Text::with_baseline("IMAN", Point::zero(), MonoTextStyle::new(&FONT_10X20, Rgb888::WHITE),
        Baseline::Top).draw(&mut logo).unwrap();
    let origin = bounds.center() - Point::new(80, 40);
    for (i, &pixel) in logo.pixels().iter().enumerate() {
        if pixel != 0 {
            target.fill_solid(&Rectangle::new(origin + Point::new((i % 40) as i32 * 4, (i / 40) as i32 * 4),
                Size::new(4, 4)), color)?;
        }
    }
    Ok(())
}

fn draw_line<D: DrawTarget<Color = Rgb888>>(
    target: &mut D, bounds: Rectangle, value: &str, y: i32, color: Rgb888,
) -> Result<(), D::Error> {
    Text::with_baseline(value, Point::new(bounds.center().x - value.chars().count() as i32 * 7 / 2, y),
        MonoTextStyle::new(&FONT_7X13, color), Baseline::Top).draw(target)?;
    Ok(())
}

fn message_lines(bounds: Rectangle, message: &str) -> Vec<String> {
    // Shared wrapping keeps incremental and reference rendering pixel-identical.
    let columns = (bounds.size.width.saturating_sub(64) / 7).max(4) as usize;
    let first_y = bounds.center().y + 64;
    let bottom = bounds.top_left.y + bounds.size.height as i32 - 64;
    let rows = ((bottom - first_y) / 17).max(1) as usize;
    let mut lines = Vec::new();
    let mut chars = message.chars().peekable();
    for row in 0..rows {
        if chars.peek().is_none() { break; }
        let mut value = String::new();
        for _ in 0..columns {
            match chars.next() {
                Some('\n') | None => break,
                Some(c) => value.push(c),
            }
        }
        if value.chars().count() == columns && chars.peek() == Some(&'\n') { chars.next(); }
        if row + 1 == rows && chars.peek().is_some() {
            for _ in 0..3 { value.pop(); }
            value.push_str("...");
        }
        lines.push(value);
    }
    lines
}

fn draw_with_title<D: DrawTarget<Color = Rgb888>>(
    target: &mut D, message: &str, error: bool, title_color: Rgb888,
) -> Result<(), D::Error> {
    let bounds = target.bounding_box();
    target.clear(BACKGROUND)?;
    draw_title(target, bounds, title_color)?;
    let color = if error { Rgb888::new(240, 91, 91) } else { MUTED };
    for (row, line) in message_lines(bounds, message).iter().enumerate() {
        draw_line(target, bounds, line, bounds.center().y + 64 + row as i32 * 17, color)?;
    }
    Text::with_baseline(BUILD_LABEL, bounds.top_left + Point::new(32, bounds.size.height as i32 - 40),
        MonoTextStyle::new(&FONT_7X13, MUTED), Baseline::Top).draw(target)?;
    Ok(())
}

/// Update only the small CPU-side footer; KMS still uploads a complete hidden
/// frame and flips it safely. No per-frame timer, font cache or persistent write.
pub fn countdown<D: DrawTarget<Color = Rgb888>>(
    target: &mut D,
    remaining: u8,
) -> Result<(), D::Error> {
    let bounds = target.bounding_box();
    let right = bounds.top_left.x + bounds.size.width as i32 - 32;
    let y = bounds.top_left.y + bounds.size.height as i32 - 40;
    let bottom = y + 13;
    let icon_height = FAREWELL.len() as i32;
    target.fill_solid(
        &Rectangle::new(Point::new(right - 126, bottom - icon_height), Size::new(126, icon_height as u32)),
        Rgb888::new(15, 21, 29),
    )?;
    if remaining == 0 {
        let origin = Point::new(right - 32, bottom - icon_height);
        return target.draw_iter(FAREWELL.iter().enumerate().flat_map(|(row, bits)| {
            (0..32).filter_map(move |x| {
                (bits & (1_u32 << (31 - x)) != 0).then_some(Pixel(
                    origin + Point::new(x, row as i32), Rgb888::new(220, 0, 0),
                ))
            })
        }));
    }
    let label = format!("Leaving in {remaining} ...");
    Text::with_baseline(
        &label,
        Point::new(right - label.len() as i32 * 7, y),
        MonoTextStyle::new(&FONT_7X13, Rgb888::new(151, 169, 174)),
        Baseline::Top,
    )
    .draw(target)?;
    Ok(())
}
