// SPDX-License-Identifier: BSD-3-Clause
//! Pixel-equivalence and write-boundary checks; no hardware or display session.
#![cfg(feature = "diag-input")]
use embedded_graphics::{pixelcolor::Rgb888, prelude::*, primitives::Rectangle};
use imanlib::surface::Surface;
use iman::{display::Redraw, input_test::{render::{self, Screen}, InputTest, Stage}};

struct Target {
    surface: Surface,
    writes: usize,
    clears: usize,
    allowed: Option<Rectangle>,
    fail: bool,
}
impl Target {
    fn new() -> Self {
        Self { surface: Surface::new(1280, 720), writes: 0, clears: 0, allowed: None, fail: false }
    }
}
impl OriginDimensions for Target {
    fn size(&self) -> Size { self.surface.size() }
}
impl DrawTarget for Target {
    type Color = Rgb888;
    type Error = &'static str;
    fn draw_iter<I: IntoIterator<Item = Pixel<Rgb888>>>(&mut self, pixels: I) -> Result<(), Self::Error> {
        for pixel in pixels {
            if self.fail { self.fail = false; return Err("synthetic render failure"); }
            if let Some(area) = self.allowed { assert!(area.contains(pixel.0), "write outside dirty area"); }
            self.writes += 1;
            self.surface.draw_iter([pixel]).unwrap();
        }
        Ok(())
    }
    fn clear(&mut self, color: Rgb888) -> Result<(), Self::Error> {
        assert!(self.allowed.is_none(), "partial update must not clear the frame");
        self.clears += 1;
        self.surface.clear(color).unwrap();
        Ok(())
    }
}

#[test]
fn discovery_seconds_counts_and_input_touch_only_their_own_rows() {
    let mut target = Target::new();
    let mut screen = Screen::default();
    let mut view = InputTest::default().view(0);
    assert_eq!(screen.update(&mut target, &view).unwrap(), Redraw::Full);
    target.writes = 0;
    target.allowed = Some(Rectangle::new(Point::new(1214, 24), Size::new(66, 13)));

    view.seconds = 4;
    assert_eq!(screen.update(&mut target, &view).unwrap(), Redraw::Partial);
    assert!(target.writes > 0 && target.writes < 2000);
    assert_eq!(target.clears, 1);
    target.writes = 0;
    assert_eq!(screen.update(&mut target, &view).unwrap(), Redraw::Unchanged);
    assert_eq!(target.writes, 0);

    target.allowed = Some(Rectangle::new(Point::new(0, 389), Size::new(1280, 20)));
    view.count = 4;
    view.opened = 4;
    assert_eq!(screen.update(&mut target, &view).unwrap(), Redraw::Partial);
    target.allowed = Some(Rectangle::new(Point::new(32, 660), Size::new(1248, 13)));
    view.notice = "Synthetic notice".into();
    assert_eq!(screen.update(&mut target, &view).unwrap(), Redraw::Partial);
    target.writes = 0;
    view.input = "Hidden by notice".into();
    assert_eq!(screen.update(&mut target, &view).unwrap(), Redraw::Unchanged);
    assert_eq!(target.writes, 0);
    view.notice.clear();
    assert_eq!(screen.update(&mut target, &view).unwrap(), Redraw::Partial);

    let mut expected = Surface::new(1280, 720);
    render::draw(&mut expected, &view).unwrap();
    assert!(target.surface.pixels() == expected.pixels());
    assert_eq!(target.clears, 1);
}

#[test]
fn incremental_frames_match_full_render_through_all_controller_steps_and_shortening() {
    let mut actual = Surface::new(1280, 720);
    let mut expected = Surface::new(1280, 720);
    let mut screen = Screen::default();
    let mut view = InputTest::default().view(0);
    let stages = [Stage::Discover, Stage::Prepare { number: 1 }, Stage::Prepare { number: 4 }].into_iter().chain((0..25).flat_map(|step| {
        [false, true].map(|release| Stage::Controller { number: 4, step, release })
    })).chain([Stage::Finished]);
    for stage in stages {
        view.stage = stage;
        for (seconds, input, notice) in [(5, "Input: KEY 103 = 1", ""),
            (4, "Input: -", "Device unavailable"), (0, "Input: -", ""), (0, "Input: -", "")] {
            view.seconds = seconds;
            view.input = input.into();
            view.notice = notice.into();
            screen.update(&mut actual, &view).unwrap();
            render::draw(&mut expected, &view).unwrap();
            assert!(actual.pixels() == expected.pixels(), "{:?}, {seconds}", view.stage);
        }
    }
}

