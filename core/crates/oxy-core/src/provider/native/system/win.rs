//! `win — the open windows list`. A port of `bin/oxy-search-windows`.
//!
//! The answer is a picture of the session, not a list of strings: every row
//! carries which workspace the window is on, whether that workspace is the
//! one a monitor is showing, how many windows it holds in total, and the
//! handful of facts that tell two same-class windows apart. Two `hyprctl`
//! calls say all of it — `clients -j` for the windows and `monitors -j` for
//! the workspace each screen is showing (a second monitor has an active
//! workspace of its own and nothing focused on it).
//!
//! The traps the script's comments record, kept:
//!
//!   * Unmapped windows are dropped — they cannot be focused — but
//!     special-workspace windows are kept: a scratchpad terminal is exactly
//!     the window somebody types `win:` to find.
//!   * Order is workspace ascending, then where the window sits on screen
//!     (left→right, top→bottom), floating after the tiled windows they sit
//!     over — never focus history, because the top row is the one Enter
//!     runs.
//!   * Focus and close go through the Lua dispatcher: under a Lua config
//!     `dispatch focuswindow` returns `ok` and does nothing (§6.6), so the
//!     execs are the `hl.dsp.*` forms the script emits.
//!   * The query matches class, title and workspace name, so `win:general`
//!     is a workspace filter without a second syntax.
//!   * Silence stays silent: no windows, or a hyprctl that cannot answer,
//!     is an empty list — never an error row.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::native::util::on_path;
use crate::provider::process;
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::support::quote::quote;

/// One `hyprctl` call's deadline: the script had none, but a wedged
/// compositor is silence either way — this is the difference between
/// "no answer" and "the launcher hung".
const HYPRCTL_TIMEOUT: Duration = Duration::from_secs(2);

pub struct Win;

/// One `hyprctl clients -j` entry, kept the way the jq pipeline shaped it —
/// including the intermediates (`ws_raw`, `focus_order`, `x`/`y`) that feed
/// matching and ordering but never reach the wire.
struct Window {
    address: String,
    cls: String,
    name: String,
    ws_id: i64,
    ws_raw: String,
    monitor: String,
    floating: bool,
    fullscreen: i64,
    xwayland: bool,
    pinned: bool,
    grouped: i64,
    focus_order: i64,
    w: i64,
    h: i64,
    x: i64,
    y: i64,
}

/// jq's `a // b` on the left-hand side only: `null` and `false` both fall
/// through to the default.
fn some(v: Option<&Value>) -> Option<&Value> {
    v.filter(|v| !matches!(v, Value::Null | Value::Bool(false)))
}

/// `.field // ""` read as a string.
fn str_or(v: Option<&Value>) -> &str {
    some(v).and_then(|v| v.as_str()).unwrap_or("")
}

/// `.field // n` read as an integer, with a truncating read of a float so a
/// fractional `at`/`size` still orders the way jq would have sorted it.
fn int_or(v: Option<&Value>, n: i64) -> i64 {
    some(v)
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
        .unwrap_or(n)
}

/// `.field == true`.
fn yes(v: Option<&Value>) -> bool {
    v.and_then(|v| v.as_bool()).unwrap_or(false)
}

