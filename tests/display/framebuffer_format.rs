// SPDX-License-Identifier: BSD-3-Clause
#![cfg(feature = "drm")]

use imanlib::kms::format::Format;
use iman::display::framebuffer_format;

#[test]
fn manager_selection_does_not_change_the_explicit_format_default() {
    assert_eq!(Format::default(), Format::Xrgb8888);
    #[cfg(feature = "rgb565")]
    assert_eq!(framebuffer_format(), Format::Rgb565);
    #[cfg(not(feature = "rgb565"))]
    assert_eq!(framebuffer_format(), Format::Xrgb8888);
}

#[test]
fn selected_format_has_drm_registration() {
    let format = framebuffer_format();
    let registration = format.registration().unwrap();

    assert_eq!(registration.bits_per_pixel as usize, format.bits_per_pixel());
}
