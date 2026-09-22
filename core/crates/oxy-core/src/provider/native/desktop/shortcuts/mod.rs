//! `shortcuts:` — every key that is bound on this machine, drawn as a keymap
//! rather than as a list. A port of `bin/oxy-shortcuts`.
//!
//!   shortcuts:              everything, grouped by modifier family
//!   shortcuts:screenshot    the ones whose action matches
//!   shortcuts:super shift   the ones whose combination matches
//!   shortcuts:win+k         the same, with the names people actually type
//!
//! The truth is `hyprctl binds`, but not straight from it. Omarchy configures
//! Hyprland from Lua, and Hyprland reports every Lua bind as dispatcher
//! `__lua` with a numeric arg, and reports `code:` binds with no key at all —
//! most of the workspace switching comes back with an empty `key` field.
//! `omarchy-menu-keybindings --print` already solves both, by re-reading
//! hyprland.lua for the keys Hyprland dropped and resolving keycodes through
//! the compiled XKB keymap, so the port keeps the script's order: the menu's
//! "COMBO → Action" lines first, `hyprctl binds -j` when it is absent or
//! silent. The hyprctl leg is honest but poorer — it can only report binds
//! that still carry a key, and the modmask is translated here.
//!
//! The launcher's own keys are in neither source. Ctrl+K, Ctrl+1 to Ctrl+9
//! and Escape live in Launcher.qml's key handler and Hyprland has never heard
//! of them; they are listed too, marked Oxy, because somebody asking what a
//! key does does not care which program owns it — and when the system owns
//! the same combination the row says so, because then the key may never
//! reach the launcher at all.
//!
//! Enter copies the combination in the form hyprland.lua wants. It does not
//! fire the binding: the dispatcher is `__lua` and the arg is a number, so
//! there is nothing here that could be re-run — and a third of these
//! bindings close a window, kill a process or lock the screen. A keymap that
//! can maim you for pressing Enter in it is not a keymap.
//!
//! Port notes: jq's `@sh` is spelled with `util::shq` (%q-style backslashes
//! rather than single quotes — one shell word either way), and the record
//! stream is kept as the tab-split the script's jq passes saw, so a field
//! carrying a tab is extra columns exactly as it was there.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::native::util::{on_path, shq};
use crate::provider::process;
use crate::provider::{Ctx, NativeExt, NativeOutcome};

#[cfg(test)]
mod tests;

/// One external call's deadline: the script had none, but a wedged
/// compositor or menu is silence either way — this is the difference between
/// "no answer" and "the launcher hung".
const CALL: Duration = Duration::from_secs(2);

pub struct Shortcuts;

/// Launcher.qml's key handler, written out — the script's `oxy_binds`
/// verbatim, `mods \t key \t action`. These are true whether or not Hyprland
/// is answering, so they are not gated on it: they describe the window the
/// reader is looking at while they read them.
///
/// Ctrl+1 to Ctrl+9 is nine bindings and one idea, so it is one row. Nine
/// rows of "Run result N" would be four percent of the keymap saying the
/// same thing.
const OXY_BINDS: &[(&str, &str, &str)] = &[
    ("CTRL", "K", "Actions for the selected result"),
    ("CTRL", "P", "Pin or unpin the selected result"),
    ("CTRL", "N", "Next result"),
    ("CTRL SHIFT", "P", "Previous result"),
    ("CTRL", "1 – 9", "Run the first to ninth result"),
    ("CTRL", "RETURN", "Ask the agent this question"),
    ("SHIFT", "RETURN", "Second action on the selected result"),
    ("", "RETURN", "Run the selected result"),
    (
        "",
        "ESCAPE",
        "Leave the answer, step back, clear, then close",
    ),
    ("", "TAB", "Next result"),
    ("SHIFT", "TAB", "Previous result"),
    ("", "UP", "Previous result"),
    ("", "DOWN", "Next result"),
    ("", "LEFT", "Adjust the value, where a view has one"),
    ("", "RIGHT", "Adjust the value, where a view has one"),
    ("", "PRIOR", "Up one page of results"),
    ("", "NEXT", "Down one page of results"),
];

/// One `scope \t mods \t key \t action` record, stored as the tab-split the
/// script's jq pass saw — a field carrying a tab becomes extra columns and
/// the tail is lost, exactly as `split("\t")` did it.
fn record(scope: &str, mods: &str, key: &str, action: &str) -> Vec<String> {
    format!("{scope}\t{mods}\t{key}\t{action}")
        .split('\t')
        .map(String::from)
        .collect()
}

