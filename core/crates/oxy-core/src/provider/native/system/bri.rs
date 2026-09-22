//! `bri:` — the screen brightness slider. A port of `bin/oxy-brightness`.
//!
//!   bri:             the slider
//!   bri:dim          the same row — the query is a word match, not a setting
//!
//! `omarchy-brightness-display` does both directions: with no argument it
//! prints the current percentage, and called with "40%" it sets it and draws
//! the OSD. It owns the three ways a display is dimmed — brightnessctl for
//! an internal panel, DDC over the monitor's own channel for an external
//! one, the Apple protocol for a Studio Display — so `setExec`/`exec` call
//! it too and this provider owns none of that.
//!
//! Two traps the script's header records are kept here: the floor is 1, not
//! 0 — zero on a backlight is a black screen with no way to find the slider
//! again — and a non-numeric reading is silence rather than an error row,
//! because a monitor that answers no control channel simply has nothing to
//! show.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::native::util::on_path;
use crate::provider::process::run;
use crate::provider::{Ctx, NativeExt, NativeOutcome};

pub struct Bri;

/// The manifest's `when`: `command -v omarchy-brightness-display`. A native
/// provider is asked even when the gate fails, so it is re-checked before
/// anything spawns (PORTING-BACKLOG §1.3).
const TOOL: &str = "omarchy-brightness-display";

/// Every reading gets its own bound — a native run has no worker deadline.
const CALL: Duration = Duration::from_secs(2);

/// The word list the script substring-matches the query against.
const WORDS: &str = "brightness screen display dim backlight";

/// `bri:` answers on an empty query or any word in the list — the script's
/// `[[ WORDS == *"$needle"* ]]`, the needle already lowercased.
fn matches(needle: &str) -> bool {
    needle.is_empty() || WORDS.contains(needle)
}

/// What the reading must look like: digits and nothing else — the script's
/// `=~ ^[0-9]+$` over what command substitution left (it strips trailing
/// newlines only). The canonical check is the other half of the script's
/// contract: jq's `--argjson` rejects a leading zero just as surely as it
/// rejects letters, so "065" is silence rather than a row.
fn level(stdout: &str) -> Option<u64> {
    let text = stdout.trim_end_matches('\n');
    let value = text.parse::<u64>().ok()?;
    (value.to_string() == text).then_some(value)
}

/// The one row, exactly the object the script's jq emitted.
fn row(monitor: &str, value: u64) -> Value {
    json!({
        "id": "brightness",
        "title": "Screen Brightness",
        "subtitle": monitor,
        "accessory": format!("{value}%"),
        "group": "Brightness",
        "view": "slider",
        "value": value,
        // 1, not 0: zero on a backlight is a black screen with no way back.
        "min": 1,
        "max": 100,
        "step": 5,
        "setExec": "omarchy-brightness-display {value}%",
        "exec": "omarchy-brightness-display 100%",
        "score": 90000,
        "actions": [
            { "title": "Set to 100%", "shortcut": "↵", "exec": "omarchy-brightness-display 100%" },
            { "title": "Set to 50%", "exec": "omarchy-brightness-display 50%" },
            { "title": "Set to 10%", "exec": "omarchy-brightness-display 10%" },
            { "title": "Turn Display Off", "exec": "omarchy-brightness-display off" },
        ],
    })
}

impl NativeExt for Bri {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        let needle = ctx.arg.to_lowercase();
        Box::pin(async move {
            if !on_path(TOOL) {
                return NativeOutcome::Fallback;
            }
            if !matches(&needle) {
                return NativeOutcome::Empty;
            }

            // The reading and the monitor name are independent — joined, so
            // two bounded calls cannot sum past the manifest's window.
            let (level_out, mon_out) = tokio::join!(
                run(TOOL, CALL),
                run("omarchy-hyprland-monitor-focused", CALL),
            );

            let Some(value) = level_out.and_then(|f| level(&f.stdout)) else {
                // A desktop with a monitor that answers no control channel
                // has no reading at all — not an error, not a row saying so.
                return NativeOutcome::Empty;
            };

            // The monitor name is context, never something that fails.
            let monitor = mon_out
                .map(|f| f.stdout.trim_end_matches('\n').to_string())
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| "Display".to_string());

            NativeOutcome::Rows(vec![row(&monitor, value)])
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_reads_what_the_script_accepted() {
        assert_eq!(level("65\n"), Some(65));
        assert_eq!(level("65"), Some(65));
        assert_eq!(level("1"), Some(1));
        assert_eq!(level("0"), Some(0));
        assert_eq!(level("100\n"), Some(100));
        // Command substitution strips every trailing newline.
        assert_eq!(level("65\n\n\n"), Some(65));
    }

    #[test]
    fn level_is_silent_on_anything_else() {
        // Every one of these was `exit 0` with no output in the script.
        assert_eq!(level(""), None);
        assert_eq!(level("\n"), None);
        assert_eq!(level("abc"), None);
        assert_eq!(level("65%"), None);
        assert_eq!(level("65 "), None);
        assert_eq!(level(" 65"), None);
        assert_eq!(level("6\n5"), None);
        // jq's --argjson rejects a leading zero; so does the port.
        assert_eq!(level("065"), None);
    }

    #[test]
    fn the_query_is_a_substring_against_the_word_list() {
        assert!(matches(""));
        assert!(matches("dim"));
        assert!(matches("screen"));
        assert!(matches("backlight"));
        assert!(matches("rightness")); // a substring still counts
        assert!(!matches("volume"));
        assert!(!matches("displayport")); // contains "display" as a prefix, not vice versa
        assert!(matches("display"));
    }

    #[test]
    fn the_row_is_the_scripts() {
        let row = row("DP-1", 65);
        assert_eq!(row["id"], json!("brightness"));
        assert_eq!(row["title"], json!("Screen Brightness"));
        assert_eq!(row["subtitle"], json!("DP-1"));
        assert_eq!(row["accessory"], json!("65%"));
        assert_eq!(row["group"], json!("Brightness"));
        assert_eq!(row["view"], json!("slider"));
        assert_eq!(row["value"], json!(65));
        assert_eq!(row["min"], json!(1));
        assert_eq!(row["max"], json!(100));
        assert_eq!(row["step"], json!(5));
        assert_eq!(row["setExec"], json!("omarchy-brightness-display {value}%"));
        assert_eq!(row["exec"], json!("omarchy-brightness-display 100%"));
        assert_eq!(row["score"], json!(90000));

        let actions = row["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 4);
        assert_eq!(actions[0]["title"], json!("Set to 100%"));
        assert_eq!(actions[0]["shortcut"], json!("↵"));
        assert_eq!(actions[0]["exec"], json!("omarchy-brightness-display 100%"));
        assert_eq!(actions[1]["exec"], json!("omarchy-brightness-display 50%"));
        assert_eq!(actions[2]["exec"], json!("omarchy-brightness-display 10%"));
        assert_eq!(actions[3]["title"], json!("Turn Display Off"));
        assert_eq!(actions[3]["exec"], json!("omarchy-brightness-display off"));
    }
}
