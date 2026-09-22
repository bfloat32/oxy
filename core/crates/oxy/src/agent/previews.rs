//! The preview store: `search` writes down what it drew, keyed by token, so
//! `send <token>` can only start something that was on screen. A JSON object
//! in the state dir, shared between the daemon and every one-shot `send` /
//! `plan` / `term` the launcher spawns.

use serde_json::{Map, Value};

use crate::agent::{self, PREVIEW_MAX, PREVIEW_TTL, SENT_TTL};

/// Whether a preview is still worth honouring. A sentence typed and left
/// behind is not a standing offer to run it: an hour later the directory has
/// moved on and so has the person, and a token that still resolves is a way
/// for a stale row to start something nobody is looking at.
fn fresh(entry: &Value, now: f64) -> bool {
    let Some(obj) = entry.as_object() else {
        return false;
    };
    if obj
        .get("instruction")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .is_empty()
    {
        return false;
    }
    let ttl = if obj.get("sent").is_some() {
        SENT_TTL
    } else {
        PREVIEW_TTL
    };
    let at = obj.get("at").and_then(|v| v.as_f64()).unwrap_or(0.0);
    now - at <= ttl
}

/// The live tokens. Expiry happens here rather than only on write, so a
/// token that has aged out fails closed on the next `send` even if nothing
/// has been typed since.
pub fn read() -> Map<String, Value> {
    let Ok(text) = std::fs::read_to_string(agent::previews_path()) else {
        return Map::new();
    };
    let Ok(Value::Object(data)) = serde_json::from_str::<Value>(&text) else {
        return Map::new();
    };
    let now = agent::now();
    data.into_iter().filter(|(_, v)| fresh(v, now)).collect()
}

fn write(data: &Map<String, Value>) {
    let _ = std::fs::create_dir_all(agent::state_dir());
    let path = agent::previews_path();
    let tmp = path.with_file_name(format!(
        "{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let Ok(body) = serde_json::to_string(&Value::Object(data.clone())) else {
        return;
    };
    if std::fs::write(&tmp, body).is_err() {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    let _ = std::fs::rename(&tmp, &path);
}

/// What the user has been shown, so `send <token>` can only start something
/// that was previewed. Every keystroke lands here, half-typed prefixes and
/// all, so it is kept small on purpose: the expired ones are dropped on the
/// way in and only the newest sixteen survive. The one Enter needs is always
/// the one just written, so a low ceiling can never evict the row on screen.
pub fn remember(token: &str, entry: Value) {
    let mut data = read();
    if data.contains_key(token) {
        return;
    }
    data.insert(token.to_string(), entry);
    if data.len() > PREVIEW_MAX {
        let mut by_age: Vec<(String, f64)> = data
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    v.get("at").and_then(|a| a.as_f64()).unwrap_or(0.0),
                )
            })
            .collect();
        by_age.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        for (key, _) in by_age.into_iter().take(data.len() - PREVIEW_MAX) {
            data.shift_remove(&key);
        }
    }
    write(&data);
}

/// This one became a run. Kept longer than a draft, because it is now the
/// only record of what the token behind a running card actually said.
/// Called by the daemon's `send`, which is unix-only.
#[cfg(unix)]
pub fn mark_sent(token: &str) {
    let mut data = read();
    let Some(entry) = data.get_mut(token) else {
        return;
    };
    if entry.get("sent").is_some() {
        return;
    }
    entry["sent"] = Value::from(agent::now() as i64);
    write(&data);
}
