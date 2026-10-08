// SPDX-License-Identifier: BSD-3-Clause
#![cfg(any(feature = "drm", feature = "pc"))]
use embedded_graphics::{
    mono_font::{iso_8859_5::{FONT_10X20, FONT_7X13}, MonoTextStyle},
    pixelcolor::Rgb888,
    prelude::*,
    text::{Baseline, Text},
};
use imanlib::surface::Surface;
use iman::display::{gui_error_message, splash};
use std::path::Path;

const BACKGROUND: u32 = 0x0f151d;
const MUTED: u32 = 0x97a9ae;
const RED: u32 = 0xf05b5b;

fn caption(value: &str, y: i32) -> Surface {
    caption_font(value, y, &FONT_7X13)
}

fn caption_font(value: &str, y: i32, font: &embedded_graphics::mono_font::MonoFont<'_>) -> Surface {
    let mut surface = Surface::new(splash::WIDTH, splash::HEIGHT);
    surface.clear(Rgb888::new(15, 21, 29)).unwrap();
    Text::with_baseline(
        value,
        Point::new(639 - value.chars().count() as i32 * font.character_size.width as i32 / 2, y),
        MonoTextStyle::new(font, Rgb888::new(151, 169, 174)),
        Baseline::Top,
    )
    .draw(&mut surface)
    .unwrap();
    surface
}

#[test]
fn startup_has_centered_title_and_neutral_status_without_subtitle() {
    let frame = splash::frame(splash::STARTING, false);
    let status = caption("...", 423);
    let width = splash::WIDTH as usize;

    assert!(frame.pixels()[399 * width..423 * width].iter().all(|&p| p == BACKGROUND));
    assert_eq!(
        &frame.pixels()[423 * width..443 * width],
        &status.pixels()[423 * width..443 * width],
    );
    let title = caption_font("IMAN", 0, &FONT_10X20);
    // Check the exact glyphs and 4x nearest-neighbour scaling, not just colours.
    for y in 0..80 {
        for x in 0..160 {
            let source = title.pixels()[(y / 4) * width + 619 + x / 4];
            assert_eq!(
                frame.pixels()[(319 + y) * width + 559 + x],
                if source == BACKGROUND { BACKGROUND } else { 0x77cfb4 },
            );
        }
    }
    assert_eq!(frame.pixels(), splash::frame(splash::STARTING, false).pixels());
}

#[test]
fn errors_change_only_the_message_colour_not_its_font_or_header() {
    let message = gui_error_message(Path::new("/mnt/sdcard/system/igui/igui"), &std::io::Error::other("exit status: 9"));
    let normal = splash::frame(&message, false);
    let error = splash::frame(&message, true);
    let mut changed = 0;

    for (i, (&a, &b)) in normal.pixels().iter().zip(error.pixels()).enumerate() {
        if a != b {
            assert_eq!((a, b), (MUTED, RED));
            assert!((423..436).contains(&(i / splash::WIDTH as usize)));
            changed += 1;
        }
    }
    assert!(changed > 0);
    assert_eq!(message, "IGUI failed: exit status: 9. Executable: /mnt/sdcard/system/igui/igui");
}

#[test]
fn long_paths_wrap_without_losing_spaces_or_unicode_characters() {
    let message = gui_error_message(Path::new(&format!("/{} /Игры/system/igui/igui", "ab".repeat(70))), &std::io::Error::other("exit status: 9"));
    let frame = splash::frame(&message, false);
    let chars: Vec<_> = message.chars().collect();
    let width = splash::WIDTH as usize;

    for (row, chunk) in chars.chunks(173).enumerate() {
        let y = 423 + row * 17;
        let expected = caption(&chunk.iter().collect::<String>(), y as i32);
        assert_eq!(
            &frame.pixels()[y * width..(y + 13) * width],
            &expected.pixels()[y * width..(y + 13) * width],
        );
    }
}

