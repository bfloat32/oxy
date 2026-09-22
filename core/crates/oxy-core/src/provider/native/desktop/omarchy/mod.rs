//! `omarchy:` — every Omarchy setting, searchable, from Omarchy's own menu
//! definition. A port of `bin/oxy-omarchy`, the one Python provider.
//!
//! `omarchy:` used to be a hand-copied list of routes inside the launcher,
//! a list that goes stale the first time Omarchy adds a setting. This reads
//! the same file the menu itself reads, so a route that exists in the menu
//! exists here, and one that does not cannot.
//!
//! The tree is flattened. The menu is something you walk, four keystrokes
//! deep, and a launcher is something you type into, so the path becomes
//! context on one row: "Theme" under "Style" reads as `Theme · Style`, and
//! typing either word finds it. A row that opens a submenu is kept as well
//! as its children — sometimes the thing you want is the submenu.
//!
//! The path, the icon and whether a route acts or opens all go across as
//! their own fields, because the `menutree` view draws them rather than
//! reading them out of a sentence. Joining the path into "Style · Theme"
//! here and splitting it there would make a separator string into a wire
//! format.
//!
//! A bare `omarchy:` is not a search, it is browsing, and the answer to
//! browsing is the menu's own root in the menu's own order — not ten
//! top-level rows plus whatever second-level routes sorted alphabetically
//! first, which is a list assembled by ranking rules for a query nobody
//! typed.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::native::util::on_path;
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::settings::paths;

/// The file the menu itself reads, then the user's overlay — same two
/// paths, same precedence: a user entry with an existing id replaces it,
/// which is how somebody removes a route they do not want.
const DEFAULT_MENU: &str = "/usr/share/omarchy/default/omarchy/omarchy-menu.jsonc";

fn user_menu() -> PathBuf {
    paths::home().join(".config/omarchy/extensions/omarchy-menu.jsonc")
}

/// The most one answer ever shows — the script's `shown >= 16` cap.
const MAX_ROWS: usize = 16;

#[derive(Default)]
pub struct Omarchy;

impl NativeExt for Omarchy {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        let arg = ctx.arg.clone();
        Box::pin(async move {
            // The manifest's `when` is `command -v omarchy`, but a native is
            // asked even when the gate fails — so it is re-checked first,
            // and a missing omarchy declines to the script's own silence.
            if !on_path("omarchy") {
                return NativeOutcome::Fallback;
            }
            // `" ".join(sys.argv[1:]).strip().lower()` — the shell already
            // split the query into words, so the join collapses the
            // whitespace runs a raw `arg` still carries.
            let query = arg.split_whitespace().collect::<Vec<_>>().join(" ");
            let query = query.to_lowercase();
            let browsing = query.is_empty();
            // The file reads and the walk are synchronous work; they have
            // no business on a runtime worker thread.
            let candidates = tokio::task::spawn_blocking(move || {
                let tree = items();
                collect(&tree, &query)
            })
            .await
            .unwrap_or_default();

            // The per-row `when` shells out, so the emit loop — sorted
            // rows, lazily gated, until sixteen are shown — stays async.
            let mut out = Vec::new();
            for c in &candidates {
                if out.len() >= MAX_ROWS {
                    break;
                }
                if !passes(&c.when).await {
                    continue;
                }
                out.push(row_json(c, out.len() + 1, browsing));
            }
            if out.is_empty() {
                NativeOutcome::Empty
            } else {
                NativeOutcome::Rows(out)
            }
        })
    }
}

/// Python truthiness: null, false, 0, "", [] and {} all read as "absent".
/// The script's `entry.get(x) or y` chain runs on it everywhere.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn truthy_at(entry: &Value, key: &str) -> bool {
    entry.get(key).is_some_and(truthy)
}

