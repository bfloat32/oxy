//! Action — filled by the row split.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A choice on a row: Ctrl+K lists them, Shift+Enter runs the second.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Action {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub glyph: String,
    #[serde(default)]
    pub exec: String,
    /// A built-in the daemon performs itself rather than a command to run.
    #[serde(default)]
    pub effect: String,
    /// Asked in the box before the action fires.
    #[serde(default)]
    pub confirm: String,
    /// Drawn on the row, e.g. "↵".
    #[serde(default)]
    pub shortcut: String,
    /// Running it does not dismiss the launcher.
    #[serde(default, rename = "keepOpen")]
    pub keep_open: bool,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