/// jq's `tostring`: strings pass through, everything else renders as JSON.
fn tostring(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The workspace name as it should read on screen: the `special:` plumbing
/// stripped, and an unnamed workspace read as its number.
fn ws_name_of(w: &Window) -> String {
    if let Some(rest) = w.ws_raw.strip_prefix("special:") {
        rest.to_string()
    } else if w.ws_raw.is_empty() {
        w.ws_id.to_string()
    } else {
        w.ws_raw.clone()
    }
}

/// The whole jq pipeline, pure: canned `hyprctl` JSON in, the rows a `win:`
/// query would emit out — no compositor needed to test it.
fn rows(clients: &Value, monitors: &Value, query: &str) -> Vec<Value> {
    // `map()`/`[]` on anything that is not an array fails jq — and the whole
    // answer with it, which is exactly the silence the script would print.
    let Some(monitors) = monitors.as_array() else {
        return Vec::new();
    };
    let Some(clients) = clients.as_array() else {
        return Vec::new();
    };

    // Which workspace is live on each monitor, and what the monitors are
    // called. A second screen has an active workspace of its own and nothing
    // focused on it — the focused window alone could not answer this.
    let active: HashSet<i64> = monitors
        .iter()
        .filter_map(|m| {
            some(m.get("activeWorkspace"))
                .and_then(|w| some(w.get("id")))
                .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
        })
        .collect();
    let mon_names: HashMap<String, String> = monitors
        .iter()
        .map(|m| {
            (
                tostring(m.get("id").unwrap_or(&Value::Null)),
                str_or(m.get("name")).to_string(),
            )
        })
        .collect();

    let mut all: Vec<Window> = Vec::new();
    for c in clients {
        // `select(.mapped == true)`: an unmapped window cannot be focused,
        // so it is not an offer. Special workspaces stay — see the header.
        if !yes(c.get("mapped")) {
            continue;
        }
        // `.address` is the one field the script reads without `// ""`:
        // `"address:" + null` is a jq error, which ends jq — and so the
        // answer — right there.
        let Some(address) = c.get("address").and_then(|v| v.as_str()) else {
            return Vec::new();
        };
        let ws = c.get("workspace");
        all.push(Window {
            address: address.to_string(),
            cls: str_or(c.get("class")).to_string(),
            name: str_or(c.get("title")).to_string(),
            ws_id: int_or(ws.and_then(|w| w.get("id")), 0),
            ws_raw: str_or(ws.and_then(|w| w.get("name"))).to_string(),
            monitor: some(c.get("monitor"))
                .map(tostring)
                .unwrap_or_else(|| "-1".into()),
            floating: yes(c.get("floating")),
            // 0 none, 1 maximised inside the gaps, 2 covering the monitor —
            // different enough to be worth telling apart on the row.
            fullscreen: int_or(c.get("fullscreen"), 0),
            xwayland: yes(c.get("xwayland")),
            pinned: yes(c.get("pinned")),
            // A tab group draws as one window and answers as several; the
            // count is the only warning that focusing this row changes what
            // is visible.
            grouped: some(c.get("grouped"))
                .and_then(|v| v.as_array())
                .map(|a| a.len() as i64)
                .unwrap_or(0),
            // focusHistoryID 0 is the window that has focus right now. It is
            // not used for ordering, only for saying which one it is.
            focus_order: int_or(c.get("focusHistoryID"), 999),
            w: int_or(c.get("size").and_then(|s| s.get(0)), 0),
            h: int_or(c.get("size").and_then(|s| s.get(1)), 0),
            x: int_or(c.get("at").and_then(|s| s.get(0)), 0),
            y: int_or(c.get("at").and_then(|s| s.get(1)), 0),
        });
    }

    // Counted before the query narrows anything, so a workspace header can
    // say "2 of 7" rather than claiming the two that matched are all there
    // is.
    let mut ws_count: HashMap<i64, i64> = HashMap::new();
    let mut ws_ids: HashSet<i64> = HashSet::new();
    for w in &all {
        *ws_count.entry(w.ws_id).or_default() += 1;
        ws_ids.insert(w.ws_id);
    }
    let total = all.len() as i64;
    let ws_total = ws_ids.len() as i64;
    let mon_total = monitors.len() as i64;

    // The workspace name joins the haystack, so `win:general` is a workspace
    // filter without needing a second filter syntax to learn. It costs the
    // odd false hit on a window whose title contains a workspace name, and
    // the grouping makes that obvious rather than confusing.
    let q = query.trim().to_lowercase();
    let mut matched: Vec<Window> = all
        .into_iter()
        .filter(|w| {
            q.is_empty()
                || format!("{} {} {}", w.cls, w.name, w.ws_raw)
                    .to_ascii_lowercase()
                    .contains(&q)
        })
        .collect();

    // Workspace by workspace, ascending, and within one by where the window
    // sits on screen: left to right, top to bottom, floating after the tiled
    // windows they sit over. Ordering by focus history instead would put the
    // window you are already in at the top, and the top row is the one Enter
    // runs. Special workspaces come last, after every numbered one.
    matched.sort_by_key(|w| {
        (
            i64::from(w.ws_id < 0),
            w.ws_id,
            i64::from(w.floating),
            w.x,
            w.y,
        )
    });

    let matched_total = matched.len() as i64;
    let mut out = Vec::with_capacity(matched.len());
    for (i, w) in matched.iter().enumerate() {
        let special = w.ws_id < 0;
        let ws_name = ws_name_of(w);
        let target = format!("address:{}", w.address);
        // The Lua dispatcher forms, quoted the way jq's @sh quoted them.
        // `hyprctl dispatch focuswindow` returns `ok` and does nothing under
        // a Lua config — `hl.dsp.*` is the call that works (§6.6).
        let focus = format!(
            "hyprctl dispatch {}",
            quote(&format!("hl.dsp.focus({{ window = \"{target}\" }})"))
        );
        let close = format!(
            "hyprctl dispatch {}",
            quote(&format!("hl.dsp.window.close({{ window = \"{target}\" }})"))
        );
        let goto = format!(
            "hyprctl dispatch {}",
            quote(&format!("hl.dsp.focus({{ workspace = {} }})", w.ws_id))
        );

        let mut actions = vec![json!({ "title": "Focus", "shortcut": "↵", "exec": focus })];
        // A special workspace has a negative id the dispatcher will not
        // take, and "go to the scratchpad" is a toggle rather than a
        // destination.
        if !special {
            actions.push(json!({ "title": "Go to Workspace", "exec": goto }));
        }
        actions.push(json!({ "title": "Close Window", "exec": close }));

        out.push(json!({
            "id": w.address,
            // Set on every row rather than the first, because ranking can
            // put any row first and only the first row has its view read.
            "view": "windows",
            "title": if w.name.is_empty() { &w.cls } else { &w.name },
            // What the plain list would show, kept working: an unregistered
            // view falls back to the list, and a half-registered feature
            // should still answer the question.
            "subtitle": format!("{}  ·  {}", w.cls, ws_name),
            "exec": focus,
            "score": 90000 - i as i64 * 300,

            "cls": w.cls,
            "wsId": w.ws_id,
            "wsName": ws_name,
            "wsActive": active.contains(&w.ws_id),
            "wsWindows": ws_count.get(&w.ws_id).copied().unwrap_or(0),
            "special": special,
            "monitor": mon_names.get(&w.monitor).cloned().unwrap_or_default(),
            "focused": w.focus_order == 0,
            "floating": w.floating,
            "fullscreen": w.fullscreen,
            "xwayland": w.xwayland,
            "pinned": w.pinned,
            "grouped": w.grouped,
            "width": w.w,
            "height": w.h,
            "session": {
                "windows": total,
                "matched": matched_total,
                "workspaces": ws_total,
                "monitors": mon_total,
            },
            "actions": actions,
        }));
    }
    out
}

impl NativeExt for Win {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        let arg = ctx.arg.clone();
        Box::pin(async move {
            // The manifest carries no `when`, but a box without hyprctl is
            // not running Hyprland — decline so the script's own silence is
            // what shows rather than answering in its place.
            if !on_path("hyprctl") {
                return NativeOutcome::Fallback;
            }
            // `[[ -n $clients ]] || exit 0`: no compositor, no answer. A
            // timeout or a spawn failure is the same silence — the script
            // cannot run where bash is absent either.
            let clients = match process::run("hyprctl clients -j", HYPRCTL_TIMEOUT).await {
                Some(fin) if !fin.stdout.trim().is_empty() => fin.stdout,
                _ => return NativeOutcome::Empty,
            };
            let Ok(clients) = serde_json::from_str::<Value>(clients.trim()) else {
                return NativeOutcome::Empty;
            };
            // `[[ -n $monitors ]] || monitors="[]"`: a missing monitors
            // answer is an empty list, not a failure. An unparseable one
            // fails jq outright, so it fails the answer here too.
            let text = process::run("hyprctl monitors -j", HYPRCTL_TIMEOUT)
                .await
                .map(|fin| fin.stdout)
                .unwrap_or_default();
            let monitors = if text.trim().is_empty() {
                Value::Array(Vec::new())
            } else {
                match serde_json::from_str::<Value>(text.trim()) {
                    Ok(v) => v,
                    Err(_) => return NativeOutcome::Empty,
                }
            };
            NativeOutcome::Rows(rows(&clients, &monitors, &arg))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Two monitors on different workspaces, the shape `monitors -j` has.
    fn monitors() -> Value {
        json!([
            { "id": 0, "name": "DP-1", "activeWorkspace": { "id": 1, "name": "1" } },
            { "id": 1, "name": "HDMI-A-1", "activeWorkspace": { "id": 2, "name": "2" } }
        ])
    }

    /// Six windows: two tiled on workspace 1 (ordered by x), a fullscreen
    /// tiled window and a floating one on workspace 2, a tiled window on
    /// workspace 3 (no monitor is showing it), one on a special workspace,
    /// and one unmapped. `0xbbb` has focus and an empty title.
    fn clients() -> Value {
        json!([
            { "address": "0xccc", "class": "mpv", "title": "video.mkv",
              "mapped": true, "monitor": 0,
              "workspace": { "id": 2, "name": "2" },
              "floating": true, "fullscreen": 0, "xwayland": true,
              "pinned": false, "grouped": [],
              "focusHistoryID": 2, "size": [640, 360], "at": [0, 0] },
            { "address": "0xaaa", "class": "foot", "title": "~/projects/example",
              "mapped": true, "monitor": 0,
              "workspace": { "id": 1, "name": "1" },
              "floating": false, "fullscreen": 0, "xwayland": false,
              "pinned": false, "grouped": [],
              "focusHistoryID": 1, "size": [960, 1040], "at": [0, 40] },
            { "address": "0xddd", "class": "foot", "title": "scratch",
              "mapped": true, "monitor": 1,
              "workspace": { "id": -99, "name": "special:magic" },
              "floating": true, "fullscreen": 0, "xwayland": false,
              "pinned": false, "grouped": [],
              "focusHistoryID": 4, "size": [900, 500], "at": [100, 100] },
            { "address": "0xeee", "class": "ghost", "title": "unmapped",
              "mapped": false, "monitor": 0,
              "workspace": { "id": 1, "name": "1" },
              "floating": false, "fullscreen": 0, "xwayland": false,
              "pinned": false, "grouped": [],
              "focusHistoryID": 5, "size": [1, 1], "at": [0, 0] },
            { "address": "0xbbb", "class": "firefox", "title": "",
              "mapped": true, "monitor": 0,
              "workspace": { "id": 2, "name": "2" },
              "floating": false, "fullscreen": 2, "xwayland": false,
              "pinned": false, "grouped": ["0xbbb", "0xccc"],
              "focusHistoryID": 0, "size": [1920, 1080], "at": [0, 0] },
            { "address": "0xaab", "class": "foot", "title": "second term",
              "mapped": true, "monitor": 0,
              "workspace": { "id": 1, "name": "1" },
              "floating": false, "fullscreen": 0, "xwayland": false,
              "pinned": false, "grouped": [],
              "focusHistoryID": 3, "size": [960, 1040], "at": [960, 40] },
            { "address": "0xffe", "class": "gimp", "title": "image.xcf",
              "mapped": true, "monitor": 0,
              "workspace": { "id": 3, "name": "3" },
              "floating": false, "fullscreen": 0, "xwayland": false,
              "pinned": false, "grouped": [],
              "focusHistoryID": 6, "size": [1920, 1080], "at": [0, 0] }
        ])
    }

    fn ids(rows: &[Value]) -> Vec<&str> {
        rows.iter().map(|r| r["id"].as_str().unwrap()).collect()
    }

    #[test]
    fn every_mapped_window_in_place_order() {
        // Input order is deliberately scrambled: the answer is sorted
        // workspace ascending, then x, floating last in its workspace,
        // special workspaces after every numbered one. The unmapped window
        // is not an offer.
        let rows = rows(&clients(), &monitors(), "");
        assert_eq!(
            ids(&rows),
            ["0xaaa", "0xaab", "0xbbb", "0xccc", "0xffe", "0xddd"]
        );
    }

    #[test]
    fn an_empty_title_falls_back_to_the_class() {
        let rows = rows(&clients(), &monitors(), "");
        let bbb = rows.iter().find(|r| r["id"] == "0xbbb").unwrap();
        assert_eq!(bbb["title"], "firefox");
        let aaa = rows.iter().find(|r| r["id"] == "0xaaa").unwrap();
        assert_eq!(aaa["title"], "~/projects/example");
    }

    #[test]
    fn subtitle_joins_class_and_workspace() {
        let rows = rows(&clients(), &monitors(), "");
        assert_eq!(rows[0]["subtitle"], "foot  ·  1");
        assert_eq!(rows[2]["subtitle"], "firefox  ·  2");
    }

    #[test]
    fn a_special_workspace_is_kept_named_and_last() {
        let rows = rows(&clients(), &monitors(), "");
        let ddd = rows.last().unwrap();
        assert_eq!(ddd["id"], "0xddd");
        assert_eq!(ddd["special"], true);
        // The `special:` plumbing is stripped for display.
        assert_eq!(ddd["wsName"], "magic");
        assert_eq!(ddd["wsId"], -99);
        // No monitor is showing it, so it is not the active workspace.
        assert_eq!(ddd["wsActive"], false);
        assert_eq!(ddd["monitor"], "HDMI-A-1");
    }

    #[test]
    fn the_active_workspace_comes_from_the_monitors() {
        // Both screens' workspaces read active — ws 1 on DP-1 and ws 2 on
        // HDMI-A-1 — while ws 3 is up on neither.
        let rows = rows(&clients(), &monitors(), "");
        assert_eq!(rows[0]["wsActive"], true); // ws 1
        assert_eq!(rows[2]["wsActive"], true); // ws 2
        assert_eq!(rows[4]["wsActive"], false); // ws 3
    }

    #[test]
    fn focus_history_zero_marks_the_focused_window() {
        let rows = rows(&clients(), &monitors(), "");
        assert_eq!(rows[2]["id"], "0xbbb");
        assert_eq!(rows[2]["focused"], true);
        for (i, r) in rows.iter().enumerate() {
            assert_eq!(r["focused"], i == 2, "row {i}");
        }
    }

    #[test]
    fn the_session_counts_ride_every_row() {
        // Six mapped windows on four workspaces over two monitors; the
        // unmapped one never counted.
        let rows = rows(&clients(), &monitors(), "");
        assert_eq!(rows.len(), 6);
        for r in &rows {
            assert_eq!(
                r["session"],
                json!({ "windows": 6, "matched": 6, "workspaces": 4, "monitors": 2 })
            );
        }
    }

    #[test]
    fn ws_windows_counts_the_workspace_not_the_answer() {
        // "2 of 7": the header's total is counted before the query narrows.
        let rows = rows(&clients(), &monitors(), "firefox");
        assert_eq!(ids(&rows), ["0xbbb"]);
        assert_eq!(rows[0]["wsWindows"], 2);
        assert_eq!(rows[0]["session"]["windows"], 6);
        assert_eq!(rows[0]["session"]["matched"], 1);
    }

    #[test]
    fn actions_are_focus_goto_and_close() {
        let rows = rows(&clients(), &monitors(), "");
        let actions = rows[0]["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0]["title"], "Focus");
        assert_eq!(actions[0]["shortcut"], "↵");
        assert_eq!(actions[1]["title"], "Go to Workspace");
        assert_eq!(actions[2]["title"], "Close Window");

        // The Lua dispatcher forms — `dispatch focuswindow` returns ok and
        // does nothing under a Lua config.
        assert_eq!(
            rows[0]["exec"],
            "hyprctl dispatch 'hl.dsp.focus({ window = \"address:0xaaa\" })'"
        );
        assert_eq!(
            actions[0]["exec"],
            "hyprctl dispatch 'hl.dsp.focus({ window = \"address:0xaaa\" })'"
        );
        assert_eq!(
            actions[1]["exec"],
            "hyprctl dispatch 'hl.dsp.focus({ workspace = 1 })'"
        );
        assert_eq!(
            actions[2]["exec"],
            "hyprctl dispatch 'hl.dsp.window.close({ window = \"address:0xaaa\" })'"
        );
    }

    #[test]
    fn a_special_workspace_row_drops_go_to_workspace() {
        // A negative workspace id is one the dispatcher will not take, and
        // "go to the scratchpad" is a toggle rather than a destination.
        let rows = rows(&clients(), &monitors(), "");
        let actions = rows[5]["actions"].as_array().unwrap();
        assert_eq!(rows[5]["id"], "0xddd");
        assert_eq!(
            actions
                .iter()
                .map(|a| a["title"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["Focus", "Close Window"]
        );
    }

    #[test]
    fn the_query_matches_class_title_and_workspace_name() {
        let cases: &[(&str, &[&str])] = &[
            ("", &["0xaaa", "0xaab", "0xbbb", "0xccc", "0xffe", "0xddd"]),
            // by class
            ("firefox", &["0xbbb"]),
            ("foot", &["0xaaa", "0xaab", "0xddd"]),
            // by title
            ("video", &["0xccc"]),
            ("scratch", &["0xddd"]),
            // by workspace name — `win:general` is a workspace filter
            // without a second syntax, and `special:magic` answers to
            // either half of itself.
            ("magic", &["0xddd"]),
            ("special", &["0xddd"]),
            ("3", &["0xffe"]),
            // the haystack is lowered with ASCII case rules; the query is
            // lowered whole.
            ("FIREFOX", &["0xbbb"]),
            ("zzz", &[]),
        ];
        for (q, want) in cases {
            assert_eq!(ids(&rows(&clients(), &monitors(), q)), *want, "query {q:?}");
        }
    }

    #[test]
    fn scores_step_down_in_emission_order() {
        let rows = rows(&clients(), &monitors(), "");
        let scores: Vec<i64> = rows.iter().map(|r| r["score"].as_i64().unwrap()).collect();
        assert_eq!(scores, [90000, 89700, 89400, 89100, 88800, 88500]);
    }

    #[test]
    fn no_monitors_still_answers() {
        // `monitors="[]"`: no names, nothing active, zero counted.
        let rows = rows(&clients(), &json!([]), "");
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[0]["monitor"], "");
        assert_eq!(rows[0]["wsActive"], false);
        assert_eq!(rows[0]["session"]["monitors"], 0);
    }

    #[test]
    fn the_wire_fields_are_exactly_the_scripts() {
        // The jq intermediates — wsRaw, focusOrder, address, w/h/x/y, name —
        // never reached the wire, and the row must not grow them back.
        let rows = rows(&clients(), &monitors(), "");
        let obj = rows[0].as_object().unwrap();
        let want: HashSet<&str> = [
            "id",
            "view",
            "title",
            "subtitle",
            "exec",
            "score",
            "cls",
            "wsId",
            "wsName",
            "wsActive",
            "wsWindows",
            "special",
            "monitor",
            "focused",
            "floating",
            "fullscreen",
            "xwayland",
            "pinned",
            "grouped",
            "width",
            "height",
            "session",
            "actions",
        ]
        .into_iter()
        .collect();
        let got: HashSet<&str> = obj.keys().map(|k| k.as_str()).collect();
        assert_eq!(got, want);
    }
}
