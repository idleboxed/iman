// SPDX-License-Identifier: BSD-3-Clause
//! Resolve only Python 3.12; child fixtures must not depend on a system prefix.
use std::{fs, path::{Path, PathBuf}, process::Command, sync::OnceLock};

pub fn find_python() -> &'static Path {
    static PYTHON: OnceLock<PathBuf> = OnceLock::new();

    PYTHON.get_or_init(|| {
        let output = Command::new("python3.12")
            .args(["-c", "import sys\nassert sys.version_info[:2] == (3, 12), \"Python 3.12 is required\"\nprint(sys.executable)"])
            .output()
            .expect("Python 3.12 must be available on PATH for child fixtures");
        assert!(output.status.success(), "Python 3.12 discovery failed: {}",
            String::from_utf8_lossy(&output.stderr));
        let executable = String::from_utf8(output.stdout).expect("Python executable path must be UTF-8");
        let executable = Path::new(executable.trim());
        assert!(executable.is_absolute(), "Python executable path must be absolute");
        let executable = fs::canonicalize(executable).expect("Python executable path must exist");
        let path = executable.to_str().expect("Python executable path must be UTF-8");
        assert!(!path.chars().any(char::is_whitespace), "Python executable path must be usable in a shebang");
        executable
    }).as_path()
}
