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

/// `printf '%q'` — bash's re-parseable quoting, which scripts used to put a
/// value inside a longer command line (`omarchy reminder N …`, `oxy-pass
/// copy …`): a plain word when it can be, backslashes for the
/// metacharacters ("tea time" is `tea\ time`, not two arguments), `$'…'`
/// once a control byte shows up. The escaped set is the one bash quotes:
/// `#` and `~` only matter at the front, `,` and `!` count as
/// metacharacters everywhere. For a whole single argument
/// `support::quote::quote` is the one; `shq` is for a word inside a line.
pub(crate) fn shq(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.chars().any(|c| c.is_control()) {
        let mut out = String::from("$'");
        for ch in s.chars() {
            match ch {
                '\'' => out.push_str("\\'"),
                '\\' => out.push_str("\\\\"),
                '\x07' => out.push_str("\\a"),
                '\x08' => out.push_str("\\b"),
                '\t' => out.push_str("\\t"),
                '\n' => out.push_str("\\n"),
                '\x0b' => out.push_str("\\v"),
                '\x0c' => out.push_str("\\f"),
                '\r' => out.push_str("\\r"),
                c if c.is_control() => {
                    // bash writes what it cannot print as octal bytes
                    let mut buf = [0u8; 4];
                    for b in c.encode_utf8(&mut buf).as_bytes() {
                        out.push_str(&format!("\\{b:03o}"));
                    }
                }
                c => out.push(c),
            }
        }
        out.push('\'');
        return out;
    }
    let mut out = String::with_capacity(s.len());
    for (i, ch) in s.chars().enumerate() {
        let esc = match ch {
            ' ' | '!' | '"' | '$' | '&' | '\'' | '(' | ')' | '*' | ',' | ';' | '<' | '>' | '?'
            | '[' | '\\' | ']' | '^' | '`' | '{' | '|' | '}' => true,
            '#' | '~' => i == 0,
            _ => false,
        };
        if esc {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shq_is_bashs_percent_q() {
        assert_eq!(shq("tea"), "tea");
        assert_eq!(shq("github/work"), "github/work");
        assert_eq!(shq("tea time"), "tea\\ time");
        assert_eq!(shq("call mum"), "call\\ mum");
        assert_eq!(shq("it's"), "it\\'s");
        assert_eq!(shq("100%"), "100%");
        assert_eq!(shq("a,b"), "a\\,b");
        assert_eq!(shq("a:b"), "a:b");
        assert_eq!(shq("a!b"), "a\\!b");
        assert_eq!(shq("#tag"), "\\#tag");
        assert_eq!(shq("a#b"), "a#b");
        assert_eq!(shq("~me"), "\\~me");
        assert_eq!(shq("a~b"), "a~b");
        assert_eq!(shq("café"), "café");
        assert_eq!(shq(""), "''");
        assert_eq!(shq("a\nb"), "$'a\\nb'");
        assert_eq!(shq("a\tb"), "$'a\\tb'");
    }

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
