//! `theme:` — the theme picker with live preview. A port of `bin/oxy-theme`.
//!
//! Moving the selection applies the theme as you go, and leaving without
//! choosing puts back the one you started on — a list of names says nothing
//! about what any of them looks like, so the launcher previews on selection
//! and reverts on Escape.
//!
//! The preview never calls `omarchy theme set`. It runs the one thing
//! `theme set` calls to retint the shell — `omarchy-shell shell applyTheme
//! <colors.toml b64> <shell.toml b64>` — at 44ms against `theme set`'s
//! 800ms, then `oxy-theme-preview <dir>` repaints every running foot
//! terminal over OSC. Reverting is the same call with the theme you
//! arrived on, so nothing is ever set and Escape leaves no trace. Enter
//! is the only `omarchy theme set`.
//!
//! The row cap is the script's own: the launcher holds every row it is
//! given and re-sorts them on every keystroke, and 322 themes was 387KB
//! and 2.1s per bare `theme:`. The answer is ordered here — current first,
//! then the list as `omarchy theme list` gave it — and cut to one
//! launcher-full before a single row is built, with `total` and `shown`
//! on every row so the view can say what it is not showing.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::native::util::on_path;
use crate::provider::process;
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::settings::paths;
use crate::support::quote::quote;

pub struct Theme;

/// One launcher-full of compact cards at the default card width — four
/// columns by twelve rows, the script's `OXY_THEME_LIMIT` default. Past
/// that the grid is scrolling through an answer nobody reads to the end,
/// and typing two letters is faster than any of it.
const DEFAULT_LIMIT: i64 = 48;

/// `omarchy theme list` on a machine that has collected hundreds of themes
/// is the read the cap exists for; `theme current` is a symlink lookup.
/// The worker's own timeout is four seconds, so the reads get three and
/// two — never the whole budget.
const LIST_TIMEOUT: Duration = Duration::from_secs(3);
const CURRENT_TIMEOUT: Duration = Duration::from_secs(2);

impl NativeExt for Theme {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        let arg = ctx.arg.clone();
        let cap = row_cap(ctx.settings.settings_for("theme"));
        Box::pin(async move {
            // The manifest's `when` is `command -v omarchy`, but a native is
            // asked even when the gate fails — so it is re-checked first,
            // and a missing omarchy declines to the script's own silence.
            if !on_path("omarchy") {
                return NativeOutcome::Fallback;
            }
            let (list, current) = tokio::join!(
                process::run("omarchy theme list", LIST_TIMEOUT),
                process::run("omarchy theme current", CURRENT_TIMEOUT),
            );
            let names = list.map(|f| f.stdout).unwrap_or_default();
            // `$(…)` strips trailing newlines; the match below is the
            // script's exact string comparison.
            let current = current
                .map(|f| f.stdout.trim_end_matches('\n').to_string())
                .unwrap_or_default();
            // Per-theme file reads are synchronous work; they have no
            // business on a runtime worker thread.
            tokio::task::spawn_blocking(move || answer(&names, &current, &arg, cap))
                .await
                .unwrap_or(NativeOutcome::Empty)
        })
    }
}

/// The whole question past the two `omarchy` reads: match, order, cap, and
/// build the rows — the work `spawn_blocking` carries.
fn answer(list_stdout: &str, current: &str, arg: &str, cap: Option<i64>) -> NativeOutcome {
    answer_at(&Roots::from_env(), list_stdout, current, arg, cap)
}