/// Bar panels. These are Omarchy settings by any reasonable definition and
/// the menu file does not contain them, because the menu is not how you
/// reach them. Leaving them out meant `omarchy:wifi` answered with a QR
/// code generator.
///
/// `run` is the command, not a menu route. Every other row here is reached
/// with `omarchy menu summon <id>`, and these ids are ours: there is no
/// `panel.network` node for the menu to find, so summoning one opened the
/// menu at its root and `omarchy:wifi` quietly did nothing useful.
fn panels() -> Vec<(&'static str, Value)> {
    let panel = |icon: &str, label: &str, aliases: &[&str], what: &str| {
        json!({
            "icon": icon,
            "label": label,
            "aliases": aliases,
            "action": format!("omarchy-shell shell summon omarchy.{what}"),
            "run": format!("omarchy-shell shell summon omarchy.{what}"),
        })
    };
    vec![
        (
            "panel.network",
            panel(
                "\u{f05f3}",
                "Wi-Fi",
                &["wifi", "wireless", "network", "internet"],
                "network",
            ),
        ),
        (
            "panel.bluetooth",
            panel(
                "\u{f00af}",
                "Bluetooth",
                &["bluetooth", "pair", "headphones"],
                "bluetooth",
            ),
        ),
        (
            "panel.audio",
            panel(
                "\u{f057e}",
                "Audio",
                &["audio", "sound", "volume", "output"],
                "audio",
            ),
        ),
        (
            "panel.display",
            panel(
                "\u{f0379}",
                "Display",
                &["display", "monitor", "scale", "resolution"],
                "display",
            ),
        ),
        (
            "panel.battery",
            panel("\u{f0079}", "Battery", &["battery", "power"], "battery"),
        ),
    ]
}

/// The default menu with the user's extension layered on top — the script's
/// `items()`. Panels lead the order; file entries follow in file order, and
/// a re-declared id keeps its first position while taking the new body —
/// dict-update semantics, which `preserve_order` shares.
fn items() -> Map<String, Value> {
    items_from(&[PathBuf::from(DEFAULT_MENU), user_menu()])
}

fn items_from(paths: &[PathBuf]) -> Map<String, Value> {
    let mut merged = Map::new();
    for (k, v) in panels() {
        merged.insert(k.to_string(), v);
    }
    for path in paths {
        for (key, value) in load(path) {
            if value.is_object() {
                merged.insert(key, value);
            }
        }
    }
    merged
}

/// Comments and trailing commas — the same two things the shell strips, and
/// deliberately the same crude rules rather than a real JSONC parser:
/// matching the menu's own leniency matters more than being correct about a
/// file the menu would also reject.
fn strip_jsonc(raw: &str) -> String {
    // `^\s*//[^\n]*(\n|$)` under re.M: a line whose first non-whitespace
    // characters are `//` goes, whitespace and all. `//` mid-line stays.
    let mut no_comments = String::with_capacity(raw.len());
    for line in raw.split_inclusive('\n') {
        if line.trim_start().starts_with("//") {
            continue;
        }
        no_comments.push_str(line);
    }
    // `,(\s*[}\]])` → `\1`: a comma directly ahead of a closer is dropped,
    // whitespace — including newlines — in between notwithstanding, and the
    // same blind rule applies inside strings.
    let mut out = String::with_capacity(no_comments.len());
    for (i, c) in no_comments.char_indices() {
        if c == ',' && no_comments[i + 1..].trim_start().starts_with(['}', ']']) {
            continue;
        }
        out.push(c);
    }
    out
}

/// One menu file, or an empty map — every failure the script's `load()`
/// swallows: unreadable, blank after stripping, unparseable, or not an
/// object. A file carrying an `"items"` object answers with that; anything
/// else is itself the map.
fn load(path: &Path) -> Map<String, Value> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Map::new();
    };
    let raw = strip_jsonc(&raw);
    if raw.trim().is_empty() {
        return Map::new();
    }
    let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
        return Map::new();
    };
    let Value::Object(obj) = parsed else {
        return Map::new();
    };
    match obj.get("items") {
        Some(Value::Object(items)) => items.clone(),
        _ => obj,
    }
}

/// The route's own `label`, else the last id segment made presentable —
/// `node_id.split(".")[-1].replace("-", " ").title()`.
fn label_of(entry: Option<&Value>, node_id: &str) -> String {
    if let Some(v) = entry.and_then(|e| e.get("label")).filter(|v| truthy(v))
        && let Some(s) = v.as_str()
    {
        return s.to_string();
    }
    // A truthy non-string label is input the script crashes on; the
    // survivable read is the same fallback an absent label takes.
    let last = node_id.rsplit('.').next().unwrap_or(node_id);
    title_case(&last.replace('-', " "))
}

/// Python's `str.title()`: a letter is uppercased when the character before
/// it was not a letter, lowercased otherwise — so "a1b" is "A1B" and
/// "foo_bar" is "Foo_Bar".
fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_cased = false;
    for ch in s.chars() {
        let cased = ch.is_lowercase() || ch.is_uppercase();
        if cased {
            if prev_cased {
                out.extend(ch.to_lowercase());
            } else {
                out.extend(ch.to_uppercase());
            }
            prev_cased = true;
        } else {
            out.push(ch);
            prev_cased = false;
        }
    }
    out
}

