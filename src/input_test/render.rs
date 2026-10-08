// SPDX-License-Identifier: BSD-3-Clause
//! Static geometry and English prompts. No images, font caches, timers or I/O.
use super::{Stage, View};
use crate::display::Redraw;
use embedded_graphics::{
    mono_font::{ascii::{FONT_10X20, FONT_7X13}, MonoTextStyle},
    pixelcolor::Rgb888, prelude::*,
    primitives::{Circle, Line, PrimitiveStyle, Rectangle, RoundedRectangle, Triangle},
    text::{Baseline, Text},
};

pub const BACKGROUND: Rgb888 = Rgb888::new(15, 21, 29);
const BODY: Rgb888 = Rgb888::new(38, 51, 60);
const KEY: Rgb888 = Rgb888::new(21, 32, 41);
const EDGE: Rgb888 = Rgb888::new(82, 102, 108);
const TEXT: Rgb888 = Rgb888::new(230, 234, 231);
const MUTED: Rgb888 = Rgb888::new(151, 169, 174);
const ACTIVE: Rgb888 = Rgb888::new(119, 207, 180);

struct Scaled<'a, D> { target: &'a mut D, origin: Point, scale: u32 }
impl<D: DrawTarget<Color = Rgb888>> OriginDimensions for Scaled<'_, D> {
    fn size(&self) -> Size { self.target.bounding_box().size }
}
impl<D: DrawTarget<Color = Rgb888>> DrawTarget for Scaled<'_, D> {
    type Color = Rgb888;
    type Error = D::Error;
    fn draw_iter<I: IntoIterator<Item = Pixel<Rgb888>>>(&mut self, pixels: I) -> Result<(), D::Error> {
        for Pixel(point, color) in pixels {
            self.target.fill_solid(&Rectangle::new(self.origin + point * self.scale as i32,
                Size::new(self.scale, self.scale)), color)?;
        }
        Ok(())
    }
}

fn text<D: DrawTarget<Color = Rgb888>>(target: &mut D, value: &str, center: Point, scale: u32, color: Rgb888) -> Result<(), D::Error> {
    let origin = center - Point::new(value.chars().count() as i32 * 10 * scale as i32 / 2, 10 * scale as i32);
    Text::with_baseline(value, Point::zero(), MonoTextStyle::new(&FONT_10X20, color), Baseline::Top)
        .draw(&mut Scaled { target, origin, scale }).map(|_| ())
}

fn small<D: DrawTarget<Color = Rgb888>>(target: &mut D, value: &str, point: Point) -> Result<(), D::Error> {
    Text::with_baseline(value, point, MonoTextStyle::new(&FONT_7X13, MUTED), Baseline::Top)
        .draw(target).map(|_| ())
}

fn style(active: bool) -> PrimitiveStyle<Rgb888> {
    PrimitiveStyle::with_fill(if active { ACTIVE } else { KEY })
}

fn button<D: DrawTarget<Color = Rgb888>>(target: &mut D, rect: Rectangle, label: &str, active: bool, round: bool) -> Result<(), D::Error> {
    let border = PrimitiveStyle::with_stroke(if active { ACTIVE } else { EDGE }, if active { 3 } else { 1 });
    if round {
        Circle::new(rect.top_left, rect.size.width).into_styled(style(active)).draw(target)?;
        Circle::new(rect.top_left - Point::new(4, 4), rect.size.width + 8).into_styled(border).draw(target)?;
    } else {
        RoundedRectangle::with_equal_corners(rect, Size::new(6, 6)).into_styled(style(active)).draw(target)?;
        RoundedRectangle::with_equal_corners(rect, Size::new(6, 6)).into_styled(border).draw(target)?;
    }
    text(target, label, rect.center(), 1, if active { BACKGROUND } else { MUTED })
}

