/// A URL you just copied, offered as the first row of an empty box. Only an
/// explicit scheme counts: a bare `github.com/x` is a plausible thing to have
/// copied for any other reason.
fn url_in_clipboard(text: &str) -> Option<String> {
    let value = text.trim();
    if value.is_empty() || value.len() >= 480 || value.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    let lower = value.to_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return None;
    }
    let rest = &value[value.find("://").unwrap() + 3..];
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() || !host.contains('.') {
        return None;
    }
    Some(value.to_string())
}

/// Read the clipboard once per open. `head -c` bounds the read: a clipboard
/// holding 40MB of screenshot costs one closed pipe rather than a string the
/// daemon then has to hold.
#[cfg(unix)]
pub(crate) async fn read_clipboard() -> Option<String> {
    let out = oxy_core::provider::process::run(
        "wl-paste -n -t text/plain 2>/dev/null | head -c 512",
        std::time::Duration::from_secs(2),
    )
    .await?;
    url_in_clipboard(&out.stdout)
}

/// The same read on Windows, through PowerShell's `Get-Clipboard`.
///
/// Spawned directly rather than through `bash -c`: the script extensions
/// assume a shell, but this row is the daemon's own and should not need one.
/// The read is bounded twice — `-Raw` is capped by the `Substring` and the
/// whole call by the timeout — because a clipboard can hold a screenshot.
#[cfg(windows)]
pub(crate) async fn read_clipboard() -> Option<String> {
    let script = "$c = Get-Clipboard -Raw -ErrorAction SilentlyContinue; \
                  if ($c) { $c.Substring(0, [Math]::Min(512, $c.Length)) }";
    let mut cmd = tokio::process::Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(2), cmd.output())
        .await
        .ok()?
        .ok()?;
    url_in_clipboard(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_explicit_url_counts() {
        assert_eq!(
            url_in_clipboard("https://github.com/x/y"),
            Some("https://github.com/x/y".into())
        );
        assert_eq!(
            url_in_clipboard("  http://example.com  "),
            Some("http://example.com".into())
        );
        // A bare host is a plausible thing to have copied for other reasons.
        assert_eq!(url_in_clipboard("github.com/x"), None);
        // So is a sentence, and a scheme with no host in it.
        assert_eq!(url_in_clipboard("look at https://example.com"), None);
        assert_eq!(url_in_clipboard("https://"), None);
        assert_eq!(url_in_clipboard("https://localhost/x"), None);
        assert_eq!(url_in_clipboard(""), None);
        // The read is bounded, so a screenshot-sized clipboard is not a row.
        assert_eq!(url_in_clipboard(&"h".repeat(600)), None);
    }
}
