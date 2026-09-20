//! `recent:` — the files you opened lately, read from the freedesktop
//! `recently-used.xbel` every GTK app writes to. A port of `bin/oxy-recent`:
//! the `files` view's fields — kind, size, folder, a thumbnail for pictures —
//! and `age` as the time since you opened it, not the file's mtime.
//!
//! No `date` and no `stat` per row: the timestamps are ISO-8601 with a fixed
//! shape, and the sizes come from one metadata call each.

use std::future::Future;
use std::pin::Pin;

use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::shellquote::quote;

pub struct Recent;

/// XBEL hrefs are XML-escaped and then percent-encoded, in that order.
/// Undoing them the other way round turns a literal "%26" in a filename into
/// an ampersand, so both passes stay separate and ordered.
fn unescape_xml(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        // Last, or "&amp;lt;" would come out as "<".
        .replace("&amp;", "&")
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let hex = |b: u8| (b as char).to_digit(16);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(a), Some(b)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((a * 16 + b) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{name}=\"");
    let start = tag.find(&key)? + key.len();
    let end = tag[start..].find('"')? + start;
    Some(&tag[start..end])
}

/// One `<bookmark …>` tag's (modified, decoded file path), or none.
fn bookmark(tag: &str) -> Option<(String, String)> {
    let href = attr(tag, "href")?;
    let modified = attr(tag, "modified")?.to_string();
    // Only local files: a recent list can hold smb:// and sftp:// entries,
    // which xdg-open would hand to a mount that is long gone.
    let path = href.strip_prefix("file://")?;
    Some((modified, urldecode(&unescape_xml(path))))
}

/// The civil-date math `calendar.rs` already carries, duplicated as two small
/// functions rather than shared: the timestamp shape here is fixed and needs
/// no date library.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// `YYYY-MM-DDTHH:MM:SS[.frac][Z|±HH:MM]` — the shape the XBEL writers emit.
/// Anything else is the epoch: a timestamp that cannot be read should not
/// silently misalign the rows under it.
fn parse_stamp(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b't') {
        return None;
    }
    let num = |a: usize, z: usize| s[a..z].parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let mut stamp = days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + sec;
    // A trailing offset: Z means UTC, ±HH:MM (or ±HHMM) shifts the other way.
    let rest = &s[19..];
    if rest.starts_with('+') || rest.starts_with('-') {
        let digits: String = rest[1..].chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.len() >= 4 {
            let oh = digits[..2].parse::<i64>().unwrap_or(0);
            let om = digits[2..4].parse::<i64>().unwrap_or(0);
            let shift = oh * 3600 + om * 60;
            stamp += if rest.starts_with('-') { shift } else { -shift };
        }
    }
    Some(stamp)
}

fn kind_of(ext: &str) -> &'static str {
    match ext {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "bmp" | "svg" | "tiff" | "tif"
        | "heic" | "ico" => "image",
        "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" | "wmv" | "flv" => "video",
        "mp3" | "flac" | "ogg" | "wav" | "m4a" | "opus" | "aac" | "wma" => "audio",
        "pdf" | "epub" | "djvu" | "doc" | "docx" | "odt" | "rtf" | "txt" | "md" | "org"
        | "tex" => "doc",
        "csv" | "tsv" | "xlsx" | "xls" | "ods" => "sheet",
        "zip" | "tar" | "gz" | "xz" | "zst" | "7z" | "rar" | "bz2" | "tgz" => "archive",
        "sh" | "bash" | "zsh" | "fish" | "py" | "js" | "mjs" | "ts" | "tsx" | "jsx"
        | "rs" | "go" | "c" | "h" | "cpp" | "hpp" | "java" | "rb" | "lua" | "qml"
        | "vim" | "pl" | "php" | "swift" | "kt" => "code",
        "json" | "yaml" | "yml" | "toml" | "ini" | "conf" | "xml" | "html" | "css"
        | "scss" | "sql" => "data",
        _ => "file",
    }
}

fn human_size(mut v: u64) -> String {
    let units = ["B", "K", "M", "G", "T", "P"];
    let mut i = 0;
    let mut rem = 0u64;
    while v >= 1024 && i < 5 {
        rem = (v % 1024) * 10 / 1024;
        v /= 1024;
        i += 1;
    }
    if i > 0 && v < 10 {
        format!("{v}.{rem}{}", units[i])
    } else {
        format!("{v}{}", units[i])
    }
}