/// jq's `"\(.)"` interpolation: strings pass through, everything else
/// renders as JSON — with whole floats written the way jq writes them
/// ("64", not "64.0").
fn interp(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n
            .as_i64()
            .map(|i| i.to_string())
            .or_else(|| n.as_u64().map(|u| u.to_string()))
            .or_else(|| n.as_f64().map(|f| format!("{f}")))
            .unwrap_or_default(),
        other => other.to_string(),
    }
}

/// `(.field // "") != ""` — jq `//` falls through on null and false, and a
/// non-string is never equal to `""`. So the field survives unless it is
/// missing, null, false, or the empty string.
fn nonempty(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// The jq `mods` table over `.modmask | tostring`: Hyprland's mask bits are
/// shift 1, ctrl 4, alt 8, super 64, and a combination the table does not
/// know — a bind holding CapsLock, say — reads as no modifiers, which is the
/// script's own shrug.
fn modmask_words(v: Option<&Value>) -> &'static str {
    let key = v.map(interp).unwrap_or_default();
    match key.as_str() {
        "0" => "",
        "1" => "SHIFT",
        "4" => "CTRL",
        "5" => "SHIFT CTRL",
        "8" => "ALT",
        "9" => "SHIFT ALT",
        "12" => "CTRL ALT",
        "13" => "SHIFT CTRL ALT",
        "64" => "SUPER",
        "65" => "SUPER SHIFT",
        "68" => "SUPER CTRL",
        "69" => "SUPER SHIFT CTRL",
        "72" => "SUPER ALT",
        "73" => "SUPER SHIFT ALT",
        "76" => "SUPER CTRL ALT",
        "77" => "SUPER SHIFT CTRL ALT",
        _ => "",
    }
}

/// The `hyprctl_binds` leg: without Omarchy's menu there is only the raw
/// report, and a bind whose key Hyprland dropped cannot be drawn as a key,
/// so it is left out rather than shown as a modifier with a hole after it.
/// The description is the action; a bind without one has nothing to say,
/// since `__lua` and `16` are not an answer. A non-array answer is the
/// `if type != "array" then empty` — nothing.
fn parse_binds(text: &str) -> Vec<Vec<String>> {
    let Ok(v) = serde_json::from_str::<Value>(text.trim()) else {
        return Vec::new();
    };
    let Some(binds) = v.as_array() else {
        return Vec::new();
    };
    binds
        .iter()
        .filter_map(|b| {
            if !nonempty(b.get("key")) || !nonempty(b.get("description")) {
                return None;
            }
            Some(record(
                "System",
                modmask_words(b.get("modmask")),
                &interp(&b["key"]),
                &interp(&b["description"]),
            ))
        })
        .collect()
}

/// The `omarchy_binds` leg: the menu's cheatsheet, reduced to the three
/// facts a row needs. Its output is "%-35s → %s", so the arrow is the split
/// and the padding is trimmed. The combination itself has exactly one " + "
/// in it, between the modifiers and the key, which is why the modifiers can
/// be recovered without a modmask table.
fn parse_menu(text: &str) -> Vec<Vec<String>> {
    text.lines()
        .filter_map(|line| {
            let at = line.find(" → ")?;
            let combo = line[..at].trim_end_matches([' ', '\t']);
            let action = line[at + " → ".len()..].trim_end_matches([' ', '\t']);
            if action.is_empty() || combo.is_empty() {
                return None;
            }
            let (mods, key) = match combo.find(" + ") {
                Some(i) => (&combo[..i], &combo[i + 3..]),
                None => ("", combo),
            };
            if key.is_empty() {
                return None;
            }
            Some(record("System", mods, key, action))
        })
        .collect()
}

/// A modifier is drawn as a word, not as a shout — the chips are small and
/// there are up to four of them on a line, and SUPER SHIFT CTRL ALT in caps
/// reads as an error message. A name the table does not know gets its first
/// letter's worth of respect and no more.
fn modname(m: &str) -> String {
    match m {
        "SUPER" => "Super".to_string(),
        "SHIFT" => "Shift".to_string(),
        "CTRL" => "Ctrl".to_string(),
        "ALT" => "Alt".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first, chars.as_str().to_ascii_lowercase()),
                None => String::new(),
            }
        }
    }
}

