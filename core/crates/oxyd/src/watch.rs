use std::path::{Path, PathBuf};

use oxy_core::engine::EngineCmd;
use tokio::sync::mpsc;

// Watch the extensions dir and oxy.json: a file landing or changing
// reloads the registry and settings, the way the FileView +
// Settings.qml watchers did. `settings.json` writes (form saves) change
// the signature too — one extra reload is harmless.
pub(crate) fn spawn(
    reload_tx: mpsc::Sender<EngineCmd>,
    watch_dir: PathBuf,
    settings_path: PathBuf,
) {
    tokio::spawn(async move {
        let mut last_ext = signature(&watch_dir);
        let mut last_cfg = file_signature(&settings_path);
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            let sig = signature(&watch_dir);
            if sig != last_ext {
                last_ext = sig;
                let _ = reload_tx.send(EngineCmd::Reload).await;
                continue;
            }
            let csig = file_signature(&settings_path);
            if csig != last_cfg {
                last_cfg = csig;
                let _ = reload_tx.send(EngineCmd::Reload).await;
            }
        }
    });
}

/// A single file's change signature — len + mtime, so an edit or a delete
/// (None metadata → 0) both register. Same fixed-key hasher as `signature`.
fn file_signature(path: &Path) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match std::fs::metadata(path) {
        Ok(meta) => {
            h.write_u64(meta.len());
            h.write_u64(
                meta.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            );
        }
        Err(_) => h.write_u64(u64::MAX),
    }
    h.finish()
}

/// What a reload watches: every extension file's name, size and mtime. A
/// summon whose signature matches pays one stat and keeps every worker it
/// already has. The hash is XOR-accumulated so readdir order doesn't matter,
/// and `DefaultHasher::new` uses fixed keys, so two polls are comparable.
fn signature(dir: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut acc = 0u64;
    if let Ok(read) = std::fs::read_dir(dir) {
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let mut h = std::collections::hash_map::DefaultHasher::new();
            if let Some(name) = path.file_name() {
                name.as_encoded_bytes().hash(&mut h);
            }
            h.write_u64(meta.len());
            h.write_u64(
                meta.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            );
            acc ^= h.finish();
        }
    }
    acc
}