fn arrow<D: DrawTarget<Color = Rgb888>>(target: &mut D, center: Point, direction: usize, color: Rgb888) -> Result<(), D::Error> {
    let direction = [Point::new(0, -1), Point::new(0, 1), Point::new(-1, 0), Point::new(1, 0)][direction];
    let perpendicular = Point::new(-direction.y, direction.x);
    let tip = center + direction * 14;
    Line::new(center - direction * 14, tip).into_styled(PrimitiveStyle::with_stroke(color, 3)).draw(target)?;
    for side in [-1, 1] {
        Line::new(tip, tip - direction * 9 + perpendicular * (side * 8))
            .into_styled(PrimitiveStyle::with_stroke(color, 3)).draw(target)?;
    }
    Ok(())
}

fn diagram<D: DrawTarget<Color = Rgb888>>(target: &mut D, step: usize) -> Result<(), D::Error> {
    // Rounded rectangles avoid the ellipse rasterizer's large-diameter arithmetic.
    for (x, y, w, h) in [(345, 326, 175, 220), (760, 326, 175, 220), (360, 292, 560, 213)] {
        RoundedRectangle::with_equal_corners(Rectangle::new(Point::new(x, y), Size::new(w, h)), Size::new(150, 150))
            .into_styled(PrimitiveStyle::with_fill(BODY)).draw(target)?;
    }
    for (x, left) in [(410, true), (770, false)] {
        RoundedRectangle::with_equal_corners(Rectangle::new(Point::new(x - 6, 239), Size::new(112, 85)), Size::new(12, 12))
            .into_styled(PrimitiveStyle::with_fill(BODY)).draw(target)?;
        button(target, Rectangle::new(Point::new(x, 226), Size::new(100, 40)), if left { "L2" } else { "R2" }, step == if left { 12 } else { 13 }, false)?;
        button(target, Rectangle::new(Point::new(x, 282), Size::new(100, 24)), if left { "L1" } else { "R1" }, step == if left { 10 } else { 11 }, false)?;
    }
    for (index, (x, y, label)) in [(0, (449, 368, "^")), (1, (449, 432, "v")), (2, (417, 400, "<")), (3, (481, 400, ">"))] {
        button(target, Rectangle::new(Point::new(x, y), Size::new(32, 32)), label, step == index, false)?;
    }
    target.fill_solid(&Rectangle::new(Point::new(449, 400), Size::new(32, 32)), KEY)?;
    for (index, x, y, letter) in [(7, 820, 347, "Y"), (5, 877, 404, "B"), (4, 820, 461, "A"), (6, 763, 404, "X")] {
        let active = step == index;
        let center = Point::new(x, y);
        button(target, Rectangle::new(center - Point::new(32, 32), Size::new(64, 64)), "", active, true)?;
        let color = if active { BACKGROUND } else { MUTED };
        text(target, letter, center + Point::new(13, 0), 1, color)?;
        let center = center - Point::new(12, 0);
        let outline = PrimitiveStyle::with_stroke(color, 2);
        match index {
            7 => { Triangle::new(center + Point::new(0, -10), center + Point::new(-9, 8), center + Point::new(9, 8)).into_styled(outline).draw(target)?; }
            5 => { Circle::new(center - Point::new(9, 9), 18).into_styled(outline).draw(target)?; }
            6 => { Rectangle::new(center - Point::new(8, 8), Size::new(16, 16)).into_styled(outline).draw(target)?; }
            _ => {
                Line::new(center - Point::new(8, 8), center + Point::new(8, 8)).into_styled(outline).draw(target)?;
                Line::new(center + Point::new(-8, 8), center + Point::new(8, -8)).into_styled(outline).draw(target)?;
            }
        }
    }
    button(target, Rectangle::new(Point::new(617, 340), Size::new(46, 46)), "H", step == 16, true)?;
    button(target, Rectangle::new(Point::new(526, 408), Size::new(94, 28)), "SELECT", step == 8, false)?;
    button(target, Rectangle::new(Point::new(639, 408), Size::new(80, 28)), "START", step == 9, false)?;
    for (x, press, first, label) in [(557, 14, 17, "L3"), (707, 15, 21, "R3")] {
        let moving = (first..first + 4).contains(&step);
        let active = step == press || moving;
        button(target, Rectangle::new(Point::new(x - 36, 474), Size::new(72, 72)), if moving { "" } else { label }, active, true)?;
        if moving { arrow(target, Point::new(x, 510), step - first, BACKGROUND)?; }
    }
    Ok(())
}