#[test]
fn overlong_messages_have_a_visible_ellipsis_instead_of_clipped_text() {
    let frame = splash::frame(&"a".repeat(5000), false);
    let expected = caption(&format!("{}...", "a".repeat(170)), 627);
    let width = splash::WIDTH as usize;

    assert_eq!(
        &frame.pixels()[627 * width..640 * width],
        &expected.pixels()[627 * width..640 * width],
    );
    assert!(frame.pixels()[640 * width..680 * width].iter().all(|&p| p == BACKGROUND));
}

#[test]
fn path_controls_are_escaped_before_display() {
    let message = gui_error_message(Path::new("/SD\n\u{1b}/igui"), &std::io::Error::other("failure\n\u{1b}"));

    assert!(!message.contains('\n'));
    assert!(!message.contains('\u{1b}'));
    assert!(message.contains("/SD\\n\\u{1b}/igui"));
}

#[test]
fn initialization_precedes_starting_with_only_the_caption_changed() {
    let initializing = splash::frame(splash::INITIALIZING, false);
    let starting = splash::frame(splash::STARTING, false);
    let expected = caption("Initializing ...", 423);
    let width = splash::WIDTH as usize;

    assert_eq!(&initializing.pixels()[..423 * width], &starting.pixels()[..423 * width]);
    assert_eq!(&initializing.pixels()[423 * width..443 * width], &expected.pixels()[423 * width..443 * width]);
    assert_eq!(&initializing.pixels()[443 * width..], &starting.pixels()[443 * width..]);
    assert_ne!(initializing.pixels(), starting.pixels());
}

#[test]
fn every_screen_has_the_small_version_and_utc_build_stamp_at_bottom_left() {
    let timestamp = env!("IMAN_BUILD_UTC");
    assert_eq!(timestamp.len(), 12);
    assert!(timestamp.bytes().all(|byte| byte.is_ascii_digit()));
    let label = format!("{}-{timestamp}", env!("CARGO_PKG_VERSION"));
    assert_eq!(splash::BUILD_LABEL, label);
    let mut expected = Surface::new(splash::WIDTH, splash::HEIGHT);
    expected.clear(Rgb888::new(15, 21, 29)).unwrap();
    Text::with_baseline(
        &label,
        Point::new(32, 680),
        MonoTextStyle::new(&FONT_7X13, Rgb888::new(151, 169, 174)),
        Baseline::Top,
    ).draw(&mut expected).unwrap();

    let width = splash::WIDTH as usize;
    for message in [splash::INITIALIZING, splash::STARTING, "Unable to start IGUI"] {
        for error in [false, true] {
            let frame = splash::frame(message, error);
            assert_eq!(&frame.pixels()[656 * width..], &expected.pixels()[656 * width..]);
        }
    }
}

#[test]
fn diagnostic_status_uses_separate_lines_without_a_departure_countdown() {
    let lines = [
        "IGUI unavailable. Diagnostics 3/10 s",
        "I/O: 4 samples; written mmcblk0 0 B, mmcblk0p5 0 B",
        "RAM: 2 samples, 0 read errors",
        "Kernel log: 1 samples, 1 read errors",
        "Display (iman): 3 renders, 3 presents, 5 completed flips",
    ];
    let frame = splash::frame(&lines.join("\n"), false);
    let width = splash::WIDTH as usize;

    for (index, line) in lines.iter().enumerate() {
        let y = 423 + index * 17;
        let expected = caption(line, y as i32);
        assert_eq!(&frame.pixels()[y * width..(y + 13) * width], &expected.pixels()[y * width..(y + 13) * width]);
    }
    for y in 653..693 {
        assert!(frame.pixels()[y * width + 1122..y * width + 1248].iter().all(|&p| p == BACKGROUND));
    }
}

#[test]
fn diagnostic_heartbeat_changes_only_title_ink_and_normal_screens_restore_its_color() {
    let normal = splash::frame("Diagnostics", false);
    let mut frame = splash::frame("Diagnostics", false);
    let colors = [0x77cfb4, 0x7eb7ff, 0xefc569, 0xc49de9];

    for second in 0..10 {
        splash::draw_diagnostics(&mut frame, "Diagnostics", second).unwrap();
        for (&pixel, &original) in frame.pixels().iter().zip(normal.pixels()) {
            assert_eq!(pixel, if original == 0x77cfb4 { colors[(second % 4) as usize] } else { original });
        }
    }
    splash::draw(&mut frame, "Diagnostics", false).unwrap();
    assert_eq!(frame.pixels(), normal.pixels());
}