/// `str(a).lower()` for one `aliases` entry — numbers and bools read the
/// way Python prints them, and null is "none".
fn py_str_lower(v: &Value) -> String {
    match v {
        Value::String(s) => s.to_lowercase(),
        Value::Null => "none".to_string(),
        Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Value::Number(n) => n.to_string(),
        // A container's Python repr differs from its JSON spelling; a menu
        // file never puts one here, so the JSON spelling stands in.
        other => other.to_string().to_lowercase(),
    }
}

/// `[str(a).lower() for a in (entry.get("aliases") or [])]` — a list is a
/// list; a bare string iterates its characters and a dict its keys, the way
/// Python's `for` does.
fn aliases_of(entry: &Value) -> Vec<String> {
    match entry.get("aliases") {
        Some(Value::Array(a)) => a.iter().map(py_str_lower).collect(),
        Some(Value::String(s)) if !s.is_empty() => {
            s.chars().map(|c| c.to_string().to_lowercase()).collect()
        }
        Some(Value::Object(o)) if !o.is_empty() => o.keys().map(|k| k.to_lowercase()).collect(),
        // A truthy non-iterable kills the script; an empty alias list is
        // the survivable read.
        _ => Vec::new(),
    }
}

/// One row's worth of precomputed answer — everything the emit loop needs
/// except the `when` verdict, which is shelled out for at the last moment.
struct Cand {
    node_id: String,
    label: String,
    label_low: String,
    trail: Vec<String>,
    kind: &'static str,
    rank: u8,
    missed: usize,
    when: Value,
    icon: Value,
    description: Value,
    children: usize,
    depth: usize,
    exec: Value,
}

/// The script's row loop: every node but `root`, flattened, matched and
/// ranked — then either cut to the root's own order (browsing) or sorted by
/// where the words landed (searching).
fn collect(tree: &Map<String, Value>, query: &str) -> Vec<Cand> {
    let mut rows = Vec::new();
    for (node_id, entry) in tree.iter() {
        if node_id == "root" {
            continue;
        }
        let label = label_of(Some(entry), node_id);
        let parts: Vec<&str> = node_id.split('.').collect();
        // The path, as words, so "Change Theme" is findable by typing
        // "style". A missing ancestor still names its last segment.
        let trail: Vec<String> = (0..parts.len().saturating_sub(1))
            .map(|i| {
                let prefix = parts[..=i].join(".");
                label_of(tree.get(&prefix), &prefix)
            })
            .collect();

        let aliases = aliases_of(entry);
        let label_low = label.to_lowercase();
        let trail_low = trail.join(" ").to_lowercase();
        let haystack = format!(
            "{label_low} {} {} {}",
            node_id.to_lowercase(),
            trail_low,
            aliases.join(" ")
        );

        // Where the words matched decides the order. Ranking on depth
        // alone put "Extra Themes" above "Theme" for `theme`, because both
        // sit two levels down and E sorts before T. Every word must appear
        // somewhere, in any order, so "theme change" still finds "Change
        // Theme".
        let mut rank = 0u8;
        let mut missed = 0usize;
        if !query.is_empty() {
            let asked = query.split_whitespace().count();
            // Words that match nothing anywhere are dropped rather than
            // fatal: "change theme" is how people ask for the theme
            // setting, and the menu calls it "Theme", so requiring both
            // words found nothing at all. At least one word still has to
            // land.
            let words: Vec<&str> = query
                .split_whitespace()
                .filter(|w| haystack.contains(*w))
                .collect();
            if words.is_empty() {
                continue;
            }
            // How much of what was asked actually landed, ahead of where
            // it landed: "set volume" matching only "set" on Setup should
            // lose to Audio, which matches "volume" and is what the words
            // together mean.
            missed = asked - words.len();
            let effective = words.join(" ");
            rank = if label_low == query || label_low == effective {
                0
            } else if label_low.starts_with(query) || label_low.starts_with(&effective) {
                1
            } else if aliases.iter().any(|a| {
                a == query || *a == effective || a.starts_with(query) || a.starts_with(&effective)
            }) {
                2
            } else if label_low.contains(query) {
                3
            } else if words.iter().all(|w| label_low.contains(*w)) {
                4
            } else {
                5
            };
        }

        let kind = if truthy_at(entry, "action") {
            "action"
        } else if truthy_at(entry, "target") {
            "link"
        } else {
            "menu"
        };

        // `run` is the command verbatim — the panels' summon — and any
        // other route is reached through the menu itself.
        let exec = match entry.get("run") {
            Some(v) if truthy(v) => v.clone(),
            _ => Value::String(format!("omarchy menu summon {node_id}")),
        };

        rows.push(Cand {
            depth: parts.len(),
            // How much is behind this route — every descendant at any
            // depth, not just direct children. Only worth saying on a
            // tile, where "Style" and "About" are the same size and one of
            // them holds eighteen settings.
            children: tree
                .keys()
                .filter(|k| k.starts_with(&format!("{node_id}.")))
                .count(),
            node_id: node_id.clone(),
            label,
            label_low,
            trail,
            kind,
            rank,
            missed,
            when: entry.get("when").cloned().unwrap_or(Value::Null),
            icon: entry
                .get("icon")
                .filter(|v| truthy(v))
                .cloned()
                .unwrap_or_else(|| Value::String(String::new())),
            description: entry
                .get("description")
                .filter(|v| truthy(v))
                .cloned()
                .unwrap_or_else(|| Value::String(String::new())),
            exec,
        });
    }

    if query.is_empty() {
        // The root, in the order the menu file writes it. Not sorted: the
        // menu's order is the one the user has already learned from
        // opening the menu, and alphabetising it here would make the same
        // ten things sit somewhere else depending on which surface they
        // came from.
        rows.retain(|r| !r.node_id.contains('.'));
    } else {
        // Then a shorter path, because a top-level route is almost always
        // what someone typing two letters meant. Stable, so equal ranks
        // keep the file's own order.
        rows.sort_by(|a, b| {
            (a.missed, a.rank, a.depth, &a.label_low).cmp(&(
                b.missed,
                b.rank,
                b.depth,
                &b.label_low,
            ))
        });
    }
    rows
}

