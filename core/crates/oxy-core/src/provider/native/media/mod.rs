//! Media extensions: internet radio, now-playing over MPRIS, and the
//! authenticated Spotify catalogue. Each is a port of the read half of its
//! `bin/oxy-*` script; the write half (play, ctl, auth) stays in the scripts
//! the rows' exec strings already call.

pub mod radio;
pub mod spotify;
pub mod spotify_library;

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// Seconds since the epoch — `date +%s`.
pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `x // "" | tostring`: jq's alternative operator collapses `null` and
/// `false` to the empty string before `tostring` renders whatever is left —
/// strings as themselves, numbers and `true` as their text, anything
/// structured as compact JSON. That is how the scripts flatten the fields
/// they read, so that is what the ports read them as.
pub(crate) fn jq_str(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(true)) => "true".to_string(),
        Some(other) if other.is_array() || other.is_object() => other.to_string(),
        _ => String::new(),
    }
}
