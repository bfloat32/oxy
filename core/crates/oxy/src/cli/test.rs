use std::time::Duration;

use oxy_core::engine::{EngineCmd, EngineEvent};
use oxy_core::{registry as extension, settings::paths as dirs};

use crate::engine_local::local_engine;

/// Every extension's `testQuery`, run through the in-process engine — the
/// Rust counterpart of `tests/cases.py`'s live checks.
///
/// `--only manifest` swaps the question for a read-only look at the manifests
/// themselves, and `--only cases` is the same as `--cases`. The layer's own
/// value is not an extension name, so the positional argument is read around
/// it rather than by "first thing that is not a flag".
pub(crate) async fn run(args: &[String]) -> i32 {
    let mut layer: Option<String> = None;
    let mut only: Option<String> = None;
    let mut cases = false;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--only" => {
                layer = args.get(i + 1).cloned();
                i += 2;
                continue;
            }
            "--cases" => cases = true,
            "--json" => json = true,
            other if !other.starts_with("--") => {
                // The first positional is the extension to test; a second is
                // ignored, the way every other verb takes one name.
                only.get_or_insert_with(|| other.to_string());
            }
            _ => {}
        }
        i += 1;
    }
    if let Some(layer) = layer {
        return match layer.as_str() {
            "manifest" => crate::cli::manifest::run(only.as_deref(), json).await,
            "cases" => crate::cases::run(only, json).await,
            other => {
                eprintln!("oxy test: --only {other} is not implemented (manifest, cases are)");
                2
            }
        };
    }
    if cases {
        return crate::cases::run(only, json).await;
    }
    let settings = oxy_core::settings::Settings::load(&dirs::settings_file());
    let report = extension::load_dir(&dirs::extensions_dir(), &settings.extensions);
    let mut failures = 0;

    for ext in &report.extensions {
        if let Some(only) = &only
            && ext.id != *only
        {
            continue;
        }
        if ext.test_query.is_empty() {
            continue;
        }
        let query_text = format!("{}:{}", ext.keyword, ext.test_query);
        let (tx, mut rx, _task) = local_engine().await;
        let _ = tx
            .send(EngineCmd::Query {
                text: query_text.clone(),
                opened: true,
            })
            .await;

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut answered: Option<usize> = None;
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline.into(), rx.recv()).await {
            if let EngineEvent::Results { rows, waiting, .. } = &event
                && !waiting.iter().any(|w| &**w == ext.id.as_str())
            {
                answered = Some(rows.len());
                break;
            }
        }
        match answered {
            Some(0) | None => {
                failures += 1;
                println!("FAIL {} ({}): no rows", ext.id, query_text);
            }
            Some(n) => println!("ok   {} ({}): {} rows", ext.id, query_text, n),
        }
    }
    if let Some(only) = &only {
        if report.extensions.iter().all(|e| e.id != *only) {
            eprintln!("oxy test: no extension '{only}'");
            return 1;
        }
        if report
            .extensions
            .iter()
            .any(|e| e.id == *only && e.test_query.is_empty())
        {
            println!("{only}: no testQuery declared");
        }
    }
    if report.extensions.is_empty() {
        eprintln!(
            "oxy test: no extensions found under {}",
            dirs::extensions_dir().display()
        );
        return 1;
    }
    failures
}
