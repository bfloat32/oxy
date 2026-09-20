//! `oxy extensions` — what is installed, and `--coverage`, how far it has
//! been ported and tested.
//!
//! The JSON listing is the machine-readable one (it is what the audits and
//! the docs' tables were built from). `--coverage` is the human view of the
//! same data: which leg answers (the compiled-in provider or the script), how
//! many cases pin it, and whether it has a `when` gate that can skip it here.

use std::io::Write;

use oxy_core::{registry as extension, settings::paths as dirs};
use serde_json::{Value, json};

pub(crate) async fn run(args: &[String]) -> i32 {
    let settings = oxy_core::settings::Settings::load(&dirs::settings_file());
    let report = extension::load_dir(&dirs::extensions_dir(), &settings.extensions);

    if args.iter().any(|a| a == "--coverage") {
        return coverage(&report);
    }

    let mut out = Vec::new();
    for ext in &report.extensions {
        out.push(json!({
            "id": ext.id,
            "keyword": ext.keyword,
            "title": ext.title,
            "aliases": ext.aliases,
            "view": ext.view,
            "tier": oxy_core::support::rank::tier_name(ext.tier),
            "native": ext.native,
            "search": ext.search,
            "socket": ext.socket,
            "when": ext.when,
        }));
    }
    for (path, why) in &report.bad {
        out.push(json!({"bad": path.to_string_lossy(), "why": why}));
    }
    let stdout = std::io::stdout();
    let mut out_w = stdout.lock();
    let _ = writeln!(
        out_w,
        "{}",
        serde_json::to_string_pretty(&Value::Array(out)).unwrap_or_default()
    );
    0
}

/// The porting ledger: one line per extension, then the totals. Read-only —
/// it counts what is on disk rather than tracking progress anywhere.
fn coverage(report: &extension::LoadReport) -> i32 {
    let mut native = 0usize;
    let mut cases_files = 0usize;
    let mut assertions = 0usize;
    let mut malformed = 0usize;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "{:<16} {:<7} {:>5}  gate", "id", "leg", "cases");
    for ext in &report.extensions {
        let leg = if ext.native.is_empty() {
            "script"
        } else {
            native += 1;
            "native"
        };
        let cases_path = ext.source.with_extension("cases.json");
        let cases = match std::fs::read_to_string(&cases_path) {
            Ok(text) => match serde_json::from_str::<Vec<Value>>(&text) {
                Ok(list) => {
                    cases_files += 1;
                    assertions += list.len();
                    list.len().to_string()
                }
                Err(_) => {
                    malformed += 1;
                    "bad".to_string()
                }
            },
            Err(_) => "-".to_string(),
        };
        let gate = if ext.when.is_empty() {
            "-".to_string()
        } else {
            ext.when.clone()
        };
        let _ = writeln!(out, "{:<16} {leg:<7} {cases:>5}  {gate}", ext.id);
    }
    let total = report.extensions.len();
    let _ = writeln!(
        out,
        "\n{total} extensions: {native} native, {} script-backed, {cases_files} with cases ({assertions} assertions)",
        total - native
    );
    for (path, why) in &report.bad {
        let _ = writeln!(out, "unreadable: {} ({why})", path.display());
    }
    if malformed > 0 {
        let _ = writeln!(out, "{malformed} cases file(s) do not parse");
    }
    i32::from(!report.bad.is_empty() || malformed > 0)
}