/// Ties inside a family, so Super Shift Ctrl always sorts before Super
/// Shift Alt rather than by whatever order the source happened to print.
fn modweight(m: &str) -> i64 {
    match m {
        "SUPER" => 0,
        "SHIFT" => 1,
        "CTRL" => 2,
        "ALT" => 3,
        _ => 4,
    }
}

/// jq's `gsub("(?<a>[a-z0-9])(?<b>[A-Z])"; "\(.a) \(.b)")` — a space at every
/// lower-or-digit → upper hump, because nobody reads a keycap name like
/// XF86AudioRaiseVolume.
fn hump_split(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    let mut prev = '\0';
    for c in s.chars() {
        if c.is_ascii_uppercase() && (prev.is_ascii_lowercase() || prev.is_ascii_digit()) {
            out.push(' ');
        }
        out.push(c);
        prev = c;
    }
    out
}

/// What goes on the last chip. A punctuation key is drawn as the
/// punctuation, because that is what is printed on the key, and an XF86 name
/// is split at its humps.
fn keydisplay(k: &str) -> String {
    let upper = k.to_ascii_uppercase();
    let named = match upper.as_str() {
        "RETURN" => "Return",
        "ESCAPE" => "Esc",
        "SPACE" => "Space",
        "TAB" => "Tab",
        "BACKSPACE" => "Backspace",
        "DELETE" => "Delete",
        "PRINT" => "Print",
        "HOME" => "Home",
        "END" => "End",
        "PRIOR" => "Page Up",
        "NEXT" => "Page Down",
        "UP" => "↑",
        "DOWN" => "↓",
        "LEFT" => "←",
        "RIGHT" => "→",
        "COMMA" => ",",
        "PERIOD" => ".",
        "SLASH" => "/",
        "MINUS" => "-",
        "EQUAL" => "=",
        "GRAVE" => "`",
        "BRACKETLEFT" => "[",
        "BRACKETRIGHT" => "]",
        "LEFT MOUSE BUTTON" => "Click",
        "RIGHT MOUSE BUTTON" => "Right click",
        "MIDDLE MOUSE BUTTON" => "Middle click",
        "MOUSE_DOWN" => "Wheel ↓",
        "MOUSE_UP" => "Wheel ↑",
        _ => "",
    };
    if !named.is_empty() {
        return named.to_string();
    }
    if let Some(rest) = k.strip_prefix("XF86") {
        return hump_split(rest);
    }
    if k.chars().count() == 1 {
        return upper;
    }
    k.to_string()
}

/// Names people type that no source uses. Without these, "win" finds
/// nothing on a machine where every second binding is a Super binding.
fn keyaliases(k: &str) -> &'static str {
    match k.to_ascii_uppercase().as_str() {
        "RETURN" => "enter",
        "ESCAPE" => "esc",
        "PRINT" => "printscreen prtsc",
        "GRAVE" => "tilde backtick",
        "SPACE" => "spacebar",
        "DELETE" => "del",
        "BACKSPACE" => "bksp",
        "PRIOR" => "page up pgup",
        "NEXT" => "page down pgdn",
        "LEFT MOUSE BUTTON" => "lmb click mouse",
        "RIGHT MOUSE BUTTON" => "rmb click mouse",
        "MOUSE_DOWN" | "MOUSE_UP" => "scroll wheel mouse",
        _ => {
            if !k.starts_with("XF86") {
                ""
            } else if k.contains("Audio") {
                "media fn"
            } else {
                "fn"
            }
        }
    }
}

/// The same combination reduced to something both sources agree on, for the
/// one question that crosses between them: does the system already own this
/// key? Modifiers sort lexicographically here — not by modweight — because
/// that is the order the `taken` pass writes, so the two agree.
fn combo_key(mods: &[String], key: &str) -> String {
    let mut sorted: Vec<&str> = mods.iter().map(String::as_str).collect();
    sorted.sort();
    format!("{}|{}", sorted.join("+"), key.to_ascii_uppercase())
}

/// One record with everything the row and the matcher need worked out —
/// the jq `map(. + {…})` chain, kept as fields rather than a Value.
struct Prep {
    scope: String,
    action: String,
    family: String,
    rank: i64,
    keys: Vec<String>,
    combo: String,
    config: String,
    combo_key: String,
    hay: String,
    words: Vec<String>,
    sort_key: String,
}

