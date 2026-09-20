use std::path::Path;

use serde_json::{Value, json};

/// One daemon boot's id — `Date.now().toString(36)` the way Logger.qml
/// minted it, so one log file can tell two sessions' lines apart.
pub(crate) fn new_sid() -> String {
    let mut n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if n == 0 {
        return "0".into();
    }
    let mut buf = Vec::new();
    while n > 0 {
        buf.push(char::from_digit((n % 36) as u32, 36).unwrap_or('0'));
        n /= 36;
    }
    buf.iter().rev().collect()
}

/// One line of the event log: `{ts, sid, ev, ...fields}` — the shape
/// Logger.qml wrote, appended by whichever daemon holds the file.
pub(crate) fn log_line(sid: &str, ev: &str, fields: &Value) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut obj = match fields {
        Value::Object(m) => m.clone(),
        _ => Default::default(),
    };
    obj.insert("ts".into(), json!(ts));
    obj.insert("sid".into(), json!(sid));
    obj.insert("ev".into(), json!(ev));
    Value::Object(obj).to_string() + "\n"
}

pub(crate) fn append_log(path: &Path, line: &str) {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Logger.qml rotated at 1MB into a single `.old`: the file can hold at
    // most ~2MB of history, and a tail-of-tails is still on disk for forensics.
    if std::fs::metadata(path)
        .map(|m| m.len() > 1_048_576)
        .unwrap_or(false)
    {
        let mut old = path.as_os_str().to_os_string();
        old.push(".old");
        let _ = std::fs::rename(path, old);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two generations, and the second rotation has to *replace* `.old`
    /// rather than fail: on Windows a rename onto an existing file is the
    /// case worth checking, and this runs on both platforms.
    #[test]
    fn rotation_keeps_two_generations() {
        let dir = std::env::temp_dir().join(format!("oxyd-log-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("oxy.log");
        let old = dir.join("oxy.log.old");

        std::fs::write(&path, vec![b'x'; 1_048_577]).unwrap();
        append_log(&path, "one\n");
        assert!(old.exists(), "the first rotation writes .old");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\n");

        std::fs::write(&path, vec![b'y'; 1_048_577]).unwrap();
        append_log(&path, "two\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two\n");
        let kept = std::fs::read_to_string(&old).unwrap();
        assert!(kept.starts_with('y'), "the second rotation replaced .old");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_small_file_is_never_rotated() {
        let dir = std::env::temp_dir().join(format!("oxyd-log-small-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("oxy.log");
        append_log(&path, "first\n");
        append_log(&path, "second\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\nsecond\n");
        assert!(!dir.join("oxy.log.old").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_line_carries_ts_sid_and_ev() {
        let line = log_line("abc", "prov.done", &json!({"ms": 12}));
        assert!(line.ends_with('\n'));
        let v: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(v["sid"], "abc");
        assert_eq!(v["ev"], "prov.done");
        assert_eq!(v["ms"], 12);
        assert!(v["ts"].as_u64().unwrap() > 0);
    }
}
