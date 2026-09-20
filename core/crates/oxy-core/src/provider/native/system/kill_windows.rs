//! The window lookup `kill:` joins against: what Hyprland knows, keyed by
//! pid, and an empty answer where no compositor can be asked.

use std::collections::HashMap;

#[cfg(unix)]
use serde_json::Value;

/// The windows Hyprland knows about, keyed by pid. One cheap subprocess,
/// same as the script; absent on any other compositor, which simply means no
/// Focus action and no title match.
#[cfg(unix)]
pub(super) fn windows() -> HashMap<u32, Win> {
    let Ok(out) = std::process::Command::new("hyprctl")
        .args(["clients", "-j"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return HashMap::new();
    };
    let Ok(list) = serde_json::from_slice::<Vec<Value>>(&out.stdout) else {
        return HashMap::new();
    };
    let mut by_pid: HashMap<u32, Win> = HashMap::new();
    for w in list {
        let Some(pid) = w.get("pid").and_then(|p| p.as_i64()) else {
            continue;
        };
        if pid <= 0 || !w.get("mapped").and_then(|m| m.as_bool()).unwrap_or(false) {
            continue;
        }
        let entry = by_pid.entry(pid as u32).or_insert_with(|| Win {
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
