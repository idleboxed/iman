// SPDX-License-Identifier: BSD-3-Clause
use std::process::Command;

fn main() {
    // Like IGUI, refresh the stamp on package changes, not when reusing a binary.
    let output = Command::new("date")
        .args(["-u", "+%Y%m%d%H%M"])
        .output()
        .expect("failed to run date for the IMAN UTC build timestamp");
    assert!(output.status.success(), "date failed: {}", output.status);
    let timestamp = String::from_utf8(output.stdout).expect("date output must be UTF-8");
    let timestamp = timestamp.trim();
    assert!(
        timestamp.len() == 12 && timestamp.bytes().all(|byte| byte.is_ascii_digit()),
        "invalid IMAN UTC build timestamp: {timestamp:?}"
    );
    println!("cargo:rustc-env=IMAN_BUILD_UTC={timestamp}");
}
