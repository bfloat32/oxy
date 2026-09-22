//! The pure half of `herdr:` — the script's jq program taken apart into
//! functions a test can drive over canned CLI output. `running_sessions` is
//! the awk over `herdr session list`, `classify` is the `*"snapshot"*` gate
//! plus the `fromjson`, and `build` is the whole pipeline from `$sessions`
//! to the emitted rows: state rebuild, bands, the needle filter, the sort.
//!
//! `None` is this port's spelling of a jq error. Where the script's jq would
//! have died mid-pipe — a `fromjson` failure, an index into a scalar, a
//! `join` over a non-string — these functions give up on the whole answer,
//! which is what the script produced when that happened: silence.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};

use crate::support::quote::quote;

/// `awk '$1 != "name" && $2 == "running" { print $1 }'` — the names of the
/// sessions whose server is alive. Only these may be named on an `api`
/// call: asking a stopped one starts a server for it, and a launcher must
/// never start something nobody asked for.
pub fn running_sessions(list: &str) -> Vec<String> {
    list.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            if name != "name" && fields.next() == Some("running") {
                Some(name.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// What one `api snapshot` call's stdout meant. `Skip` is a dead socket or
/// an error payload — a session listed as `running` can still answer
/// `{"error":...}` and is skipped rather than counted. `Poison` is stdout
/// that looked like it carried a snapshot but jq could not parse or index —
/// in the script that killed jq outright, and the whole answer with it.
pub enum Snapshot {
    Skip,
    Poison,
    Shot(Value),
}

/// `[[ $snap == *'"snapshot"'* ]]` then `fromjson | .result.snapshot` —
/// including jq's index errors: a parsed answer that is not an object, or a
/// `result` that is not one, is `Cannot index` — `Poison`, not `Skip`.
pub fn classify(stdout: &str) -> Snapshot {
    if !stdout.contains("\"snapshot\"") {
        return Snapshot::Skip;
    }
    let Ok(v) = serde_json::from_str::<Value>(stdout.trim()) else {
        return Snapshot::Poison;
    };
    let Some(top) = v.as_object() else {
        return Snapshot::Poison;
    };
    match top.get("result") {
        None | Some(Value::Null) => Snapshot::Skip,
        Some(Value::Object(result)) => match result.get("snapshot") {
            None | Some(Value::Null) => Snapshot::Skip,
            Some(snap) => Snapshot::Shot(snap.clone()),
        },
        Some(_) => Snapshot::Poison,
    }
}

/// `. != null and . != ""` inverted — the two values `pick` skips.
fn blank(v: &Value) -> bool {
    matches!(v, Value::Null) || matches!(v, Value::String(s) if s.is_empty())
}

/// jq's `pick`: the first element that is neither null nor `""`. A `false`
/// survives the select but collapses under `first // ""` — to `""`.
fn pick(opts: &[Option<&Value>]) -> Value {
    match opts.iter().copied().flatten().find(|v| !blank(v)) {
        Some(&Value::Bool(false)) | None => Value::String(String::new()),
        Some(v) => v.clone(),
    }
}

/// `a // b`: jq's alternative — null and false both fall through.
fn jq_or(v: Option<&Value>, default: Value) -> Value {
    match v {
        Some(Value::Null) | Some(Value::Bool(false)) | None => default,
        Some(v) => v.clone(),
    }
}

/// `def band` — waiting on you, then finished behind your back, then
/// running, then seen and idle, then present but unclassified. The sort
/// order and the row order; the view spends it on colour and size.
fn band(status: &str) -> i64 {
    match status {
        "blocked" => 0,
        "done" => 1,
        "working" => 2,
        "idle" => 3,
        _ => 4,
    }
}

/// jq's `tostring`: strings pass through, everything else renders as JSON.
fn tostring(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `def tilde`: `~` for a path under $HOME. The `startswith` errors on a
/// non-string input — jq dying — so a non-string path is `None`.
fn tilde(v: &Value, home: &str) -> Option<Value> {
    // The guard short-circuits before `startswith` runs: an empty path or an
    // empty $HOME means the input is echoed untouched, whatever its type.
    if matches!(v, Value::String(s) if s.is_empty()) || home.is_empty() {
        return Some(v.clone());
    }
    match v {
        Value::String(s) => Some(Value::String(
            s.strip_prefix(home)
                .map(|rest| format!("~{rest}"))
                .unwrap_or_else(|| s.clone()),
        )),
        _ => None,
    }
}

/// `session + "|" + field` — jq's `+` treats null as the identity and
/// errors on every other non-string, so a missing id is a `session|` key
/// and a number is `None`.
fn key3(session: &str, field: Option<&Value>) -> Option<String> {
    match field {
        None | Some(Value::Null) => Some(format!("{session}|")),
        Some(Value::String(s)) => Some(format!("{session}|{s}")),
        Some(_) => None,
    }
}

/// `.snap.<key>[]?` — the `?` swallows `.key` on a non-object too, so a
/// malformed `snapshot` iterates empty rather than failing the session that
/// holds it (and, through the caller, every sibling session with it).
/// Object values iterate their members, as jq does.
fn members<'a>(snap: &'a Value, key: &str) -> Option<Vec<&'a Value>> {
    let Some(snap) = snap.as_object() else {
        return Some(Vec::new());
    };
    match snap.get(key) {
        None | Some(Value::Null) => Some(Vec::new()),
        Some(Value::Array(list)) => Some(list.iter().collect()),
        Some(Value::Object(map)) => Some(map.values().collect()),
        // `scalar[]?` — the `?` swallows it; no members.
        Some(_) => Some(Vec::new()),
    }
}

/// `.field` read jq's way: null on a missing key or a null base, an error —
/// `None` — on a scalar or an array.
fn field<'a>(v: &'a Value, key: &str) -> Option<Option<&'a Value>> {
    match v {
        Value::Object(m) => Some(m.get(key)),
        Value::Null => Some(None),
        _ => None,
    }
}

