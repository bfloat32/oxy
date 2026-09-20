use std::time::Duration;

use oxy_core::engine::{EngineCmd, EngineEvent};
use oxy_core::{registry as extension, settings::paths as dirs};

use crate::engine_local::local_engine;

/// Every extension's `testQuery`, run through the in-process engine — the
/// Rust counterpart of `tests/cases.py`'s live checks.
pub(crate) async fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--cases") {
        let only = args.iter().find(|a| !a.starts_with("--")).cloned();
        return crate::cases::run(only).await;
    }
    let only = args.first().cloned();
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
