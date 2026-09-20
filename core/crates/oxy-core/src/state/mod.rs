//! The state a launcher earns: frecency, pins and recent queries.
//!
//! Two files, same as the QML launcher used: `oxy-frecency.json` (decaying
//! launch counts) and `oxy-state.json` (`{recents, pins}` — neither decays).

mod frecency;
mod mru;
mod pins;
mod recents;

use std::path::Path;

use serde::{Deserialize, Serialize};

pub use frecency::*;
pub use mru::*;
pub use pins::*;
pub use recents::*;

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
    pub fn load(frecency_path: &Path, state_path: &Path) -> State {
        let frecency = std::fs::read_to_string(frecency_path)
            .ok()
            .and_then(|t| serde_json::from_str::<Frecency>(&t).ok())
            .unwrap_or_default();

        let (recents, pins) = std::fs::read_to_string(state_path)
            .ok()
            .and_then(|t| serde_json::from_str::<StateFile>(&t).ok())
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

        State {
            frecency,
            recents,
            pins,
        }
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
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}