/// `($state_labels // {})[status] // (to_entries | map(.value) | pick)` —
/// the current status as a key first, any label second. This is where the
/// actual question a blocked agent is asking arrives. A `state_labels`
/// that is not an object is the index error that killed jq.
fn note(labels: Option<&Value>, status: &str) -> Option<Value> {
    let obj = match labels {
        None | Some(Value::Null) | Some(Value::Bool(false)) => {
            return Some(Value::String(String::new()));
        }
        Some(Value::Object(m)) => m,
        Some(_) => return None,
    };
    if let Some(v) = obj
        .get(status)
        .filter(|v| !matches!(v, Value::Null | Value::Bool(false)))
    {
        return Some(v.clone());
    }
    Some(pick(
        &obj.values().map(Some).collect::<Vec<Option<&Value>>>(),
    ))
}

/// `$state` — the seen file rebuilt from the agents that exist now, which
/// is also what prunes it: a pane that closed is simply not written back.
/// Keyed on the status and the counter together, so a pane that went
/// blocked, was answered, and blocked again gets a new start time rather
/// than drawing the second wait as if it had lasted all morning.
fn rebuild_state(
    agents: &[Value],
    seen: &Map<String, Value>,
    now: i64,
) -> Option<Map<String, Value>> {
    let mut state = Map::new();
    for a in agents {
        let session = a.get("session").and_then(Value::as_str)?;
        let key = key3(session, a.get("pane_id"))?;
        // `($seen[$k] // null)` — a `false` entry collapses to null too.
        let prev = seen
            .get(&key)
            .filter(|v| !matches!(v, Value::Null | Value::Bool(false)));
        let seq = jq_or(a.get("state_change_seq"), json!(0));
        let status = a.get("agent_status").cloned().unwrap_or(Value::Null);
        let entry = match prev {
            // `$prev.st` on a scalar is the index error that killed jq.
            Some(p) if !p.is_object() => return None,
            Some(p)
                if p.get("st").cloned().unwrap_or(Value::Null) == status
                    && p.get("sq").cloned().unwrap_or(Value::Null) == seq =>
            {
                p.clone()
            }
            Some(_) => json!({ "st": status, "sq": seq, "at": now, "known": true }),
            None => json!({ "st": status, "sq": seq, "at": now, "known": false }),
        };
        state.insert(key, entry);
    }
    Some(state)
}

/// `if ($s.known == true) then ($now - $s.at) else -1` — a first sighting
/// has no start to subtract from, so it draws no number rather than "0m".
/// A `known` entry whose `at` is not a number is the subtraction error.
fn since(entry: &Value, now: i64) -> Option<Value> {
    if entry.get("known").and_then(Value::as_bool) == Some(true) {
        let at = entry.get("at").and_then(Value::as_f64)?;
        Some(number(now as f64 - at))
    } else {
        Some(json!(-1))
    }
}

