/// A URL you just copied, offered as the first row of an empty box. Only an
/// explicit scheme counts: a bare `github.com/x` is a plausible thing to have
/// copied for any other reason.
#[cfg(unix)]
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

#[cfg(not(unix))]
pub(crate) async fn read_clipboard() -> Option<String> {
    None
}