fn prepare(fields: &[String]) -> Prep {
    let scope = fields[0].clone();
    // Canonical order, always. The two sources disagree — Omarchy prints
    // "SHIFT CTRL" and the handler here was written "CTRL SHIFT" — and
    // untouched that is two families holding one key each instead of one
    // family holding two.
    let mut raw_mods: Vec<String> = if fields[1].is_empty() {
        Vec::new()
    } else {
        fields[1].split(' ').map(String::from).collect()
    };
    raw_mods.sort_by_key(|m| modweight(m));
    let raw_key = fields[2].clone();
    let action = fields[3].clone();

    let mod_names: Vec<String> = raw_mods.iter().map(|m| modname(m)).collect();
    let key_name = keydisplay(&raw_key);
    let has_super = raw_mods.iter().any(|m| m == "SUPER");

    // The family is what the view groups by, and it is the modifiers and
    // nothing else: Super plus a letter is one keyboard, Super Shift plus a
    // letter is another, and telling them apart is the whole reason to draw
    // this rather than list it.
    let family = if mod_names.is_empty() {
        "Unmodified".to_string()
    } else {
        mod_names.join(" ")
    };
    // Super first, then fewest modifiers first, then a stable tie-break.
    // Super is where nearly everything lives, so it goes at the top and the
    // bare media keys go at the bottom.
    let rank = if has_super { 0 } else { 1000 }
        + 100 * raw_mods.len() as i64
        + raw_mods.iter().map(|m| modweight(m)).sum::<i64>()
        + if raw_mods.is_empty() { 4000 } else { 0 };

    // Every chip on the row, modifiers then key. The view draws the last one
    // differently, because inside a family the key is the only thing that
    // varies and it is the thing being looked for.
    let mut keys = mod_names.clone();
    keys.push(key_name.clone());
    let combo = keys.join(" + ");
    // What hyprland.lua wants. Its own parser splits on "+" and matches each
    // piece against a modifier table, so "SUPER SHIFT + K" would be read as
    // a key called "SUPER SHIFT"; every piece gets its own plus.
    let mut config = raw_mods.clone();
    config.push(raw_key.clone());
    let config = config.join(" + ");

    // Searching matches the key or the action, both, because half the
    // questions are "what is on Super Shift B" and the other half are "what
    // takes a screenshot" and neither half knows which half it is in.
    let hay = [
        combo.clone(),
        config.clone(),
        action.clone(),
        scope.clone(),
        raw_key.clone(),
        keyaliases(&raw_key).to_string(),
        if has_super { "win meta cmd mod" } else { "" }.to_string(),
    ]
    .join(" ")
    .to_ascii_lowercase();
    // Single characters before names, so a family reads 1..9 then A..Z then
    // the long keys, which is the order a keyboard is in.
    let sort_key = format!(
        "{}{}",
        if key_name.chars().count() == 1 {
            "0"
        } else {
            "1"
        },
        key_name.to_ascii_uppercase()
    );
    let words = hay
        .split(' ')
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect();

    Prep {
        scope,
        action,
        family,
        rank,
        keys,
        combo,
        config,
        combo_key: combo_key(&raw_mods, &raw_key),
        hay,
        words,
        sort_key,
    }
}