fn ago(d: i64) -> String {
    if d < 60 {
        return "just now".into();
    }
    let (n, unit) = if d < 3600 {
        (d / 60, "minute")
    } else if d < 86400 {
        (d / 3600, "hour")
    } else if d < 2592000 {
        (d / 86400, "day")
    } else if d < 31536000 {
        (d / 2592000, "month")
    } else {
        (d / 31536000, "year")
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

fn short_age(d: i64) -> String {
    let d = d.max(0);
    if d < 60 {
        "now".into()
    } else if d < 3600 {
        format!("{}m", d / 60)
    } else if d < 86400 {
        format!("{}h", d / 3600)
    } else if d < 2592000 {
        format!("{}d", d / 86400)
    } else if d < 31536000 {
        format!("{}mo", d / 2592000)
    } else {
        format!("{}y", d / 31536000)
    }
}

impl NativeExt for Recent {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move {
            let xbel = crate::dirs::data_home().join("recently-used.xbel");
            let Ok(text) = std::fs::read_to_string(&xbel) else {
                return NativeOutcome::Empty;
            };
            let needle = ctx.arg.trim().to_lowercase();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);

            // Every <bookmark> tag, href+modified, newest first — ISO strings
            // sort chronologically as text.
            let mut entries: Vec<(String, String)> = Vec::new();
            let mut rest = text.as_str();
            while let Some(start) = rest.find("<bookmark") {
                let tag = &rest[start..];
                let Some(end) = tag.find('>') else { break };
                if let Some(b) = bookmark(&tag[..=end]) {
                    entries.push(b);
                }
                rest = &tag[end + 1..];
            }
            entries.sort_by(|a, b| b.0.cmp(&a.0));
            entries.truncate(60); // parse at most a page past the cap

            let home = crate::dirs::home().to_string_lossy().into_owned();
            let mut rows = Vec::new();
            for (modified, path) in entries {
                if rows.len() >= 25 {
                    break;
                }
                // Filtered before anything else reads the path.
                if !needle.is_empty() && !path.to_lowercase().contains(&needle) {
                    continue;
                }
                let meta = std::fs::metadata(&path).ok();
                let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                if meta.is_none() {
                    continue;
                }
                let delta = now - parse_stamp(&modified).unwrap_or(now);

                let base = path.rsplit('/').next().unwrap_or(&path).to_string();
                let dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or("/");
                let display = if let Some(rest) = dir.strip_prefix(&home) {
                    format!("~{rest}")
                } else {
                    dir.to_string()
                };

                let (ext, kind, size) = if is_dir {
                    (String::new(), "folder", String::new())
                } else {
                    let ext = if base.len() > 1 && base[1..].contains('.') {
                        let e = base.rsplit('.').next().unwrap_or("");
                        if e.len() <= 5 { e.to_string() } else { String::new() }
                    } else {
                        String::new()
                    };
                    let kind = kind_of(&ext.to_lowercase());
                    let size = human_size(meta.as_ref().map(|m| m.len()).unwrap_or(0));
                    (ext.to_uppercase(), kind, size)
                };
                let art = if kind == "image" {
                    format!("file://{path}")
                } else {
                    String::new()
                };
                let qpath = quote(&path);
                let qdir = quote(dir);
                rows.push(json!({
                    "id": path,
                    "title": base,
                    "dir": display,
                    "ext": ext,
                    "kind": kind,
                    "size": size,
                    "age": short_age(delta),
                    "art": art,
                    "subtitle": display,
                    "accessory": ago(delta),
                    "exec": format!("xdg-open {qpath}"),
                    "score": 90000 - rows.len() as i64 * 100,
                    "actions": [
                        { "title": "Open", "shortcut": "↵", "exec": format!("xdg-open {qpath}") },
                        { "title": "Copy Path", "exec": format!("printf %s {qpath} | wl-copy") },
                        { "title": "Open Folder", "exec": format!("xdg-open {qdir}") },
                        { "title": "Reveal in Files", "exec": format!("nautilus --select {qpath}") },
                    ]
                }));
            }
            NativeOutcome::Rows(rows)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_order() {
        // XML first, percent second: "%26" must not become an ampersand.
        assert_eq!(unescape_xml("a&amp;lt;b"), "a&lt;b");
        assert_eq!(urldecode("a%20b%26c"), "a b&c");
        assert_eq!(urldecode("100%"), "100%");
        assert_eq!(urldecode("%2f"), "/");
        assert_eq!(unescape_xml("&quot;x&apos;"), "\"x'");
    }

    #[test]
    fn stamps() {
        // 2024-01-01T00:00:00Z is 1704067200.
        assert_eq!(parse_stamp("2024-01-01T00:00:00Z"), Some(1704067200));
        assert_eq!(parse_stamp("2024-01-01T02:30:00+02:30"), Some(1704067200));
        assert_eq!(parse_stamp("garbage"), None);
        assert_eq!(parse_stamp("2024-13-99T00:00:00Z"), None);
    }

    #[test]
    fn sizes_and_ages() {
        assert_eq!(human_size(0), "0B");
        assert_eq!(human_size(1536), "1.5K");
        assert_eq!(human_size(1024 * 1024), "1.0M");
        assert_eq!(short_age(30), "now");
        assert_eq!(short_age(7200), "2h");
        assert_eq!(ago(30), "just now");
        assert_eq!(ago(90), "1 minute ago");
        assert_eq!(ago(86400 * 40), "1 month ago");
    }
}