#[test]
fn countdown_changes_only_the_bottom_right_footer_and_erases_old_digits() {
    let mut frame = splash::frame("Unable to start IGUI. Check /mnt/sdcard/system/igui/igui", true);
    let original = frame.pixels().to_vec();
    for seconds in [0, 255].into_iter().chain((1..=10).rev()) {
        splash::countdown(&mut frame, seconds).unwrap();
        if seconds == 0 {
            continue; // Also check that a subsequent countdown erases the entire icon.
        }
        let label = format!("Leaving in {seconds} ...");
        let mut expected = Surface::new(splash::WIDTH, splash::HEIGHT);
        expected.clear(Rgb888::new(15, 21, 29)).unwrap();
        Text::with_baseline(
            &label,
            Point::new(1248 - label.len() as i32 * 7, 680),
            MonoTextStyle::new(&FONT_7X13, Rgb888::new(151, 169, 174)),
            Baseline::Top,
        ).draw(&mut expected).unwrap();

        for (i, &pixel) in frame.pixels().iter().enumerate() {
            let (x, y) = (i % splash::WIDTH as usize, i / splash::WIDTH as usize);
            assert_eq!(pixel, if (1122..1248).contains(&x) && (653..693).contains(&y) {
                expected.pixels()[i]
            } else {
                original[i]
            });
        }
    }
}

#[test]
fn farewell_replaces_text_with_a_small_red_fist_without_a_backplate() {
    let original = splash::frame("Missing IGUI", true);
    let mut frame = splash::frame("Missing IGUI", true);
    splash::countdown(&mut frame, 1).unwrap();

    splash::countdown(&mut frame, 0).unwrap();

    let mut red = 0;
    for (i, &pixel) in frame.pixels().iter().enumerate() {
        let (x, y) = (i % splash::WIDTH as usize, i / splash::WIDTH as usize);
        if (1216..1248).contains(&x) && (653..693).contains(&y) {
            assert!(matches!(pixel, BACKGROUND | 0xdc0000));
            red += usize::from(pixel == 0xdc0000);
        } else {
            assert_eq!(pixel, original.pixels()[i]);
        }
    }
    assert_eq!(red, 501);
    // Selected variant 1 has a continuous wrist, not a diagonal gap through it.
    for y in 680..689 {
        for x in 1227..1238 {
            assert_eq!(frame.pixels()[y * splash::WIDTH as usize + x], 0xdc0000);
        }
    }
    // Rounded lower corners and transparent padding, without a backplate.
    for (x, y, color) in [(1229, 691, 0xdc0000), (1237, 691, 0xdc0000),
                         (1228, 691, BACKGROUND), (1238, 691, BACKGROUND),
                         (1216, 653, BACKGROUND), (1247, 692, BACKGROUND)] {
        assert_eq!(frame.pixels()[y * splash::WIDTH as usize + x], color);
    }
    let mut clean = splash::frame("Missing IGUI", true);
    splash::countdown(&mut clean, 0).unwrap();
    assert_eq!(frame.pixels(), clean.pixels());
}

#[test]
fn incremental_diagnostics_match_full_render_for_wrapping_shortening_and_resize() {
    let mut painter = splash::DiagnosticScreen::default();
    for (width, height) in [(1280, 720), (1024, 600), (321, 241), (1280, 720)] {
        let mut actual = Surface::new(width, height);
        let mut expected = Surface::new(width, height);
        for message in ["Diagnostics 0/10 s\nRAM: 19 samples\nUSB: unavailable".into(),
            "Diagnostics 1/10 s\nRAM: 2 samples".into(), "Я".into(), "".into(),
            "\n\nBlank rows\n".into(), "Длинный путь ".repeat(500), "Short".into()] {
            for seconds in [0, 1, 2, 3, 4, 4, 8] {
                painter.update(&mut actual, &message, seconds).unwrap();
                splash::draw_diagnostics(&mut expected, &message, seconds).unwrap();
                assert_eq!(actual.pixels(), expected.pixels(), "{width}x{height} {seconds}");
            }
        }
    }
}

