// SPDX-License-Identifier: BSD-3-Clause
//! Static PPM frames only. Live PC sessions use the ordinary iman binary with pc.
use imanlib::surface::Surface;
use iman::display::{gui_error_message, splash};
use std::{io::{self, Write}, path::Path};

fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() > 1 {
        return Err(io::Error::other("expected one screen name"));
    }
    let frame = match args.first().map(String::as_str) {
        None | Some("initializing") => splash::frame(splash::INITIALIZING, false),
        Some("starting") => splash::frame(splash::STARTING, false),
        Some("diagnostics") => {
            let mut frame = Surface::new(splash::WIDTH, splash::HEIGHT);
            splash::draw_diagnostics(&mut frame, "Run iman with the pc feature for live diagnostics", 3).unwrap();
            frame
        }
        #[cfg(feature = "diag-input")]
        Some("input-test") => {
            let mut frame = Surface::new(splash::WIDTH, splash::HEIGHT);
            iman::input_test::render::draw(&mut frame, &iman::input_test::View {
                stage: iman::input_test::Stage::Controller { number: 1, step: 9, release: false },
                count: 2, opened: 2, seconds: 4, input: "Input: KEY 28 = 0".into(), notice: String::new(),
            }).unwrap();
            frame
        }
        Some("report") => {
            let mut frame = Surface::new(splash::WIDTH, splash::HEIGHT);
            splash::draw_report_path(&mut frame,
                "/mnt/sdcard/iman-reports/iman-341-ipc-session-Ab12Cd-cycle-000001/report.json").unwrap();
            frame
        }
        Some("error" | "goodbye") => {
            let mut frame = splash::frame(&gui_error_message(Path::new("/mnt/sdcard/system/igui/igui"), &std::io::Error::other("IGUI exited with signal: 25 (SIGXFSZ)")), true);
            splash::countdown(&mut frame, if args[0] == "goodbye" { 0 } else { 10 }).unwrap();
            frame
        }
        Some(_) => return Err(io::Error::other("expected initializing, starting, diagnostics, input-test (diag-input), report, error or goodbye; use iman with pc for a window")),
    };
    let mut output = io::stdout().lock();
    write!(output, "P6\n{} {}\n255\n", frame.width(), frame.height())?;
    output.write_all(&frame.rgb_bytes())
}