/// Keeps only the visible state, never another full-size image. Reset whenever
/// another screen overwrites the retained target. Failed draws discard the cache.
#[derive(Default)]
pub struct Screen {
    previous: Option<(Rectangle, View)>,
}

impl Screen {
    pub fn update<D: DrawTarget<Color = Rgb888>>(
        &mut self, target: &mut D, view: &View,
    ) -> Result<Redraw, D::Error> {
        let bounds = target.bounding_box();
        let previous = self.previous.take();
        let mut redraw = Redraw::Unchanged;
        let partial = previous.as_ref().is_some_and(|(old_bounds, old)| {
            let same_layout = match (&old.stage, &view.stage) {
                (Stage::Controller { number: old_number, .. }, Stage::Controller { number, .. }) => old_number == number,
                _ => old.stage == view.stage,
            };
            *old_bounds == bounds && bounds == Rectangle::new(Point::zero(), Size::new(1280, 720))
                && same_layout
                && (view.stage == Stage::Discover || (old.count == view.count && old.opened == view.opened))
                && !input_line(old).chars().any(char::is_control)
                && !input_line(view).chars().any(char::is_control)
        });
        if previous.as_ref().is_some_and(|(old_bounds, old)| *old_bounds == bounds && old == view) {
            // No surface writes or presentation for an identical visible state.
        } else if partial {
            let (_, old) = previous.as_ref().unwrap();
            if old.seconds != view.seconds {
                target.fill_solid(&seconds_area(bounds, view), BACKGROUND)?;
                draw_seconds(target, bounds, view)?;
                redraw = Redraw::Partial;
            }
            if let (Stage::Controller { step: old_step, release: old_release, .. },
                Stage::Controller { step, release, .. }) = (&old.stage, &view.stage) {
                if old_step != step {
                    target.fill_solid(&Rectangle::new(Point::new(0, 137), Size::new(1280, 20)), BACKGROUND)?;
                    draw_step(target, bounds.center(), *step)?;
                    target.fill_solid(&Rectangle::new(Point::new(320, 210), Size::new(640, 350)), BACKGROUND)?;
                    diagram(target, *step)?;
                    redraw = Redraw::Partial;
                }
                if old_step != step || old_release != release {
                    target.fill_solid(&Rectangle::new(Point::new(0, 569), Size::new(1280, 76)), BACKGROUND)?;
                    draw_instruction(target, bounds.center(), *step, *release)?;
                    redraw = Redraw::Partial;
                }
            }
            if old.count != view.count || old.opened != view.opened {
                target.fill_solid(&Rectangle::new(Point::new(0, bounds.center().y + 30), Size::new(1280, 20)), BACKGROUND)?;
                draw_discovery_counts(target, bounds, view)?;
                redraw = Redraw::Partial;
            }
            if input_line(old) != input_line(view) {
                target.fill_solid(&Rectangle::new(Point::new(32, 660), Size::new(1248, 13)), BACKGROUND)?;
                small(target, input_line(view), Point::new(32, 660))?;
                redraw = Redraw::Partial;
            }
        } else {
            draw(target, view)?;
            redraw = Redraw::Full;
        }
        self.previous = Some((bounds, view.clone()));
        Ok(redraw)
    }
}

fn input_line(view: &View) -> &str {
    if view.notice.is_empty() { &view.input } else { &view.notice }
}

fn seconds_area(bounds: Rectangle, view: &View) -> Rectangle {
    if matches!(view.stage, Stage::Prepare { .. }) {
        Rectangle::new(bounds.center() + Point::new(-45, 45), Size::new(90, 60))
    } else {
        Rectangle::new(Point::new(bounds.size.width as i32 - 66, 24), Size::new(66, 13))
    }
}