/// A jq-shaped number: whole values stay ints so `120` never prints `120.0`.
fn number(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() <= 9e15 {
        json!(f as i64)
    } else {
        json!(f)
    }
}

/// jq's ordering across types — `name`/`wsLabel` are whatever `pick` found,
/// so the sort cannot assume strings: null < false < true < numbers <
/// strings < arrays < objects.
fn rank(v: &Value) -> u8 {
    match v {
        Value::Null => 0,
        Value::Bool(false) => 1,
        Value::Bool(true) => 2,
        Value::Number(_) => 3,
        Value::String(_) => 4,
        Value::Array(_) => 5,
        Value::Object(_) => 6,
    }
}

fn jq_cmp(a: &Value, b: &Value) -> Ordering {
    let (ra, rb) = (rank(a), rank(b));
    if ra != rb {
        return ra.cmp(&rb);
    }
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&y.as_f64().unwrap_or(0.0)),
        (Value::String(x), Value::String(y)) => x.cmp(y),
        _ => Ordering::Equal,
    }
}

/// `[name, kind, status, wsLabel, tabLabel, session, path, what] | join(" ")
/// | ascii_downcase` — jq's `join` maps null to `""` and dies on every
/// other non-string, so a scalar field where a string belongs is `None`.
fn haystack(r: &Map<String, Value>) -> Option<String> {
    let mut parts = Vec::with_capacity(8);
    for k in [
        "name", "kind", "status", "wsLabel", "tabLabel", "session", "path", "what",
    ] {
        match r.get(k) {
            None | Some(Value::Null) => parts.push(String::new()),
            Some(Value::String(s)) => parts.push(s.clone()),
            Some(_) => return None,
        }
    }
    Some(parts.join(" ").to_ascii_lowercase())
}

/// `sort_by([.band, (if .since < 0 then 0 else -.since end), .session,
/// .wsLabel, .name])` — longest wait first inside a band, unknown durations
/// last rather than first, which is what -1 would otherwise do.
fn sort_cmp(a: &Map<String, Value>, b: &Map<String, Value>) -> Ordering {
    let num = |m: &Map<String, Value>, k: &str| m.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let wait = |m: &Map<String, Value>| {
        let s = num(m, "since");
        if s < 0.0 { 0.0 } else { -s }
    };
    num(a, "band")
        .total_cmp(&num(b, "band"))
        .then_with(|| wait(a).total_cmp(&wait(b)))
        .then_with(|| jq_cmp(&a["session"], &b["session"]))
        .then_with(|| jq_cmp(&a["wsLabel"], &b["wsLabel"]))
        .then_with(|| jq_cmp(&a["name"], &b["name"]))
}

/// One agent's row object — the jq `{ session, band, status, name, kind,
/// note, what, path, paneId, wsLabel, tabLabel, tabCount, here, since }`
/// verbatim, field names included: the `herdr` view reads every one.
fn agent_row(
    a: &Value,
    ws: &HashMap<String, Value>,
    tabs: &HashMap<String, Value>,
    state: &Map<String, Value>,
    home: &str,
    now: i64,
) -> Option<Value> {
    let session = a.get("session")?.as_str()?.to_string();
    let status = a.get("agent_status").cloned().unwrap_or(Value::Null);
    // `(state_labels // {})[agent_status]` is computed for every row, and
    // indexing an object by a non-string is the error that killed jq — so
    // the status must be a string here, not merely usually one.
    let status_s = status.as_str()?;

    let empty = Map::new();
    let w = ws
        .get(&key3(&session, a.get("workspace_id"))?)
        .filter(|v| !matches!(v, Value::Null | Value::Bool(false)))
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let t = tabs
        .get(&key3(&session, a.get("tab_id"))?)
        .filter(|v| !matches!(v, Value::Null | Value::Bool(false)))
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let s = state.get(&key3(&session, a.get("pane_id"))?)?;

    let path = tilde(&pick(&[a.get("foreground_cwd"), a.get("cwd")]), home)?;
    // `name` is the handle the user gave this agent and the one they would
    // type; everything after it is a fallback so a row is never nameless.
    let label = pick(&[
        a.get("name"),
        a.get("title"),
        a.get("display_agent"),
        a.get("agent"),
    ]);
    let name = if label.as_str() == Some("") {
        json!("agent")
    } else {
        label
    };
    let kind = pick(&[a.get("display_agent"), a.get("agent")]);
    let note = note(a.get("state_labels"), status_s)?;
    let what = pick(&[a.get("terminal_title_stripped"), a.get("terminal_title")]);
    let ws_fallback = json!(format!("w{}", tostring(&jq_or(w.get("number"), json!(0)))));
    let tab_fallback = json!(tostring(&jq_or(t.get("number"), json!(0))));

    let mut r = Map::new();
    r.insert("session".into(), json!(session));
    r.insert("band".into(), json!(band(status_s)));
    r.insert("status".into(), status);
    r.insert("name".into(), name);
    r.insert("kind".into(), kind);
    r.insert("note".into(), note);
    r.insert("what".into(), what);
    r.insert("path".into(), path);
    r.insert(
        "paneId".into(),
        a.get("pane_id").cloned().unwrap_or(Value::Null),
    );
    r.insert(
        "wsLabel".into(),
        pick(&[w.get("label"), Some(&ws_fallback)]),
    );
    r.insert(
        "tabLabel".into(),
        pick(&[t.get("label"), Some(&tab_fallback)]),
    );
    r.insert("tabCount".into(), jq_or(w.get("tab_count"), json!(1)));
    r.insert(
        "here".into(),
        json!(a.get("focused").and_then(Value::as_bool) == Some(true)),
    );
    r.insert("since".into(), since(s, now)?);
    Some(Value::Object(r))
}

