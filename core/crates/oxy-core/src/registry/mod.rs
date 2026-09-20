//! An extension is one JSON file in `~/.config/omarchy/oxy/extensions/`. It
//! names a keyword and something that answers for it. A port of
//! `Extensions.normalize` plus the loader `Launcher.applyExtensions` ran.
//!
//! On this branch an extension may also name a `"native"` implementation — a
//! provider compiled into the daemon — and may carry both `native` and
//! `search`, in which case the native provider answers first and may decline
//! to the command.

mod command;
mod def;

use std::path::{Path, PathBuf};

use serde_json::Value;

pub use command::*;
pub use def::*;

/// What a malformed file logged. A bad extension failing silently is the one
/// bug a user cannot see.
pub struct LoadReport {
    pub extensions: Vec<Extension>,
    /// (file, why) for every file that could not be loaded.
    pub bad: Vec<(PathBuf, String)>,
}

/// Read every `*.json` in the dir except `*.cases.json`, normalize each.
/// `enabled` is the `extensions` map from settings: absent means on.
pub fn load_dir(dir: &Path, enabled: &serde_json::Map<String, Value>) -> LoadReport {
    let mut report = LoadReport {
        extensions: Vec::new(),
        bad: Vec::new(),
    };

    let mut files: Vec<PathBuf> = Vec::new();
    if let Ok(read) = std::fs::read_dir(dir) {
        for entry in read.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".json") && !name.ends_with(".cases.json") {
                files.push(path);
            }
        }
    }
    files.sort();

    for path in files {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                report.bad.push((path, format!("read: {e}")));
                continue;
            }
        };
        let raw = match serde_json::from_str::<Value>(&text) {
            Ok(v) => v,
            Err(e) => {
                report.bad.push((path, format!("json: {e}")));
                continue;
            }
        };
        match Extension::normalize(&raw, path.clone()) {
            Some(ext) => {
                // Absent means on. Name one false to silence it.
                let off = enabled.get(&ext.id).and_then(|v| v.as_bool()) == Some(false);
                if !off {
                    report.extensions.push(ext);
                }
            }
            None => report.bad.push((path, "normalize".to_string())),
        }
    }
    report
}