/// A row the menu would hide, hidden here too — the script's `passes()`.
/// `when` is a shell condition the menu runs; running it per row would mean
/// a subprocess per route on every keystroke, so it only runs for rows that
/// survived the text match, and there are never many. Two seconds is the
/// script's fuse; a wedged probe, a failed spawn and a non-string `when`
/// (which Python raises on) all read as "hidden".
async fn passes(when: &Value) -> bool {
    let cond = match when {
        v if !truthy(v) => return true,
        Value::String(s) => s,
        _ => return false,
    };
    let spawned = tokio::process::Command::new("bash")
        .arg("-lc")
        .arg(cond)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let Ok(mut child) = spawned else {
        return false;
    };
    match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
        Ok(Ok(status)) => status.success(),
        _ => {
            let _ = child.kill().await;
            false
        }
    }
}

/// `json.dumps(s)` — double quotes, the short escapes, and `ensure_ascii`:
/// every non-ASCII codepoint as `\uXXXX`, surrogate pairs past the BMP.
/// The Copy action's exec embeds it, so the quoting is the script's own.
fn py_dumps_str(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c if (c as u32) < 0x7f => out.push(c),
            c => {
                let cp = c as u32;
                if cp > 0xffff {
                    let v = cp - 0x10000;
                    out.push_str(&format!(
                        "\\u{:04x}\\u{:04x}",
                        0xd800 + (v >> 10),
                        0xdc00 + (v & 0x3ff)
                    ));
                } else {
                    out.push_str(&format!("\\u{:04x}", cp));
                }
            }
        }
    }
    out.push('"');
    out
}

/// One emitted row — every key the script prints, in the script's order.
/// `shown` is the 1-based count of rows being emitted; the script
/// increments before it scores, so the first row lands at 94900.
fn row_json(c: &Cand, shown: usize, browsing: bool) -> Value {
    // Still written out, because a row that ends up in the plain list view
    // for any reason should not lose its context. The view ignores it.
    let mut subtitle = if c.trail.is_empty() {
        "Omarchy".to_string()
    } else {
        c.trail.join("  ·  ")
    };
    if c.kind == "menu" {
        subtitle.push_str("  ·  opens a submenu");
    }
    json!({
        "id": format!("omarchy-{}", c.node_id),
        "title": c.label,
        "subtitle": subtitle,
        "detail": c.description,
        "accessory": c.node_id,
        "glyph": c.icon,
        "exec": c.exec,
        "score": 95000 - shown as i64 * 100,
        "view": "menutree",
        // Browsing and searching are two different pictures of the same
        // tree, and only this provider knows which one was asked for.
        "mode": if browsing { "browse" } else { "search" },
        "trail": c.trail,
        "kind": c.kind,
        "node": c.node_id,
        "depth": c.depth,
        "children": c.children,
        "actions": [
            { "title": "Open", "shortcut": "↵", "exec": c.exec },
            { "title": "Copy the Route",
              "exec": format!("printf %s {} | wl-copy", py_dumps_str(&c.node_id)) },
        ],
    })
}

#[cfg(test)]
mod tests;