#[test]
fn preparation_countdown_updates_only_the_central_digits_and_erases_shortened_text() {
    let mut target = Target::new();
    let mut screen = Screen::default();
    let mut view = InputTest::default().view(0);
    view.stage = Stage::Prepare { number: 2 };
    view.count = 4;
    view.opened = 4;
    view.seconds = 4;
    assert_eq!(screen.update(&mut target, &view).unwrap(), Redraw::Full);
    assert!(target.surface.pixels()[500 * 1280..560 * 1280].iter().all(|&pixel| pixel == 0x0f151d));
    target.allowed = Some(Rectangle::new(Point::new(594, 404), Size::new(90, 60)));
    let mut expected = Surface::new(1280, 720);

    for seconds in [3, 2, 1, 0] {
        view.seconds = seconds;
        target.writes = 0;

        assert_eq!(screen.update(&mut target, &view).unwrap(), Redraw::Partial);

        assert!(target.writes > 0 && target.writes < 10_000);
        assert_eq!(target.clears, 1);
        render::draw(&mut expected, &view).unwrap();
        assert!(target.surface.pixels() == expected.pixels());
        target.writes = 0;
        assert_eq!(screen.update(&mut target, &view).unwrap(), Redraw::Unchanged);
        assert_eq!(target.writes, 0);
    }
}

#[test]
fn changing_steps_and_release_prompts_never_repaints_the_static_controller_heading() {
    let mut target = Target::new();
    let mut screen = Screen::default();
    let mut view = InputTest::default().view(0);
    view.stage = Stage::Controller { number: 1, step: 0, release: false };
    view.count = 4;
    view.opened = 4;
    view.seconds = 5;
    assert_eq!(screen.update(&mut target, &view).unwrap(), Redraw::Full);
    let mut expected = Surface::new(1280, 720);

    for step in 0..25 {
        if step > 0 {
            target.allowed = Some(Rectangle::new(Point::new(0, 137), Size::new(1280, 508)));
            view.stage = Stage::Controller { number: 1, step, release: false };
            target.writes = 0;

            assert_eq!(screen.update(&mut target, &view).unwrap(), Redraw::Partial);

            assert!(target.writes < 1280 * 720);
            render::draw(&mut expected, &view).unwrap();
            assert!(target.surface.pixels() == expected.pixels(), "step {step}");
        }
        target.allowed = Some(Rectangle::new(Point::new(0, 569), Size::new(1280, 76)));
        view.stage = Stage::Controller { number: 1, step, release: true };

        assert_eq!(screen.update(&mut target, &view).unwrap(), Redraw::Partial);

        render::draw(&mut expected, &view).unwrap();
        assert!(target.surface.pixels() == expected.pixels(), "release {step}");
        assert_eq!(target.clears, 1);
    }
}

#[test]
fn resize_multiline_input_and_explicit_reset_rebuild_the_whole_frame() {
    let mut screen = Screen::default();
    let mut view = InputTest::default().view(0);
    for (w, h) in [(1280, 720), (1024, 600), (1280, 720)] {
        let mut actual = Surface::new(w, h);
        let mut expected = Surface::new(w, h);
        assert_eq!(screen.update(&mut actual, &view).unwrap(), Redraw::Full);
        view.notice = "first line\nsecond line".into();
        assert_eq!(screen.update(&mut actual, &view).unwrap(), Redraw::Full);
        render::draw(&mut expected, &view).unwrap();
        assert!(actual.pixels() == expected.pixels());
        view.notice.clear();
        assert_eq!(screen.update(&mut actual, &view).unwrap(), Redraw::Full);
        actual.clear(Rgb888::BLACK).unwrap();
        screen = Screen::default();
        assert_eq!(screen.update(&mut actual, &view).unwrap(), Redraw::Full);
        render::draw(&mut expected, &view).unwrap();
        assert!(actual.pixels() == expected.pixels());
    }
}

#[test]
fn partial_render_failure_does_not_leave_a_valid_cached_frame() {
    let mut target = Target::new();
    let mut screen = Screen::default();
    let mut view = InputTest::default().view(0);
    screen.update(&mut target, &view).unwrap();
    target.fail = true;
    view.seconds = 4;

    assert_eq!(screen.update(&mut target, &view).unwrap_err(), "synthetic render failure");
    assert_eq!(screen.update(&mut target, &view).unwrap(), Redraw::Full);
    let mut expected = Surface::new(1280, 720);
    render::draw(&mut expected, &view).unwrap();
    assert!(target.surface.pixels() == expected.pixels());
}
