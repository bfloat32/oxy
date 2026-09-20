//! What every machine-reading provider shares: the `command -v` gate as a
//! PATH walk instead of a subprocess, so an absent tool is a cheap `false`
//! rather than a spawned shell per keystroke.

/// `command -v name` without the shell: true when `name` (or `name.EXE` and
/// friends on Windows) sits on PATH and is a file. The native providers
/// re-check their manifest's `when` this way — a worker asks a native even
/// when the `when` fails, so the provider answers `Fallback` here rather
/// than pretending a missing tool answered.
pub fn on_path(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    // An absolute or relative path with a separator checks the file itself.
    if name.contains(['/', '\\']) {
        return executable(std::path::Path::new(name));
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(name);
        if executable(&candidate) {
            return true;
        }
        #[cfg(windows)]
        {
            for ext in ["exe", "bat", "cmd", "com"] {
                if executable(&dir.join(format!("{name}.{ext}"))) {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(unix)]
fn executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn executable(path: &std::path::Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_binary_is_found() {
        // `cargo` built this test binary, so it is on this machine's PATH.
        assert!(on_path("cargo"));
    }

    #[test]
    fn an_absent_binary_is_not() {
        assert!(!on_path("definitely-not-a-real-tool-xyz"));
        assert!(!on_path(""));
    }
}
