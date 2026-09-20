//! MRU files: the provider-declared recency lists, one file each, newest
//! first.

use std::path::Path;

/// A provider-declared recency file. A row or an action carrying
/// `"remember": {"file": "emoji-recent", "value": "😂"}` asks the engine to
/// put the value at the top of `~/.local/state/omarchy/oxy-<file>` — the
/// write `oxy-emoji --used` did, without the script having to be on PATH.
///
/// Newest first, one value per line, exact duplicates kept once — the same
/// contract the scripts that own these files follow, so a machine running
/// both variants shares one list.
pub fn mru_record(state_dir: &Path, name: &str, value: &str, keep: usize) {
    // The name picks the file: only `a-z0-9-` reaches the path, so a provider
    // cannot write outside the state dir through it.
    let clean: String = name
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .collect();
    if clean.is_empty() || value.is_empty() || keep == 0 {
        return;
    }
    let path = state_dir.join(format!("oxy-{clean}"));
    let mut lines: Vec<String> = std::fs::read_to_string(&path)
        .ok()
        .map(|t| {
            t.lines()
                .map(|l| l.to_string())
                .filter(|l| !l.is_empty() && *l != value)
                .collect()
        })
        .unwrap_or_default();
    lines.insert(0, value.to_string());
    lines.truncate(keep);
    let _ = super::write_atomic(&path, &(lines.join("\n") + "\n"));
}
