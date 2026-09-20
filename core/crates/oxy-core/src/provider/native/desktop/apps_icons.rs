//! The icon index `apps:` resolves a desktop file's `Icon=` through: name →
//! file, built by walking the icon dirs the way the QML fallback's `find`
//! did.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::apps::App;

/// name → icon file, built beside the app scan. `app` mirrors the QML
/// `iconIndex`: `*/apps/*` and `*/devices/*` under every icon dir plus
/// /usr/share/pixmaps, svg before png, first hit per name. `any` is every
/// context in the same dirs — the stand-in for `Quickshell.iconPath(name,
/// true)`, which walks the same places Qt's theme search does, so a name the
/// narrow index missed still resolves to a file instead of shipping a dead
/// string to `Image.source`.
#[derive(Default)]
pub(super) struct IconIndex {
    pub(super) app: HashMap<String, String>,
    pub(super) any: HashMap<String, String>,
}

/// The dirs the QML icon scan walked: `~/.icons`, the data-home `icons`, and
/// `icons` under each XDG data dir — /usr/share/pixmaps joins per ext pass,
/// top level only, the way its `find -maxdepth 1` did.
fn icon_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        crate::settings::paths::home().join(".icons"),
        crate::settings::paths::data_home().join("icons"),
    ];
    for base in std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string())
        .split(':')
        .filter(|d| !d.is_empty())
    {
        dirs.push(Path::new(base).join("icons"));
    }
    dirs
}

/// One file into the index, keyed the way `indexIconLine` keyed it: basename
/// minus its last extension, first hit wins. `primary` marks the apps/devices
/// index the QML consulted before its themed fallback.
fn index_icon(path: &Path, primary: bool, idx: &mut IconIndex) {
    let Some(file) = path.file_name().and_then(|f| f.to_str()) else {
        return;
    };
    let name = match file.rfind('.') {
        Some(dot) if dot > 0 => &file[..dot],
        _ => file,
    };
    if name.is_empty() {
        return;
    }
    let p = path.to_string_lossy().into_owned();
    idx.any.entry(name.to_string()).or_insert_with(|| p.clone());
    // `*/apps/*`/`*/devices/*` in find's grammar is a directory component —
    // the parent's components, so a file literally named `apps.png` does not
    // count as living under an apps dir.
    let themed = primary
        || path.parent().is_some_and(|parent| {
            parent
                .components()
                .any(|c| c.as_os_str() == "apps" || c.as_os_str() == "devices")
        });
    if themed {
        idx.app.entry(name.to_string()).or_insert(p);
    }
}

/// Recursive walk for the icon dirs — `find` without -L, so a symlinked dir
/// is not descended into but a symlinked file still counts.
fn collect_icons(dir: &Path, ext: &str, idx: &mut IconIndex) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(path),
                Ok(t)
                    if (t.is_file() || t.is_symlink())
                        && path.extension().and_then(|e| e.to_str()) == Some(ext) =>
                {
                    index_icon(&path, false, idx);
                }
                _ => {}
            }
        }
    }
}

/// /usr/share/pixmaps, one level deep — every file there is a primary hit.
fn collect_pixmaps(ext: &str, idx: &mut IconIndex) {
    let Ok(read) = std::fs::read_dir("/usr/share/pixmaps") else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some(ext)
            && entry
                .file_type()
                .map(|t| t.is_file() || t.is_symlink())
                .unwrap_or(false)
        {
            index_icon(&path, true, idx);
        }
    }
}

/// The set the icon index was built for, fingerprinted order-independently:
/// a changed `.desktop` set is the signal that re-scans the icon dirs, so
/// `scan_icons` does not run on every summon.
pub(super) fn apps_fingerprint(apps: &[App]) -> u64 {
    use std::hash::Hasher;
    let mut mix = apps.len() as u64;
    for app in apps {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        h.write(app.entry.id.as_bytes());
        h.write(app.icon.as_bytes());
        mix ^= h.finish();
    }
    mix
}

/// The two ext passes the script ran: svg everywhere before png anywhere, so
/// a scalable icon beats a raster one no matter which dir it lives in.
pub(super) fn scan_icons() -> IconIndex {
    let mut idx = IconIndex::default();
    let dirs = icon_dirs();
    for ext in ["svg", "png"] {
        for base in &dirs {
            collect_icons(base, ext, &mut idx);
        }
        collect_pixmaps(ext, &mut idx);
    }
    idx
}

/// `AppLibraryFallback.iconSource` in Rust: file and image urls pass through,
/// an absolute path becomes a file url, a bare name resolves through the
/// apps/devices index first — an unconstrained lookup can answer with an
/// action icon for a name like "zoom" — then the whole icon tree, then the
/// generic executable icon, so a row never ships a name nothing can draw.
pub(super) fn resolve_icon(icon: &str, icons: &IconIndex) -> String {
    let value = icon.trim();
    let fallback = || {
        icons
            .any
            .get("application-x-executable")
            .map(|p| crate::provider::native::system::file::file_url(p))
            .unwrap_or_default()
    };
    if value.is_empty() {
        return fallback();
    }
    if value.starts_with("file://") || value.starts_with("image://") {
        return value.to_string();
    }
    if value.starts_with('/') || (value.len() > 2 && value.as_bytes()[1] == b':') {
        // An absolute path — `/usr/…` on Unix, `C:\…` on Windows.
        return crate::provider::native::system::file::file_url(value);
    }
    if let Some(path) = icons.app.get(value).or_else(|| icons.any.get(value)) {
        return crate::provider::native::system::file::file_url(path);
    }
    fallback()
}