fn answer_at(
    roots: &Roots,
    list_stdout: &str,
    current: &str,
    arg: &str,
    cap: Option<i64>,
) -> NativeOutcome {
    let needle = arg.to_lowercase();
    let squashed_needle = squash(&needle);
    // Light and dark are the one split worth having when there are
    // hundreds, and a filter is cheaper than grouping — the card is already
    // painted in the theme's own background, so which half a theme is in
    // is the first thing you see. Names still match, so `theme:light`
    // keeps "Flexoki Light"; only those two queries pay a colors.toml
    // read per name that did not match.
    let mode_filter = match needle.as_str() {
        "light" | "dark" => Some(needle.as_str()),
        _ => None,
    };

    // Every theme that matches, in the order `omarchy theme list` gives
    // them — which is alphabetical.
    let mut matches: Vec<&str> = Vec::new();
    // `while IFS= read -r` drops a last line that never ended, so an
    // unterminated tail is not a theme.
    let mut lines: Vec<&str> = list_stdout.lines().collect();
    if !list_stdout.is_empty() && !list_stdout.ends_with('\n') {
        lines.pop();
    }
    for theme in lines {
        if theme.is_empty() {
            continue;
        }
        if !needle.is_empty() {
            let lower = theme.to_lowercase();
            let named = lower.contains(&needle) || squash(&lower).contains(&squashed_needle);
            if !named {
                match mode_filter {
                    Some(want) if theme_mode(&roots.theme_dir(theme)) == want => {}
                    _ => continue,
                }
            }
        }
        matches.push(theme);
    }
    if matches.is_empty() {
        return NativeOutcome::Empty;
    }
    let total = matches.len() as i64;

    // The current theme leads, wherever the alphabet put it — the launcher
    // scores rows too, but it only ever sees the ones that survive the cap,
    // so the order has to be right here first.
    let mut ordered: Vec<&str> = Vec::with_capacity(matches.len());
    ordered.extend(matches.iter().copied().filter(|t| *t == current));
    ordered.extend(matches.iter().copied().filter(|t| *t != current));

    // `[[ shown -gt limit ]] && shown=limit`: a failed comparison is not a
    // zero — it leaves `shown` at `total` and the answer uncapped.
    let shown = match cap {
        Some(limit) if total > limit => limit,
        _ => total,
    };
    if shown <= 0 {
        return NativeOutcome::Empty;
    }

    // The revert is the same string on every row — the theme you arrived
    // on — so it is built once; built in the loop it cost two base64 runs
    // per theme for an answer that never changed.
    let back = retint(&roots.theme_dir(current));

    let mut rows = Vec::with_capacity(shown as usize);
    let mut rank = 90000i64;
    for theme in ordered.iter().take(shown as usize) {
        rows.push(theme_row(
            roots,
            theme,
            rank,
            *theme == current,
            total,
            shown,
            &back,
        ));
        rank -= 100;
    }
    NativeOutcome::Rows(rows)
}

/// One card: the palette carried through to the view, so the card is
/// painted in the theme's own colours rather than naming them. A theme
/// with no colors.toml leaves the card unpainted — the honest answer —
/// and its preview empty: a preview that cannot be reverted must not be
/// offered.
fn theme_row(
    roots: &Roots,
    theme: &str,
    rank: i64,
    is_current: bool,
    total: i64,
    shown: i64,
    back: &str,
) -> Value {
    let (subtitle, score) = if is_current {
        ("Current theme", rank + 5000)
    } else {
        ("Theme", rank)
    };
    let dir = roots.theme_dir(theme);
    let colors_bytes = std::fs::read(dir.join("colors.toml")).ok();
    let c = colors_bytes
        .as_deref()
        .map(|b| parse_flat(&String::from_utf8_lossy(b)))
        .unwrap_or_default();
    let preview = match &colors_bytes {
        Some(bytes) => retint_with(&dir, bytes),
        None => String::new(),
    };
    // jq's `//`: only absence falls through — a key that is present with an
    // empty value still wins.
    let pick = |keys: &[&str], default: &str| -> String {
        keys.iter()
            .find_map(|k| c.get(*k))
            .cloned()
            .unwrap_or_else(|| default.to_string())
    };
    // Six hues, always in the same order, so the cards can be compared
    // column by column rather than read one at a time.
    let swatches: Vec<Value> = ["red", "yellow", "green", "cyan", "blue", "magenta"]
        .iter()
        .filter_map(|k| c.get(*k))
        .filter(|v| !v.is_empty())
        .map(|v| json!(v))
        .collect();
    json!({
        "id": theme,
        "title": theme,
        "subtitle": subtitle,
        "exec": format!("omarchy theme set {}", quote(theme)),
        "score": score,
        "glyph": "\u{f0e0c}",
        "previewExec": preview,
        "revertExec": back,
        "current": is_current,
        "total": total,
        "shown": shown,
        "mode": pick(&["mode"], "dark"),
        "bg": pick(&["background"], ""),
        "fg": pick(&["foreground"], ""),
        "dim": pick(&["dark_foreground", "muted"], ""),
        "accent": pick(&["accent"], ""),
        // The window a theme paints on top of its background: without it
        // two themes with the same near-black background look identical.
        "surface": pick(&["lighter_background", "selection"], ""),
        "swatches": swatches,
    })
}

/// Where a theme's files live. A user's own themes in
/// `~/.config/omarchy/themes` win over the shipped ones — which is what
/// `omarchy theme dir` answers and `omarchy theme set` reads. Looking only
/// in /usr/share left every card unpainted on exactly the machines this
/// keyword is for.
struct Roots {
    user: PathBuf,
    system: PathBuf,
}

impl Roots {
    fn from_env() -> Roots {
        let base = std::env::var("OMARCHY_PATH")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "/usr/share/omarchy".to_string());
        Roots {
            user: paths::home().join(".config/omarchy/themes"),
            system: PathBuf::from(base).join("themes"),
        }
    }

    fn theme_dir(&self, name: &str) -> PathBuf {
        let user = self.user.join(slug(name));
        if user.is_dir() {
            user
        } else {
            self.system.join(slug(name))
        }
    }
}

/// A theme's directory is its name lowercased with spaces as dashes — the
/// same translation `omarchy theme set` does.
fn slug(name: &str) -> String {
    name.to_lowercase().replace(' ', "-")
}

