//! The state a launcher earns: frecency, pins and recent queries.
//!
//! Two files, same as the QML launcher used: `oxy-frecency.json` (decaying
//! launch counts) and `oxy-state.json` (`{recents, pins}` — neither decays).

mod frecency;
mod mru;
mod pins;
mod recents;
pub mod usage;

use std::path::Path;

use serde::{Deserialize, Serialize};

pub use frecency::*;
pub use mru::*;
pub use pins::*;
pub use recents::*;
pub use usage::*;

// ------------------------------------------------------------ persistence

#[derive(Debug, Default, Serialize, Deserialize)]
struct StateFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    recents: Vec<String>,
    #[serde(default)]
    pins: Pins,
}

pub struct State {
    pub frecency: Frecency,
    pub recents: Vec<String>,
    pub pins: Pins,
}

impl State {
    /// Read both files. The second half of the return is the files that did
    /// not parse and were moved aside — the caller reports them, because a
    /// silent reset of pins and recents is exactly the failure this prevents.
    pub fn load(frecency_path: &Path, state_path: &Path) -> (State, Vec<std::path::PathBuf>) {
        let mut moved = Vec::new();
        let frecency = crate::support::store::read_json::<Frecency>(frecency_path, &mut moved)
            .unwrap_or_default();

        let (recents, pins) = crate::support::store::read_json::<StateFile>(state_path, &mut moved)
            .map(|s| {
                let recents = s
                    .recents
                    .into_iter()
                    .map(|r| r.trim().to_string())
                    .filter(|r| !r.is_empty())
                    .take(RECENTS_LIMIT)
                    .collect();
                let pins: Pins = s.pins.into_iter().filter(|(_, v)| *v).collect();
                (recents, pins)
            })
            .unwrap_or_default();

        (
            State {
                frecency,
                recents,
                pins,
            },
            moved,
        )
    }

    /// Atomic write: the file is renamed over, so a crash mid-save cannot lose
    /// the ranking.
    pub fn save(&self, frecency_path: &Path, state_path: &Path) -> std::io::Result<()> {
        write_atomic(
            frecency_path,
            &serde_json::to_string(&self.frecency).unwrap_or_default(),
        )?;
        write_atomic(
            state_path,
            &serde_json::to_string(&StateFile {
                version: 1,
                recents: self.recents.clone(),
                pins: self.pins.clone(),
            })
            .unwrap_or_default(),
        )
    }
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A per-writer tmp name: `oxy test --local` and the daemon can both be
    // mid-write on the same file, and one shared `.tmp` would interleave.
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("oxy-state-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_truncated_state_file_is_moved_aside_not_read_as_empty() {
        let d = dir("corrupt");
        let frecency = d.join("oxy-frecency.json");
        let state_path = d.join("oxy-state.json");
        std::fs::write(&frecency, r#"{"app:x":{"count":3,"last":1}}"#).unwrap();
        std::fs::write(&state_path, r#"{"recents":["git:"],"#).unwrap(); // cut mid-write

        let (state, moved) = State::load(&frecency, &state_path);
        assert_eq!(moved, vec![d.join("oxy-state.corrupt")]);
        assert!(state.pins.is_empty());
        assert!(state.recents.is_empty());
        // The half that did parse is untouched, and the broken file survives.
        assert!(state.frecency.contains_key("app:x"));
        assert!(d.join("oxy-state.corrupt").exists());
        assert!(!state_path.exists());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_clean_load_moves_nothing() {
        let d = dir("clean");
        let frecency = d.join("oxy-frecency.json");
        let state_path = d.join("oxy-state.json");
        std::fs::write(&frecency, "{}").unwrap();
        std::fs::write(
            &state_path,
            r#"{"recents":["git:",""],"pins":{"app:x":true,"app:y":false}}"#,
        )
        .unwrap();

        let (state, moved) = State::load(&frecency, &state_path);
        assert!(moved.is_empty());
        assert_eq!(
            state.recents,
            vec!["git:".to_string()],
            "empty entries drop"
        );
        assert!(state.pins.contains_key("app:x"));
        assert!(
            !state.pins.contains_key("app:y"),
            "a false pin is not a pin"
        );
        std::fs::remove_dir_all(&d).ok();
    }
}