struct DiagnosticTarget {
    surface: Surface,
    writes: usize,
    clears: usize,
    allowed: Option<embedded_graphics::primitives::Rectangle>,
    fail: bool,
}
impl DiagnosticTarget {
    fn new() -> Self {
        Self { surface: Surface::new(splash::WIDTH, splash::HEIGHT), writes: 0,
            clears: 0, allowed: None, fail: false }
    }
}
impl OriginDimensions for DiagnosticTarget {
    fn size(&self) -> Size { self.surface.size() }
}
impl DrawTarget for DiagnosticTarget {
    type Color = Rgb888;
    type Error = &'static str;
    fn draw_iter<I: IntoIterator<Item = Pixel<Rgb888>>>(&mut self, pixels: I) -> Result<(), Self::Error> {
        for pixel in pixels {
            if self.fail { self.fail = false; return Err("synthetic draw error"); }
            if let Some(area) = self.allowed { assert!(area.contains(pixel.0), "pixel outside updated region"); }
            self.writes += 1;
            self.surface.draw_iter([pixel]).unwrap();
        }
        Ok(())
    }
    fn clear(&mut self, color: Rgb888) -> Result<(), Self::Error> {
        assert!(self.allowed.is_none(), "full clear during partial draw");
        self.clears += 1;
        self.surface.clear(color).unwrap();
        Ok(())
    }
}

#[test]
fn changed_status_writes_only_its_row_and_unchanged_visible_text_writes_nothing() {
    use embedded_graphics::primitives::Rectangle;
    let mut target = DiagnosticTarget::new();
    let mut painter = splash::DiagnosticScreen::default();
    assert_eq!(painter.update(&mut target, "Diagnostics\nRAM: 100 samples", 0).unwrap(), splash::Redraw::Full);
    target.writes = 0;
    target.allowed = Some(Rectangle::new(Point::new(0, 440), Size::new(1280, 13)));

    assert_eq!(painter.update(&mut target, "Diagnostics\nRAM: 2 samples", 4).unwrap(), splash::Redraw::Partial);
    assert!(target.writes > 0 && target.writes < 1280 * 720 / 20);
    assert_eq!(target.clears, 1);
    target.writes = 0;
    assert_eq!(painter.update(&mut target, "Diagnostics\nRAM: 2 samples", 8).unwrap(), splash::Redraw::Unchanged);
    assert_eq!(target.writes, 0);

    target.allowed = Some(Rectangle::new(Point::new(559, 319), Size::new(160, 80)));
    assert_eq!(painter.update(&mut target, "Diagnostics\nRAM: 2 samples", 9).unwrap(), splash::Redraw::Partial);
    assert!(target.writes > 0 && target.writes < 160 * 80);
    let mut expected = Surface::new(1280, 720);
    splash::draw_diagnostics(&mut expected, "Diagnostics\nRAM: 2 samples", 9).unwrap();
    assert_eq!(target.surface.pixels(), expected.pixels());
}

#[test]
fn failed_partial_draw_and_explicit_reset_require_a_full_rebuild() {
    let mut target = DiagnosticTarget::new();
    let mut painter = splash::DiagnosticScreen::default();
    painter.update(&mut target, "Before", 0).unwrap();
    target.fail = true;

    assert_eq!(painter.update(&mut target, "After", 1).unwrap_err(), "synthetic draw error");
    assert_eq!(painter.update(&mut target, "After", 1).unwrap(), splash::Redraw::Full);
    let mut expected = Surface::new(1280, 720);
    splash::draw_diagnostics(&mut expected, "After", 1).unwrap();
    assert_eq!(target.surface.pixels(), expected.pixels());

    splash::draw(&mut target, "Different screen", true).unwrap();
    painter = Default::default();
    assert_eq!(painter.update(&mut target, "After", 1).unwrap(), splash::Redraw::Full);
    assert_eq!(target.surface.pixels(), expected.pixels());
}
