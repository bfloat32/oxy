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
    let value = value.to_string();
    // The engine's caller does not wait on the write, so the lock's spin
    // belongs off its thread — a raw spawn, since this can run where no
    // runtime is.
    std::thread::spawn(move || {
        // `flock -w 2` parity: `File::try_lock` is flock(2) on unix, so the
        // script's `flock "$RECENT.lock"` and this lock the same file — and
        // like the script, the record is dropped rather than written
        // unlocked when the wait expires. The name is `$RECENT.lock`
        // appended, not `with_extension`, so a file that carries one keeps it.
        let mut lock = path.as_os_str().to_os_string();
        lock.push(".lock");
        let lock = std::path::PathBuf::from(lock);
        let Ok(file) = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock)
        else {
            return;
        };
        let mut held = false;
        for _ in 0..200 {
            match file.try_lock() {
                Ok(()) => {
                    held = true;
                    break;
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(10))
                }
                Err(_) => break,
            }
        }
        if !held {
            return;
        }
        let mut lines: Vec<String> = std::fs::read_to_string(&path)
            .ok()
            .map(|t| {
                t.lines()
                    .map(|l| l.to_string())
                    .filter(|l| !l.is_empty() && *l != value)
                    .collect()
            })
            .unwrap_or_default();
        lines.insert(0, value);
        lines.truncate(keep);
        let _ = super::write_atomic(&path, &(lines.join("\n") + "\n"));
        // The lockfile itself stays — the script's persistent `$RECENT.lock`
        // is the same inode everyone flocks; `file` drops here, releasing it.
    });
}
