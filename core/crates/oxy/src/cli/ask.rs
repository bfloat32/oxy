//! `oxy ask "question"` — the ask track from a terminal.
//!
//! The card is the usual way to ask a local model; this is the same client
//! without the card, so the feature can be used in a terminal, piped into
//! something else, or run in CI. It answers through `ask.endpoint` only: the
//! CLI providers are commands the daemon runs for the card, and pretending
//! this verb drives them would be a second implementation of the same thing.
//!
//! `oxy ask doctor` is the diagnostic, in `cli/doctor.rs`.

use std::io::Write;

use oxy_core::provider::llm::Local;
use oxy_core::provider::llm::turn::{self, Piece};
use oxy_core::settings::paths as dirs;

pub(crate) async fn run(args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some("doctor") {
        return crate::cli::doctor::run(&args[1..]).await;
    }

    let question = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    if question.trim().is_empty() {
        eprintln!(
            "usage: oxy ask \"question\"   |   oxy ask doctor [--tier offline|catalog] [--json]"
        );
        return 2;
    }

    let settings = oxy_core::settings::Settings::load(&dirs::settings_file());
    let Some(local) = Local::from_settings(&settings.ask) else {
        eprintln!(
            "oxy ask: no ask.endpoint in oxy.json — the card's CLI providers are the daemon's, not this verb's"
        );
        return 1;
    };

    let mut pieces = turn::spawn(local, Vec::new(), question);
    let mut ends_with_newline = true;
    let mut error = String::new();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    while let Some(piece) = pieces.recv().await {
        match piece {
            // Straight out as it arrives: a terminal wants the stream, and
            // the card is where line buffering belongs.
            Piece::Text(text) => {
                let _ = out.write_all(text.as_bytes());
                let _ = out.flush();
                ends_with_newline = text.ends_with('\n');
            }
            // stderr, so `oxy ask "…" > answer.txt` holds the answer alone.
            Piece::Notice(line) => eprintln!("{line}"),
            Piece::Error(why) => {
                error = why;
                break;
            }
        }
    }
    if !ends_with_newline {
        let _ = writeln!(out);
    }
    if !error.is_empty() {
        eprintln!("oxy ask: {error}");
        return 1;
    }
    0
}