/// "tokyo night", "tokyonight" and "tokyo-night" should all find the same
/// theme — nobody remembers which spelling a theme shipped with.
fn squash(s: &str) -> String {
    s.chars().filter(|c| *c != ' ' && *c != '-').collect()
}

/// The theme's `mode`, read with the same narrow regex the script used —
/// `^mode[[:space:]]*=[[:space:]]*"([a-z]+)"` — first match wins, "dark"
/// when nothing says otherwise. Only `theme:light` and `theme:dark` ever
/// call this, so the file read costs nothing on any other query.
fn theme_mode(dir: &Path) -> String {
    let Ok(bytes) = std::fs::read(dir.join("colors.toml")) else {
        return "dark".to_string();
    };
    let text = String::from_utf8_lossy(&bytes);
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("mode") else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('"') else {
            continue;
        };
        let value: String = rest
            .chars()
            .take_while(|c| c.is_ascii_lowercase())
            .collect();
        if !value.is_empty() {
            return value;
        }
    }
    "dark".to_string()
}

/// The script's colors.toml read: the file is flat, every line is
/// `key = "value"`, so a TOML parser was never pulled in — jq's
/// `capture("^(?<k>[a-z_]+) *= *"(?<v>[^"]*)")` was enough. The same rules
/// here: nothing may lead the line, the key is lowercase letters and
/// underscores only, only spaces sit around the `=`, the value ends at the
/// next quote, and a repeated key's last line wins.
fn parse_flat(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let key_len = line
            .bytes()
            .take_while(|b| b.is_ascii_lowercase() || *b == b'_')
            .count();
        if key_len == 0 {
            continue;
        }
        let Some(rest) = line[key_len..].trim_start_matches(' ').strip_prefix('=') else {
            continue;
        };
        let Some(rest) = rest.trim_start_matches(' ').strip_prefix('"') else {
            continue;
        };
        let value = rest.split('"').next().unwrap_or_default();
        out.insert(line[..key_len].to_string(), value.to_string());
    }
    out
}

/// The retint call for one theme — or nothing when its colors.toml is
/// missing, because a preview that cannot be reverted must not be offered.
/// `oxy-theme-preview` is the sibling script that writes the OSC palette
/// to every foot tty; that does not fit in an exec string.
fn retint(dir: &Path) -> String {
    match std::fs::read(dir.join("colors.toml")) {
        Ok(colors) => retint_with(dir, &colors),
        Err(_) => String::new(),
    }
}

fn retint_with(dir: &Path, colors: &[u8]) -> String {
    // shell.toml is optional; the call still names the argument so the
    // receiver's argv does not shift.
    let shell = std::fs::read(dir.join("shell.toml")).unwrap_or_default();
    format!(
        "omarchy-shell shell applyTheme {} {}; oxy-theme-preview {}",
        quote(&base64(colors)),
        quote(&base64(&shell)),
        quote(&dir.to_string_lossy()),
    )
}

/// `base64 -w0` without the subprocess: the standard alphabet, `=` padding
/// on the tail, no wrapping. Most of a row's ~2.2KB is this.
fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = u32::from(chunk[0]) << 16
            | u32::from(*chunk.get(1).unwrap_or(&0)) << 8
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(ABC[((n >> 18) & 63) as usize] as char);
        out.push(ABC[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ABC[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ABC[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// `${OXY_THEME_LIMIT:-48}` — which the script leg receives from
/// `extensionSettings.theme.theme_limit` as an `OXY_…` environment prefix,
/// so the same key wins here too; the raw environment variable is the
/// fallback and 48 is the script's own default.
///
/// The returned Option is the cap, not the setting: `None` is the bash
/// arithmetic *failure*. `[[ shown -gt limit ]]` evaluates `limit` as an
/// arithmetic expression — a bare word reads as an unset variable (0, so
/// no rows), but anything else unparseable is a failed condition, which
/// leaves `shown` at `total` and the answer uncapped.
fn row_cap(settings: Option<&Map<String, Value>>) -> Option<i64> {
    cap_value(raw_limit(settings).as_deref())
}

fn raw_limit(settings: Option<&Map<String, Value>>) -> Option<String> {
    settings
        .and_then(|m| m.get("theme_limit"))
        .filter(|v| !v.is_null())
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .or_else(|| std::env::var("OXY_THEME_LIMIT").ok())
}

fn cap_value(raw: Option<&str>) -> Option<i64> {
    // `:-` covers unset and empty alike.
    let s = raw.unwrap_or_default();
    if s.is_empty() {
        return Some(DEFAULT_LIMIT);
    }
    let t = s.trim();
    if let Ok(n) = t.parse::<i64>() {
        return Some(n);
    }
    let mut chars = t.chars();
    if matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Some(0);
    }
    None
}

#[cfg(test)]
mod tests;
