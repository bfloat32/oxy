mod check;
mod view;

use std::time::Duration;

use oxy_core::engine::{EngineCmd, EngineEvent};
use oxy_core::{registry as extension, settings::paths as dirs};
use serde_json::{Value, json};

use self::check::check_case;
use self::view::{case_preflight, case_view};
use crate::engine_local::local_engine;

/// `oxy test --cases [EXT]`: the shipped `*.cases.json` fixtures run through
/// the engine — the assertions `tests/cases.py` makes against the scripts,
/// made against the rows the merged pipeline actually returns. The same
/// contract holds: cases whose extension cannot run here skip rather than
/// fail, and a row assertion only checks when the row exists.
///
/// `--json` swaps the human lines for one object, so a CI job can read the
/// result instead of parsing prose.
pub(crate) async fn run(only: Option<String>, json: bool) -> i32 {
    let settings = oxy_core::settings::Settings::load(&dirs::settings_file());
    let report = extension::load_dir(&dirs::extensions_dir(), &settings.extensions);
    let mut held = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;
    let mut lines: Vec<String> = Vec::new();
    let mut failures: Vec<Value> = Vec::new();

    // One engine for the whole run — each case is just the next query.
    let (tx, mut rx, _task) = local_engine().await;
    // Every EngineCmd::Query bumps the engine's epoch by one. Counting our
    // own sends gives the epoch each case's Results must carry — a worker
    // still finishing the previous case can land a stale Results after the
    // drain, and without this check its empty `waiting` reads as an answer.
    let mut epoch = 0u64;

    for ext in &report.extensions {
        // The filter takes the id or the file's own name: `windows.json`
        // holds `"id": "win"`, `tests/cases.py` has always matched the file,
        // and both are names a person would type.
        let stem = ext
            .source
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if let Some(only) = &only
            && ext.id != *only
            && stem != only.as_str()
        {
            continue;
        }
        let cases_path = ext.source.with_extension("cases.json");
        let Ok(text) = std::fs::read_to_string(&cases_path) else {
            continue;
        };
        let cases: Vec<Value> = match serde_json::from_str(&text) {
            Ok(c) => c,
            Err(e) => {
                lines.push(format!("FAIL {} cases: {e}", ext.id));
                failures.push(json!({ "ext": ext.id, "problem": e.to_string() }));
                failed += 1;
                continue;
            }
        };

        // The same "cannot answer here is a skip, not a failure" gates
        // cases.py applies — for the script leg. A native provider answers
        // in-process whatever `when` and `search` say, so those cases still
        // run; ones needing the fallback report honestly when the machine
        // lacks the script.
        if ext.native.is_empty() {
            if !ext.when.is_empty() && !oxy_core::provider::process::check(&ext.when).await {
                lines.push(format!("skip {} cases (when fails here)", ext.id));
                skipped += 1;
                continue;
            }
            let cmd = ext.search.split_whitespace().next().unwrap_or("");
            if cmd.is_empty()
                || !oxy_core::provider::process::check(&format!("command -v {cmd}")).await
            {
                lines.push(format!("skip {} cases ({cmd} not on PATH)", ext.id));
                skipped += 1;
                continue;
            }
        }
        if !case_preflight(&ext.id).await {
            lines.push(format!(
                "skip {} cases (its data service is unreachable here)",
                ext.id
            ));
            skipped += 1;
            continue;
        }

        let mut problems = 0usize;
        let mut skipped_cases = 0usize;
        for case in &cases {
            let why = case.get("why").and_then(|w| w.as_str()).unwrap_or("");
            let query = case.get("query").and_then(|q| q.as_str()).unwrap_or("");
            // A case can build what it needs first: `setup` is the fixture,
            // its failure is a failure. `requires` is a probe, and failing it
            // skips — the two are different, and a fixture written through
            // `requires` made the notes suite depend on its own order.
            if let Some(setup) = case.get("setup").and_then(|s| s.as_str())
                && !setup.trim().is_empty()
            {
                let ok = matches!(
                    oxy_core::provider::process::run(setup, Duration::from_secs(15)).await,
                    Some(f) if f.code == Some(0)
                );
                if !ok {
                    problems += 1;
                    lines.push(format!("FAIL {} case: setup failed", ext.id));
                    failures.push(json!({
                        "ext": ext.id, "query": query, "problem": "setup failed", "why": why,
                    }));
                    continue;
                }
            }
            if let Some(requires) = case.get("requires").and_then(|r| r.as_str())
                && !requires.trim().is_empty()
                && !oxy_core::provider::process::check(requires).await
            {
                skipped_cases += 1;
                continue;
            }
            let text = format!("{}:{query}", ext.keyword);
            // Results queued by the previous case are its answer, not this
            // one's — drain them so the loop below only sees this epoch.
            while rx.try_recv().is_ok() {}
            if tx
                .send(EngineCmd::Query {
                    text: text.clone(),
                    opened: true,
                })
                .await
                .is_err()
            {
                break;
            }
            epoch += 1;

            let deadline = std::time::Instant::now()
                + Duration::from_millis((ext.timeout_ms + ext.debounce_ms + 4000).max(8000));
            let mut rows: Vec<Value> = Vec::new();
            while let Ok(Some(event)) = tokio::time::timeout_at(deadline.into(), rx.recv()).await {
                if let EngineEvent::Results {
                    epoch: ep,
                    rows: r,
                    waiting,
                    ..
                } = event
                {
                    // Stale Results from an earlier case's stragglers are
                    // not this case's answer.
                    if ep != epoch {
                        continue;
                    }
                    rows = r
                        .iter()
                        .filter(|row| row.provider_id == ext.id)
                        .map(|row| case_view(row, &ext.id))
                        .collect();
                    if !waiting.iter().any(|w| &**w == ext.id.as_str()) {
                        break;
                    }
                }
            }

            for prob in check_case(case, &rows) {
                problems += 1;
                lines.push(format!("FAIL {} case: {prob}", ext.id));
                if !why.is_empty() {
                    lines.push(format!("        ({why})"));
                }
                failures.push(json!({
                    "ext": ext.id,
                    "query": query,
                    "problem": prob,
                    "why": why,
                }));
            }
        }
        if problems > 0 {
            failed += problems;
        } else if skipped_cases == cases.len() {
            lines.push(format!("skip {} cases (requires not met here)", ext.id));
            skipped += 1;
        } else {
            held += 1;
            let tail = if skipped_cases > 0 {
                format!(", {skipped_cases} skipped")
            } else {
                String::new()
            };
            lines.push(format!(
                "ok   {} cases  {} held{tail}",
                ext.id,
                cases.len() - skipped_cases
            ));
        }
    }

    if json {
        let summary = json!({
            "held": held,
            "broke": failed,
            "skipped": skipped,
            "failures": failures,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&summary).unwrap_or_default()
        );
    } else {
        for line in &lines {
            println!("{line}");
        }
        println!("{held} held, {failed} broke, {skipped} skipped");
    }
    if failed > 0 { 1 } else { 0 }
}