/// The whole jq pipeline, pure: tab-split records in (system first, then the
/// launcher's own), the row objects a `shortcuts:` query would emit out.
fn rows(records: &[Vec<String>], query: &str) -> Vec<Value> {
    // The combinations a system bind already owns. The launcher's keys are
    // listed whether or not anything else claims them, and when something
    // does the key may never reach the launcher at all — which is the one
    // thing about a keymap a reader most needs to know, and the one thing
    // neither source says.
    let taken: HashSet<String> = records
        .iter()
        .filter(|f| f.len() >= 3 && f[0] != "Oxy")
        .map(|f| {
            combo_key(
                &f[1].split(' ').map(String::from).collect::<Vec<_>>(),
                &f[2],
            )
        })
        .collect();

    // The query's terms: "win+k" is "win" and "k" — the plus is punctuation,
    // not a word.
    let terms: Vec<String> = query
        .to_ascii_lowercase()
        .replace('+', " ")
        .split(' ')
        .filter(|t| !t.is_empty())
        .map(String::from)
        .collect();

    let mut all: Vec<Prep> = records
        .iter()
        .filter(|f| f.len() >= 4)
        .map(|f| prepare(f))
        // A term matches a whole word from its start, or, once it is long
        // enough to mean something on its own, anywhere at all. Plain
        // substring matching on the whole row was useless at one character:
        // `shortcuts:win k` returned sixty-eight rows, because "k" is in
        // "workspace". Prefix-only matching was wrong in the other
        // direction, because "shot" is how people look for Screenshot.
        .filter(|r| {
            terms.iter().all(|t| {
                r.words.iter().any(|w| w.starts_with(t.as_str()))
                    || (t.chars().count() >= 3 && r.hay.contains(t.as_str()))
            })
        })
        .collect();

    all.sort_by(|a, b| {
        a.rank
            .cmp(&b.rank)
            .then_with(|| a.sort_key.cmp(&b.sort_key))
            .then_with(|| a.action.cmp(&b.action))
    });

    all.iter()
        .enumerate()
        .map(|(i, r)| {
            let copy_combo = format!("printf %s {} | wl-copy", shq(&r.config));
            let copy_action = format!("printf %s {} | wl-copy", shq(&r.action));
            let copy_line = format!(
                "printf %s {} | wl-copy",
                shq(&format!("{}  →  {}", r.combo, r.action))
            );
            // Nothing on a system row: a chip saying "System" on two hundred
            // rows out of two hundred and seventeen is furniture. The
            // seventeen that are not the system say so — and say when the
            // system owns the same combination, because then the launcher
            // may never see the key.
            let accessory = if r.scope == "Oxy" {
                if taken.contains(&r.combo_key) {
                    "Oxy · also bound"
                } else {
                    "Oxy"
                }
            } else {
                ""
            };
            json!({
                "id": format!("{}|{}|{}", r.scope, r.config, r.action),
                "title": r.action,
                // The combination in words, so the row still reads correctly
                // if the shortcuts view is not registered and these fall
                // back to a list.
                "subtitle": r.combo,
                "group": r.family,
                "keys": r.keys,
                "combo": r.combo,
                "config": r.config,
                "scope": r.scope,
                "accessory": accessory,
                "view": "shortcuts",
                "exec": copy_combo,
                "score": 99999 - i as i64,
                "actions": [
                    { "title": "Copy Combination", "shortcut": "↵", "exec": copy_combo },
                    { "title": "Copy Action", "exec": copy_action },
                    { "title": "Copy Line", "exec": copy_line },
                ],
            })
        })
        .collect()
}

/// `omarchy_binds` — Some only when the menu is installed *and* answered,
/// so "the menu is installed but answered nothing" falls through to
/// `hyprctl` instead of being a silent empty keymap.
async fn omarchy_records() -> Option<Vec<Vec<String>>> {
    if !on_path("omarchy-menu-keybindings") {
        return None;
    }
    let fin = process::run("omarchy-menu-keybindings --print", CALL).await?;
    let recs = parse_menu(&fin.stdout);
    if recs.is_empty() { None } else { Some(recs) }
}

/// `hyprctl_binds` — the raw report, Some only when it produced rows.
async fn hyprctl_records() -> Option<Vec<Vec<String>>> {
    let fin = process::run("hyprctl binds -j", CALL).await?;
    let recs = parse_binds(&fin.stdout);
    if recs.is_empty() { None } else { Some(recs) }
}

impl NativeExt for Shortcuts {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        let arg = ctx.arg.clone();
        Box::pin(async move {
            // The manifest's `when`: `command -v hyprctl`. A native provider
            // is asked even when the gate fails, so it is re-checked before
            // anything spawns.
            if !on_path("hyprctl") {
                return NativeOutcome::Fallback;
            }
            // `omarchy_binds || hyprctl_binds`, then the launcher's own keys
            // — listed whether or not anything else claims them. A hyprctl
            // that cannot answer still leaves the Oxy rows, which is what
            // the script prints too.
            let mut records = match omarchy_records().await {
                Some(recs) => recs,
                None => hyprctl_records().await.unwrap_or_default(),
            };
            records.extend(OXY_BINDS.iter().map(|(m, k, a)| record("Oxy", m, k, a)));
            NativeOutcome::Rows(rows(&records, &arg))
        })
    }
}
