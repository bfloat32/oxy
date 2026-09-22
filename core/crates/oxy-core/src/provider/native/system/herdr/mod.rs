//! `herdr:` — every agent herdr is watching, the ones waiting on you first.
//! A port of `bin/oxy-herdr`'s read path: `herdr session list` names the
//! running sessions, `herdr api snapshot` carries agents, panes, tabs and
//! workspaces for each of them in one read, and the script's jq emit is
//! `rows::build`.
//!
//!   herdr:          every agent, the ones waiting on you first
//!   herdr:blocked   by state, name, kind, workspace, session or path
//!
//! The two traps the port keeps:
//!
//!   * Snapshots go only to sessions `session list` calls `running`. Naming
//!     a *stopped* session on an `api` call starts a server for it, and a
//!     launcher must never start something nobody asked for — so a stale
//!     name is skipped, never asked.
//!   * Every read stays read-only — `session list` and `api snapshot` — so
//!     the 2s refresh cannot mark a tab seen and quietly erase the `done`
//!     state the keyword exists to report. The one thing written is the
//!     script's own seen file, which is what gives `since` an honest
//!     duration.
//!
//! The `--focus` argv stays the script's: the rows' exec strings still call
//! `oxy-herdr --focus`, so Enter takes the same session-raising path it
//! always did — the script stays installed for exactly that reason.

mod rows;
#[cfg(test)]
mod tests;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::native::util::on_path;
use crate::provider::process;
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::support::quote::quote;

pub struct Herdr;

/// Per-call deadline: the script had none, but a wedged herdr socket is the
/// difference between "no answer" and "the launcher hung". The manifest's
/// own timeout is 4s; one read gets half of that.
const HERDR_TIMEOUT: Duration = Duration::from_secs(2);

/// The row a `herdr:` answers when no session has a server — the script's
/// verbatim emit, because showing nothing there reads as "no agents" or
/// "the keyword is broken", neither of which is true.
fn offline_row() -> Value {
    json!({
        "id": "herdr:offline",
        "view": "herdr",
        "offline": true,
        "title": "herdr is not running",
        "subtitle": "No session has a server. Press ↵ to start one.",
        "exec": "omarchy-launch-tui --app-id=org.omarchy.herdr herdr",
        "score": 90000,
    })
}

/// `herdr session list` → the running names → one `api snapshot` each,
/// falling back to the default session when the list answers nothing — an
/// older herdr without `session list` still has one worth asking, and if
/// it is not there either the snapshot fails and the offline card is the
/// right answer anyway. `None` is the jq death: a snapshot that looked
/// like JSON and was not.
async fn snapshots() -> Option<Vec<(String, Value)>> {
    let listed = process::run("herdr session list", HERDR_TIMEOUT).await;
    let mut names = listed
        .map(|f| rows::running_sessions(&f.stdout))
        .unwrap_or_default();
    if names.is_empty() {
        names.push("default".to_string());
    }
    // The snapshots are independent; sequential bounded calls would sum
    // past the manifest's window with two sessions running, so they join.
    let mut pending = tokio::task::JoinSet::new();
    for (idx, name) in names.into_iter().enumerate() {
        // The script's own argv: the default session takes no `--session`.
        let cmd = if name == "default" {
            "herdr api snapshot".to_string()
        } else {
            format!("herdr --session {} api snapshot", quote(&name))
        };
        pending.spawn(async move { (idx, name, process::run(&cmd, HERDR_TIMEOUT).await) });
    }
    let mut got = Vec::new();
    while let Some(joined) = pending.join_next().await {
        let Ok((idx, name, fin)) = joined else {
            continue;
        };
        let Some(fin) = fin else {
            continue;
        };
        match rows::classify(&fin.stdout) {
            rows::Snapshot::Skip => {}
            rows::Snapshot::Poison => return None,
            rows::Snapshot::Shot(snap) => got.push((idx, name, snap)),
        }
    }
    // Joined in completion order; the script emitted in session-list order.
    got.sort_by_key(|(idx, ..)| *idx);
    Some(
        got.into_iter()
            .map(|(_, name, snap)| (name, snap))
            .collect(),
    )
}

/// `${XDG_RUNTIME_DIR:-/tmp}/oxy-herdr-seen.json` — the path the script
/// keeps the seen map on, so a pane's "waiting since" survives restarts
/// and is shared when the `search` script runs instead.
fn seen_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    dir.join("oxy-herdr-seen.json")
}

/// The seen map, or the empty one: `[[ $seen == \{* ]] || seen='{}'` plus
/// `--argjson` needing an object. Anything else is the script's retry `{}`
/// arriving early — a truncated file is not a map, it is no memory.
fn read_seen() -> Map<String, Value> {
    let Ok(text) = std::fs::read_to_string(seen_path()) else {
        return Map::new();
    };
    if !text.starts_with('{') {
        return Map::new();
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

/// `printf '%s\n' "$state" > tmp && mv -f tmp seen` — written even when no
/// row matched, so typing a query cannot reset the durations on screen.
/// Best-effort as the script's was: a lost write costs one agent its start
/// time and nothing else.
fn write_seen(state: &Map<String, Value>) {
    let path = seen_path();
    // `"$seen_file.$$"` — the same pid suffix the script's temp carried.
    let mut tmp = path.into_os_string();
    tmp.push(format!(".{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let text = serde_json::to_string(&Value::Object(state.clone())).unwrap_or_default();
    if std::fs::write(&tmp, format!("{text}\n")).is_ok()
        && std::fs::rename(&tmp, seen_path()).is_err()
    {
        let _ = std::fs::remove_file(&tmp);
    }
}

impl NativeExt for Herdr {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        // `needle="${query,,}"` — the argument lowercased, untrimmed, as the
        // script took it.
        let needle = ctx.arg.to_lowercase();
        Box::pin(async move {
            // The manifest's `when` re-checked: a worker asks natives even
            // when the gate fails, so an absent herdr declines to the
            // script leg rather than answering for it.
            if !on_path("herdr") {
                return NativeOutcome::Fallback;
            }
            let Some(sessions) = snapshots().await else {
                return NativeOutcome::Empty;
            };
            if sessions.is_empty() {
                // No server running anywhere, or every socket stale — said
                // as the whole answer, the way the script said it.
                return NativeOutcome::Rows(vec![offline_row()]);
            }

            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let home = std::env::var("HOME").unwrap_or_default();
            let seen = read_seen();
            // The script's retry: a state file jq refused — truncated by a
            // crash, edited by hand — answers once more with `{}` rather
            // than going silent on every keystroke forever.
            let built = rows::build(&sessions, &seen, now, &home, &needle).or_else(|| {
                if seen.is_empty() {
                    None
                } else {
                    rows::build(&sessions, &Map::new(), now, &home, &needle)
                }
            });
            let Some((state, out)) = built else {
                return NativeOutcome::Empty;
            };
            write_seen(&state);
            NativeOutcome::Rows(out)
        })
    }
}
