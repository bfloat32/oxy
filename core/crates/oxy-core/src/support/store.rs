//! Reading the JSON files this launcher owns, without losing one silently.
//!
//! Every loader used to be `.ok().and_then(|t| parse(t).ok()).unwrap_or_default()`.
//! A file truncated by a kill mid-write therefore read as "no state at all",
//! and the next save wrote that emptiness back over it — pins, recents and
//! frecency gone, with nothing anywhere saying so. A file that does not parse
//! is moved aside here instead, and the caller is handed the path to report.
//! Nothing is ever deleted.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

/// Read one JSON file.
///
/// - not there → `None`, nothing moved: a first run.
/// - parses → the value.
/// - does not → `None`, the file renamed beside itself to `<stem>.corrupt`,
///   and that path pushed onto `moved` for the caller to say out loud. One
///   generation is kept: a second corruption replaces the first, which is the
///   one a user is least likely to still want.
///
/// An empty file counts as corrupt: every writer here writes an object, so a
/// zero-byte file is a write that did not finish.
pub fn read_json<T: DeserializeOwned>(path: &Path, moved: &mut Vec<PathBuf>) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&text) {
        Ok(value) => Some(value),
        Err(_) => {
            let backup = backup_path(path);
            if std::fs::rename(path, &backup).is_ok() {
                moved.push(backup);
            }
            None
        }
    }
}

/// `<dir>/<stem>.corrupt` — beside the original, so a user finds it where the
/// file was, and distinct per file (`oxy-state.corrupt`, `oxy-frecency.corrupt`).
pub fn backup_path(path: &Path) -> PathBuf {
    path.with_extension("corrupt")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Doc {
        n: u32,
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("oxy-store-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_missing_file_is_a_first_run_not_a_loss() {
        let mut moved = Vec::new();
        let d = dir("missing");
        assert!(read_json::<Doc>(&d.join("none.json"), &mut moved).is_none());
        assert!(moved.is_empty());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_good_file_reads_and_moves_nothing() {
        let mut moved = Vec::new();
        let d = dir("good");
        let path = d.join("state.json");
        std::fs::write(&path, r#"{"n":7}"#).unwrap();
        assert_eq!(read_json::<Doc>(&path, &mut moved), Some(Doc { n: 7 }));
        assert!(moved.is_empty());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_corrupt_file_is_kept_beside_itself() {
        let mut moved = Vec::new();
        let d = dir("corrupt");
        let path = d.join("oxy-state.json");
        std::fs::write(&path, r#"{"n": 1"#).unwrap(); // truncated mid-write
        assert!(read_json::<Doc>(&path, &mut moved).is_none());
        assert_eq!(moved, vec![d.join("oxy-state.corrupt")]);
        assert!(!path.exists(), "the broken file is out of the way");
        assert_eq!(
            std::fs::read_to_string(&moved[0]).unwrap(),
            r#"{"n": 1"#,
            "its bytes are intact for a human to look at"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_second_corruption_replaces_the_backup() {
        let mut moved = Vec::new();
        let d = dir("second");
        let path = d.join("oxy-state.json");
        std::fs::write(&path, "first is broken").unwrap();
        assert!(read_json::<Doc>(&path, &mut moved).is_none());
        std::fs::write(&path, "second is broken too").unwrap();
        assert!(read_json::<Doc>(&path, &mut moved).is_none());
        assert_eq!(moved.len(), 2);
        assert_eq!(moved[0], moved[1], "one generation, not a pile");
        assert_eq!(
            std::fs::read_to_string(&moved[1]).unwrap(),
            "second is broken too"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn an_empty_file_counts_as_corrupt() {
        let mut moved = Vec::new();
        let d = dir("empty");
        let path = d.join("oxy-frecency.json");
        std::fs::write(&path, "").unwrap();
        assert!(read_json::<Doc>(&path, &mut moved).is_none());
        assert_eq!(moved, vec![d.join("oxy-frecency.corrupt")]);
        std::fs::remove_dir_all(&d).ok();
    }
}
