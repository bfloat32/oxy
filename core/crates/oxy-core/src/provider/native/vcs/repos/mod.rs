//! What `bin/oxy-repo` knows, as a library, split in two: `discover` is
//! which repo a query names (roots, the walk, the list cache, `--resolve`),
//! `state` is what is true inside it (the porcelain counts, the stamps, the
//! per-repo cache). `git:`/`branch:`/`stash:` all start from `resolve` — the
//! same matching the script's `--resolve` mode does, pure string work over
//! the cached list, so "which repo" never forks `git status` per candidate
//! again (227ms a keystroke before that mode existed).
//!
//! Every piece that reads the environment or spawns a command sits in a thin
//! wrapper over a pure core so the logic is unit-testable without unsafe
//! `env::set_var`.

mod discover;
mod state;

pub(crate) use discover::*;
pub(crate) use state::*;

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::settings::paths;

/// Rows a `repo:` answer carries — the script's MAX_ROWS.
pub(crate) const MAX_ROWS: usize = 12;

/// A cached dirty count older than this is re-derived in the background; the
/// worktree can move without anything under .git noticing (DIRTY_RECHECK).
/// Ceiling on per-repo cache entries; a working set is far smaller.
/// Bump when the state-line format changes — the script's CACHE_FORMAT.
/// The byte a repo path rides in on, because a tab inside one must not split
/// the line (the scripts' SEP — unit separator, never whitespace-collapsed).
pub(crate) const US: char = '\u{1f}';

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The `roots` value from the `repo` extension's settings — what the script
/// leg receives injected as `OXY_ROOTS`. Every provider that resolves
/// through the roots list reads it so a settings-UI edit applies here too.
pub(crate) fn roots_setting(settings: &crate::settings::Settings) -> Option<String> {
    settings
        .settings_for("repo")
        .and_then(|m| m.get("roots"))
        .and_then(serde_json::Value::as_str)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

fn mtime(path: &Path) -> u64 {
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn cache_home() -> PathBuf {
    std::env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| paths::home().join(".cache"))
}

/// `$XDG_STATE_HOME/omarchy` — the scripts' `$STATE`, holding the repo list
/// and the pin.
pub(crate) fn oxy_state_dir() -> PathBuf {
    paths::state_home().join("omarchy")
}

fn repo_pin() -> PathBuf {
    oxy_state_dir().join("oxy-repo")
}

/// `~/path` for the row's `detail` — the scripts' `${repo/#$HOME/\~}`.
pub(crate) fn display_path(repo: &Path) -> String {
    let home = paths::home();
    let s = repo.to_string_lossy();
    match s.strip_prefix(&*home.to_string_lossy()) {
        Some(rest) if rest.starts_with('/') || rest.starts_with('\\') => format!("~{rest}"),
        _ => s.into_owned(),
    }
}

/// The scripts' `short_age`/`age`: `0m` under a minute (not `now` — these
/// families say minutes from zero), then h/d/mo/y.
pub(crate) fn age(secs: i64) -> String {
    let secs = secs.max(0);
    match secs {
        0..=3599 => format!("{}m", secs / 60),
        3600..=86399 => format!("{}h", secs / 3600),
        86400..=2591999 => format!("{}d", secs / 86400),
        2592000..=31535999 => format!("{}mo", secs / 2592000),
        _ => format!("{}y", secs / 31536000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_read_like_the_scripts() {
        assert_eq!(age(0), "0m");
        assert_eq!(age(59), "0m");
        assert_eq!(age(60), "1m");
        assert_eq!(age(3600), "1h");
        assert_eq!(age(86400), "1d");
        assert_eq!(age(2592000), "1mo");
        assert_eq!(age(31536000), "1y");
        assert_eq!(age(-3), "0m");
    }
}
