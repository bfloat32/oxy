//! `vol:` — output and input volume sliders. A port of `bin/oxy-volume`.
//!
//!   vol:             both sliders
//!   vol:mic          just the input one
//!
//! Each row carries `setExec` holding the literal token `{value}` for the
//! view to substitute, and that call still goes to `oxy-volume set …` — the
//! script owns the set path (validating the number, unmuting, drawing the
//! OSD), so the port reads and leaves the writing to it. Enter on a row
//! toggles that device's mute, the one thing a slider cannot express.
//!
//! Traps the script's header records, kept here:
//!   * the output reading is the *chosen* sink — `omarchy-audio-output-sink`,
//!     not the default — so a speaker tuning or an EasyEffects sink in front
//!     of the hardware does not swallow the change (setting the DSP sink's
//!     volume moves the number going *into* the processing while the
//!     speakers do not);
//!   * the input reads `wpctl get-volume @DEFAULT_AUDIO_SOURCE@`, not pactl,
//!     because that is what `omarchy-audio-input-mute` drives and the two
//!     must agree about which microphone is the default.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::native::util::on_path;
use crate::provider::process::run;
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::support::quote::quote;

pub struct Vol;

/// Every reading gets its own bound, sized so the two sequential reads of
/// the output chain fit the manifest's 3s window when joined with input.
const CALL: Duration = Duration::from_millis(1500);

/// The script's per-row word lists: a bare `vol:` answers both rows, a word
/// answers whichever list contains it — `[[ ${words,,} == *"$needle"* ]]`.
fn matches(needle: &str, words: &str) -> bool {
    needle.is_empty() || words.contains(needle)
}

/// `pactl get-sink-volume`: the first whitespace field ending in '%' on the
/// first line, minus its '%' — the script's awk verbatim. `None` is "no such
/// field", which the script's `${level:-0}` reads as zero.
fn sink_percent(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .find(|f| f.ends_with('%'))
        .map(|f| f.replacen('%', "", 1))
}

/// `pactl get-sink-mute` — the script's `== *yes` glob: after command
/// substitution strips the trailing newline, the output ends with the word.
fn sink_muted(stdout: &str) -> bool {
    stdout.trim_end_matches('\n').ends_with("yes")
}

/// The sink's display name is its tail — `alsa_output.….analog-stereo`
/// draws as "analog-stereo" (the script's `${sink##*.}`).
fn short_name(sink: &str) -> &str {
    sink.rsplit('.').next().unwrap_or(sink)
}

/// `wpctl get-volume @DEFAULT_AUDIO_SOURCE@` reads `Volume: 0.65 [MUTED]`:
/// the level is field two times a hundred and the mute is the word MUTED
/// anywhere in the output, both out of the one call. The script's awk used
/// `%d`, which truncates — 0.58 came out as 57 — so the port rounds and the
/// slider lands where the mixer actually is. Empty or unrecognised output
/// is the `${level:-0}` default: 0, not muted.
fn wpctl_volume(stdout: &str) -> (u32, bool) {
    let level = stdout
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .and_then(|f| f.parse::<f64>().ok())
        .map(|v| (v * 100.0).round())
        .unwrap_or(0.0);
    (
        level.clamp(0.0, u32::MAX as f64) as u32,
        stdout.contains("MUTED"),
    )
}

/// The output slider, exactly the object the script's `slider` emitted.
/// `value` is the reading parsed as JSON — `--argjson` in the script, so a
/// %-field that is not a number fails the whole row and no row is emitted.
fn output_row(sink: &str, volume_out: &str, mute_out: &str) -> Option<Value> {
    let field = sink_percent(volume_out);
    let value = match &field {
        None => json!(0),                         // the script's ${level:-0}
        Some(s) => serde_json::from_str(s).ok()?, // what --argjson rejected
    };
    let accessory = if sink_muted(mute_out) {
        "Muted".to_string()
    } else {
        format!("{}%", field.as_deref().unwrap_or("0"))
    };
    Some(json!({
        "id": "output",
        "title": "Output Volume",
        "subtitle": short_name(sink),
        "accessory": accessory,
        "group": "Volume",
        "view": "slider",
        "value": value,
        "min": 0,
        "max": 100,
        "step": 5,
        "setExec": "oxy-volume set output {value}",
        "exec": "omarchy-audio-output-volume mute-toggle",
        "score": 90000,
        "actions": [
            { "title": "Toggle Mute", "shortcut": "↵", "exec": "omarchy-audio-output-volume mute-toggle" },
            { "title": "Set to 100%", "exec": "oxy-volume set output 100" },
            { "title": "Set to 50%", "exec": "oxy-volume set output 50" },
            { "title": "Set to 0%", "exec": "oxy-volume set output 0" },
            { "title": "Switch Output Device", "exec": "omarchy-audio-output-switch" },
        ],
    }))
}