fn draw_seconds<D: DrawTarget<Color = Rgb888>>(
    target: &mut D, bounds: Rectangle, view: &View,
) -> Result<(), D::Error> {
    if view.seconds > 0 {
        if matches!(view.stage, Stage::Prepare { .. }) {
            text(target, &view.seconds.to_string(), bounds.center() + Point::new(0, 75), 3, ACTIVE)?;
        } else {
            small(target, &format!("{} s", view.seconds), Point::new(bounds.size.width as i32 - 66, 24))?;
        }
    }
    Ok(())
}

fn draw_discovery_counts<D: DrawTarget<Color = Rgb888>>(
    target: &mut D, bounds: Rectangle, view: &View,
) -> Result<(), D::Error> {
    text(target, &format!("Controllers detected: {} (input streams opened: {})", view.count, view.opened),
        bounds.center() + Point::new(0, 40), 1, ACTIVE)
}

fn draw_step<D: DrawTarget<Color = Rgb888>>(target: &mut D, center: Point, step: usize) -> Result<(), D::Error> {
    text(target, &format!("STEP {} / 25", step + 1), Point::new(center.x, 147), 1, ACTIVE)
}

fn draw_instruction<D: DrawTarget<Color = Rgb888>>(
    target: &mut D, center: Point, step: usize, release: bool,
) -> Result<(), D::Error> {
    let names = ["D-pad up", "D-pad down", "D-pad left", "D-pad right",
        "bottom face button", "right face button", "left face button", "top face button",
        "SELECT", "START", "L1", "R1", "left rear trigger", "right rear trigger",
        "left stick", "right stick", "HOME (H)",
        "left stick up", "left stick down", "left stick left", "left stick right",
        "right stick up", "right stick down", "right stick left", "right stick right"];
    let moving = step >= 17;
    let instruction = if release && moving { "Return the stick to center".into() }
        else if release { format!("Release {}", names[step]) }
        else if moving { format!("Move {}", names[step]) }
        else if step == 14 || step == 15 { format!("Click the {}", names[step]) }
        else { format!("Press {}", names[step]) };
    text(target, &instruction, Point::new(center.x, 589), 2, TEXT)?;
    if !release { text(target, if moving { "then return to center" } else { "then release" }, Point::new(center.x, 635), 1, TEXT)?; }
    Ok(())
}

pub fn draw<D: DrawTarget<Color = Rgb888>>(target: &mut D, view: &View) -> Result<(), D::Error> {
    target.clear(BACKGROUND)?;
    let bounds = target.bounding_box();
    let center = bounds.center();
    small(target, "Controller test", Point::new(32, 24))?;
    draw_seconds(target, bounds, view)?;
    match view.stage {
        Stage::Discover => {
            text(target, "Press a button on each controller", center - Point::new(0, 30), 2, TEXT)?;
            draw_discovery_counts(target, bounds, view)?;
        }
        Stage::Prepare { number } => {
            text(target, &format!("CONTROLLER {number} / {}", view.count), Point::new(center.x, 100), 2, TEXT)?;
            text(target, if number == 1 { "Take the first controller" } else { "Take the next controller" },
                center - Point::new(0, 45), 2, TEXT)?;
            text(target, "Release all controls", center + Point::new(0, 10), 1, MUTED)?;
        }
        Stage::Controller { number, step, release } => {
            text(target, &format!("CONTROLLER {number} / {}", view.count), Point::new(center.x, 100), 2, TEXT)?;
            draw_step(target, center, step)?;
            diagram(target, step)?;
            draw_instruction(target, center, step, release)?;
        }
        Stage::Finished => {
            text(target, if view.opened == 0 { "No matching input devices opened" }
                else if view.count == 0 { "No fresh button presses detected" } else { "Controller test complete" }, center, 2, TEXT)?;
        }
    }
    small(target, input_line(view), Point::new(32, bounds.size.height as i32 - 60))?;
    small(target, concat!("iman ", env!("CARGO_PKG_VERSION"), "-", env!("IMAN_BUILD_UTC")), Point::new(32, bounds.size.height as i32 - 35))
}