/// A workspace with nothing running in it is still somewhere you want to
/// go — `herdr:` moves between projects as well as between agents, and a
/// project you have not started work in yet would otherwise be unreachable.
fn idle_row(session: &str, w: &Value) -> Option<Value> {
    // `("w" + (.number | tostring))` — no `// 0` here, the script's own
    // quirk: an unnumbered workspace really does read `wnull`.
    let fallback = json!(format!(
        "w{}",
        tostring(field(w, "number")?.unwrap_or(&Value::Null))
    ));
    let label = pick(&[field(w, "label")?, Some(&fallback)]);
    let mut r = Map::new();
    r.insert("session".into(), json!(session));
    r.insert("band".into(), json!(5));
    r.insert("status".into(), json!(""));
    r.insert("name".into(), label.clone());
    r.insert("kind".into(), json!(""));
    r.insert("note".into(), json!(""));
    r.insert("what".into(), json!(""));
    r.insert("path".into(), json!(""));
    r.insert(
        "paneId".into(),
        field(w, "workspace_id")?.cloned().unwrap_or(Value::Null),
    );
    r.insert("wsLabel".into(), label);
    r.insert("tabLabel".into(), json!(""));
    r.insert("tabCount".into(), jq_or(field(w, "tab_count")?, json!(1)));
    r.insert(
        "here".into(),
        json!(field(w, "focused")?.and_then(Value::as_bool) == Some(true)),
    );
    r.insert("since".into(), json!(-1));
    Some(Value::Object(r))
}

/// `$r + { id, view, title, subtitle, detail, exec, score, sessions,
/// counts, actions }` — the emit block. The row's own fields ride along
/// untouched; the exec drives `oxy-herdr --focus` exactly as the script's
/// did, so Enter takes the same session-raising path.
fn emit(
    r: &Map<String, Value>,
    i: usize,
    counts: &Value,
    sessions: usize,
    matched: usize,
) -> Option<Value> {
    let session = r.get("session").and_then(Value::as_str).unwrap_or("");
    // `@sh` is computed before the id, but either failure ends jq — a paneId
    // that is not a string cannot be named on a command line at all.
    let pane = r.get("paneId").and_then(Value::as_str)?;
    let is_ws = r.get("band").and_then(Value::as_i64) == Some(5);
    let kind_arg = if is_ws { "workspace" } else { "agent" };
    let go = format!(
        "oxy-herdr --focus {} {} {}",
        quote(session),
        kind_arg,
        quote(pane)
    );

    let mut actions = vec![json!({
        "title": if is_ws { "Go To Workspace" } else { "Go To Agent" },
        "shortcut": "↵",
        "exec": go,
    })];
    if !is_ws {
        actions.push(json!({
            "title": "Copy Pane ID",
            "exec": format!("printf %s {} | wl-copy", quote(pane)),
        }));
    }

    let mut row = r.clone();
    row.insert("id".into(), json!(format!("herdr:{session}:{pane}")));
    row.insert("view".into(), json!("herdr"));
    // The view draws the band fields, but a row still needs a title and a
    // subtitle: the launcher reads them for recents, frecency and the
    // heading over the action panel.
    row.insert(
        "title".into(),
        r.get("name").cloned().unwrap_or(Value::Null),
    );
    row.insert(
        "subtitle".into(),
        if r.get("status") == Some(&json!("")) {
            json!("workspace")
        } else {
            r.get("status").cloned().unwrap_or(Value::Null)
        },
    );
    row.insert(
        "detail".into(),
        r.get("path").cloned().unwrap_or(Value::Null),
    );
    row.insert("exec".into(), json!(go));
    row.insert("score".into(), json!(90000 - i as i64 * 100));
    row.insert("sessions".into(), json!(sessions));
    // Only the first row carries the counts — the view header reads them
    // from whichever row leads.
    row.insert(
        "counts".into(),
        if i == 0 {
            let mut c = counts.as_object().cloned().unwrap_or_default();
            c.insert("matched".into(), json!(matched));
            Value::Object(c)
        } else {
            Value::Null
        },
    );
    row.insert("actions".into(), json!(actions));
    Some(Value::Object(row))
}