/// The input slider — the same shape, the microphone's own names.
fn input_row(wpctl_out: &str) -> Value {
    let (level, muted) = wpctl_volume(wpctl_out);
    json!({
        "id": "input",
        "title": "Input Volume",
        "subtitle": "Microphone",
        "accessory": if muted { "Muted".to_string() } else { format!("{level}%") },
        "group": "Volume",
        "view": "slider",
        "value": level,
        "min": 0,
        "max": 100,
        "step": 5,
        "setExec": "oxy-volume set input {value}",
        "exec": "omarchy-audio-input-mute",
        "score": 89000,
        "actions": [
            { "title": "Toggle Mute", "shortcut": "↵", "exec": "omarchy-audio-input-mute" },
            { "title": "Set to 100%", "exec": "oxy-volume set input 100" },
            { "title": "Set to 50%", "exec": "oxy-volume set input 50" },
            { "title": "Set to 0%", "exec": "oxy-volume set input 0" },
            { "title": "Switch Input Device", "exec": "omarchy-audio-source-switch" },
        ],
    })
}

impl NativeExt for Vol {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        let needle = ctx.arg.to_lowercase();
        Box::pin(async move {
            // The manifest's `when`: `command -v pactl`. A native provider is
            // asked even when the gate fails, so it is re-checked before
            // anything spawns (PORTING-BACKLOG §1.3).
            if !on_path("pactl") {
                return NativeOutcome::Fallback;
            }
            let want_out = matches(&needle, "volume output speaker sound mute");
            // wpctl rather than pactl for the source, because that is what
            // omarchy-audio-input-mute drives and both have to agree about
            // which mic is the default one.
            let want_in = on_path("wpctl") && matches(&needle, "volume input microphone mic mute");

            // The two chains are independent; joined, their sequential
            // reads fit the manifest's window rather than summing past it.
            let out_chain = async {
                if !want_out {
                    return None;
                }
                let sink = run("omarchy-audio-output-sink", CALL)
                    .await
                    .map(|f| f.stdout.trim().to_string())
                    .unwrap_or_default();
                if sink.is_empty() {
                    return None;
                }
                let quoted = quote(&sink);
                let get_volume = format!("pactl get-sink-volume {quoted}");
                let get_mute = format!("pactl get-sink-mute {quoted}");
                let (volume, mute) = tokio::join!(run(&get_volume, CALL), run(&get_mute, CALL));
                let volume = volume.map(|f| f.stdout).unwrap_or_default();
                let mute = mute.map(|f| f.stdout).unwrap_or_default();
                output_row(&sink, &volume, &mute)
            };
            let in_chain = async {
                if !want_in {
                    return String::new();
                }
                run("wpctl get-volume @DEFAULT_AUDIO_SOURCE@", CALL)
                    .await
                    .map(|f| f.stdout)
                    .unwrap_or_default()
            };
            let (out_row, in_out) = tokio::join!(out_chain, in_chain);

            let mut rows = Vec::new();
            if let Some(row) = out_row {
                rows.push(row);
            }
            if want_in {
                rows.push(input_row(&in_out));
            }
            NativeOutcome::Rows(rows)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sink_percent_is_the_first_percent_field_on_line_one() {
        let pactl = "Volume: front-left: 65536 / 65% / -10.50 dB,   \
                     front-right: 65536 / 65% / -10.50 dB\n\
                     Balance: 0.00\n";
        assert_eq!(sink_percent(pactl).as_deref(), Some("65"));
        assert_eq!(
            sink_percent("Volume: mono: 98304 / 150% / 0.00 dB").as_deref(),
            Some("150")
        );
        assert_eq!(sink_percent(""), None);
        assert_eq!(sink_percent("Volume: no percent here"), None);
        // awk reads NR == 1 only — a % on line two is not a reading.
        assert_eq!(sink_percent("Volume: pending\nVolume: 50%"), None);
    }

    #[test]
    fn sink_muted_is_the_trailing_yes() {
        assert!(sink_muted("Mute: yes"));
        assert!(sink_muted("Mute: yes\n"));
        assert!(!sink_muted("Mute: no"));
        assert!(!sink_muted("Mute: no\n"));
        assert!(!sink_muted(""));
    }

    #[test]
    fn wpctl_volume_reads_field_two_and_the_muted_word() {
        assert_eq!(wpctl_volume("Volume: 0.65"), (65, false));
        assert_eq!(wpctl_volume("Volume: 0.65 [MUTED]"), (65, true));
        assert_eq!(wpctl_volume("Volume: 0.00 [MUTED]"), (0, true));
        assert_eq!(wpctl_volume("Volume: 1.50"), (150, false));
        // The script's %d truncated to 57; the port rounds to where the
        // mixer is.
        assert_eq!(wpctl_volume("Volume: 0.58"), (58, false));
        // Empty and unrecognised output both read as the "0" default.
        assert_eq!(wpctl_volume(""), (0, false));
        assert_eq!(wpctl_volume("garbage"), (0, false));
        assert_eq!(wpctl_volume("Volume:"), (0, false));
        assert_eq!(wpctl_volume("Volume: --"), (0, false));
    }

    #[test]
    fn each_row_has_its_own_word_list() {
        const OUT: &str = "volume output speaker sound mute";
        const IN: &str = "volume input microphone mic mute";
        assert!(matches("", OUT) && matches("", IN));
        assert!(matches("speaker", OUT) && !matches("speaker", IN));
        assert!(!matches("mic", OUT) && matches("mic", IN));
        assert!(matches("mute", OUT) && matches("mute", IN));
        assert!(!matches("brightness", OUT) && !matches("brightness", IN));
    }

    #[test]
    fn the_output_row_is_the_scripts() {
        let row = output_row(
            "alsa_output.pci-0000_00_1f.3.analog-stereo",
            "Volume: front-left: 65536 / 65% / -10.50 dB,   \
             front-right: 65536 / 65% / -10.50 dB",
            "Mute: no",
        )
        .unwrap();
        assert_eq!(row["id"], json!("output"));
        assert_eq!(row["title"], json!("Output Volume"));
        // The subtitle is the sink's tail — ${sink##*.}.
        assert_eq!(row["subtitle"], json!("analog-stereo"));
        assert_eq!(row["accessory"], json!("65%"));
        assert_eq!(row["group"], json!("Volume"));
        assert_eq!(row["view"], json!("slider"));
        assert_eq!(row["value"], json!(65));
        assert_eq!(row["min"], json!(0));
        assert_eq!(row["max"], json!(100));
        assert_eq!(row["step"], json!(5));
        assert_eq!(row["setExec"], json!("oxy-volume set output {value}"));
        assert_eq!(
            row["exec"],
            json!("omarchy-audio-output-volume mute-toggle")
        );
        assert_eq!(row["score"], json!(90000));

        let actions = row["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 5);
        assert_eq!(actions[0]["title"], json!("Toggle Mute"));
        assert_eq!(actions[0]["shortcut"], json!("↵"));
        assert_eq!(
            actions[0]["exec"],
            json!("omarchy-audio-output-volume mute-toggle")
        );
        assert_eq!(actions[1]["exec"], json!("oxy-volume set output 100"));
        assert_eq!(actions[2]["exec"], json!("oxy-volume set output 50"));
        assert_eq!(actions[3]["exec"], json!("oxy-volume set output 0"));
        assert_eq!(actions[4]["exec"], json!("omarchy-audio-output-switch"));
    }

    #[test]
    fn the_output_row_says_muted_or_defaults_to_zero() {
        let row = output_row("sink.one", "Volume: 40% / 0 dB", "Mute: yes").unwrap();
        assert_eq!(row["accessory"], json!("Muted"));
        assert_eq!(row["value"], json!(40));
        // A sink with no readable level still answers — at 0.
        let row = output_row("sink.one", "Volume: pending", "Mute: no").unwrap();
        assert_eq!(row["value"], json!(0));
        assert_eq!(row["accessory"], json!("0%"));
        // A %-field that is not a number is what jq --argjson rejected:
        // the script printed no row at all.
        assert!(output_row("sink.one", "Volume: abc% / 0 dB", "Mute: no").is_none());
    }

    #[test]
    fn the_input_row_is_the_scripts() {
        let row = input_row("Volume: 0.65 [MUTED]");
        assert_eq!(row["id"], json!("input"));
        assert_eq!(row["title"], json!("Input Volume"));
        assert_eq!(row["subtitle"], json!("Microphone"));
        assert_eq!(row["accessory"], json!("Muted"));
        assert_eq!(row["group"], json!("Volume"));
        assert_eq!(row["view"], json!("slider"));
        assert_eq!(row["value"], json!(65));
        assert_eq!(row["min"], json!(0));
        assert_eq!(row["max"], json!(100));
        assert_eq!(row["step"], json!(5));
        assert_eq!(row["setExec"], json!("oxy-volume set input {value}"));
        assert_eq!(row["exec"], json!("omarchy-audio-input-mute"));
        assert_eq!(row["score"], json!(89000));

        let actions = row["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 5);
        assert_eq!(actions[0]["exec"], json!("omarchy-audio-input-mute"));
        assert_eq!(actions[1]["exec"], json!("oxy-volume set input 100"));
        assert_eq!(actions[2]["exec"], json!("oxy-volume set input 50"));
        assert_eq!(actions[3]["exec"], json!("oxy-volume set input 0"));
        assert_eq!(actions[4]["exec"], json!("omarchy-audio-source-switch"));

        let row = input_row("Volume: 0.30");
        assert_eq!(row["accessory"], json!("30%"));
        assert_eq!(row["value"], json!(30));
        // No reading at all is still a row, at 0 — the script's ${level:-0}.
        let row = input_row("");
        assert_eq!(row["accessory"], json!("0%"));
        assert_eq!(row["value"], json!(0));
    }

    #[test]
    fn the_sink_subtitle_is_the_name_after_the_last_dot() {
        assert_eq!(
            short_name("alsa_output.pci-0.analog-stereo"),
            "analog-stereo"
        );
        assert_eq!(short_name("easyeffects_sink"), "easyeffects_sink");
        assert_eq!(short_name("a."), "");
    }
}
