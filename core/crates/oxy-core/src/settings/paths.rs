//! Where Oxy's files live. The same paths the QML launcher used, so a machine
//! that ran one can run the other without losing state.

use std::path::PathBuf;

fn env_path(name: &str, fallback: PathBuf) -> PathBuf {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or(fallback)
}

pub fn home() -> PathBuf {
    env_path(
        "HOME",
        std::env::var("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_default(),
    )
}

pub fn config_home() -> PathBuf {
    env_path("XDG_CONFIG_HOME", home().join(".config"))
}

pub fn state_home() -> PathBuf {
    env_path("XDG_STATE_HOME", home().join(".local/state"))
}

pub fn data_home() -> PathBuf {
    env_path("XDG_DATA_HOME", home().join(".local/share"))
}

/// `~/.config/omarchy/oxy.json` — the user's settings.
pub fn settings_file() -> PathBuf {
    config_home().join("omarchy/oxy.json")
}

/// `~/.config/omarchy/oxy/extensions/` — the extension registry.
pub fn extensions_dir() -> PathBuf {
    config_home().join("omarchy/oxy/extensions")
}

/// `~/.local/state/omarchy/oxy-state.json` — pins and recent queries.
pub fn state_file() -> PathBuf {
    state_home().join("omarchy/oxy-state.json")
}

/// `~/.local/state/omarchy/oxy-frecency.json` — launch counts.
pub fn frecency_file() -> PathBuf {
    state_home().join("omarchy/oxy-frecency.json")
}

/// `~/.local/state/omarchy/oxy-calc-history.json` — accepted answers.
pub fn calc_history_file() -> PathBuf {
    state_home().join("omarchy/oxy-calc-history.json")
}

/// `~/.local/state/omarchy/oxy-log.jsonl` — the event log.
pub fn log_file() -> PathBuf {
    state_home().join("omarchy/oxy-log.jsonl")
}

/// The model ledger: which backend answered, how often, when last.
pub fn usage_file() -> PathBuf {
    state_home().join("omarchy/oxy-model-usage.json")
}

/// The address the daemon listens on and the frontend connects to.
///
/// A filesystem socket on Unix (`$XDG_RUNTIME_DIR/oxyd.sock`, falling back to
/// the state dir), a named pipe on Windows so the daemon still runs under a
/// dev checkout there.
pub fn socket_name() -> String {
    #[cfg(unix)]
    {
        if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
            if !dir.is_empty() {
                return format!("{dir}/oxyd.sock");
            }
        }
        return state_home()
            .join("omarchy/oxyd.sock")
            .to_string_lossy()
            .into_owned();
    }
    #[cfg(windows)]
    {
        "oxyd".to_string()
    }
}