/// The whole pipeline: `(name, snapshot)` pairs plus the seen map in, the
/// rebuilt state and the emitted rows out. `None` is jq's death — the
/// caller retries once with an empty seen map, as the script did.
pub fn build(
    sessions: &[(String, Value)],
    seen: &Map<String, Value>,
    now: i64,
    home: &str,
    needle: &str,
) -> Option<(Map<String, Value>, Vec<Value>)> {
    // `[ $sessions[] | .session as $s | .snap.agents[]? | . + { session: $s } ]`
    let mut agents: Vec<Value> = Vec::new();
    for (name, snap) in sessions {
        for a in members(snap, "agents")? {
            let mut a = a.clone();
            // `. + { session: $s }` — object merge; a non-object agent dies.
            a.as_object_mut()?.insert("session".into(), json!(name));
            agents.push(a);
        }
    }

    // The `from_entries` lookups the jq built for workspaces and tabs.
    let mut ws: HashMap<String, Value> = HashMap::new();
    let mut tabs: HashMap<String, Value> = HashMap::new();
    for (name, snap) in sessions {
        for w in members(snap, "workspaces")? {
            let key = key3(name, field(w, "workspace_id")?)?;
            ws.insert(key, w.clone());
        }
        for t in members(snap, "tabs")? {
            let key = key3(name, field(t, "tab_id")?)?;
            tabs.insert(key, t.clone());
        }
    }

    let state = rebuild_state(&agents, seen, now)?;

    // `$busy`: the `session|workspace_id` pairs that hold an agent.
    let mut busy: HashSet<String> = HashSet::new();
    for a in &agents {
        let session = a.get("session").and_then(Value::as_str)?;
        busy.insert(key3(session, a.get("workspace_id"))?);
    }

    let mut rows = Vec::with_capacity(agents.len());
    for a in &agents {
        rows.push(agent_row(a, &ws, &tabs, &state, home, now)?);
    }

    let mut all = rows.clone();
    for (name, snap) in sessions {
        for w in members(snap, "workspaces")? {
            let key = key3(name, field(w, "workspace_id")?)?;
            if busy.contains(&key) {
                continue;
            }
            all.push(idle_row(name, w)?);
        }
    }

    // The counts the view header draws, counted over the agent rows alone.
    let in_band = |b: i64| {
        rows.iter()
            .filter(|r| r.get("band").and_then(Value::as_i64) == Some(b))
            .count()
    };
    let counts = json!({
        "blocked": in_band(0),
        "done": in_band(1),
        "working": in_band(2),
        "idle": in_band(3),
        "unknown": in_band(4),
        "agents": rows.len(),
        "spaces": all.len(),
        "sessions": sessions.len(),
    });

    // `select($needle == "" or (haystack | contains($needle)))` — jq's `or`
    // short-circuits, so an unjoinable row only kills a narrowed query.
    let mut shown: Vec<&Map<String, Value>> = Vec::new();
    for r in &all {
        let m = r.as_object()?;
        if needle.is_empty() {
            shown.push(m);
        } else {
            match haystack(m) {
                Some(h) if h.contains(needle) => shown.push(m),
                Some(_) => {}
                None => return None,
            }
        }
    }
    shown.sort_by(|a, b| sort_cmp(a, b));
    let shown = &shown[..shown.len().min(24)];

    let mut out = Vec::with_capacity(shown.len());
    for (i, r) in shown.iter().enumerate() {
        out.push(emit(r, i, &counts, sessions.len(), shown.len())?);
    }
    Some((state, out))
}
