//! The window lookup `kill:` joins against: what Hyprland knows, keyed by
//! pid, and an empty answer where no compositor can be asked.

use std::collections::HashMap;

#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use serde_json::Value;

/// The windows Hyprland knows about, keyed by pid. One cheap subprocess,
/// same as the script; absent on any other compositor, which simply means no
/// Focus action and no title match. `probe` bounds it: a wedged compositor
/// socket dies at the deadline instead of holding the blocking thread.
#[cfg(unix)]
pub(super) fn windows() -> HashMap<u32, Win> {
    let Some(out) =
        crate::provider::process::probe(&["hyprctl", "clients", "-j"], Duration::from_secs(2))
    else {
        return HashMap::new();
    };
    let Ok(list) = serde_json::from_str::<Vec<Value>>(&out) else {
        return HashMap::new();
    };
    let mut by_pid: HashMap<u32, Win> = HashMap::new();
    for w in list {
        let Some(pid) = w
            .get("pid")
            .and_then(|p| p.as_i64())
            .and_then(|p| u32::try_from(p).ok())
        else {
            continue;
        };
        if !w.get("mapped").and_then(|m| m.as_bool()).unwrap_or(false) {
            continue;
        }
        let entry = by_pid.entry(pid).or_insert_with(|| Win {
            addr: String::new(),
            class: String::new(),
            title: String::new(),
            workspace: String::new(),
            count: 0,
        });
        if entry.count == 0 {
            entry.addr = w
                .get("address")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .into();
            entry.class = w.get("class").and_then(|v| v.as_str()).unwrap_or("").into();
            entry.title = w.get("title").and_then(|v| v.as_str()).unwrap_or("").into();
            entry.workspace = w
                .get("workspace")
                .and_then(|s| s.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .into();
        }
        entry.count += 1;
    }
    by_pid
}

#[cfg(not(unix))]
pub(super) fn windows() -> HashMap<u32, Win> {
    HashMap::new()
}

pub(super) struct Win {
    pub(super) addr: String,
    pub(super) class: String,
    pub(super) title: String,
    pub(super) workspace: String,
    pub(super) count: usize,
}
