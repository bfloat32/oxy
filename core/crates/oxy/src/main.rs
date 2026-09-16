//! `oxy` — the core without the frontend.
//!
//!   oxy query [--local] TEXT   ask the engine a question, print the rows
//!   oxy test [EXT]             run each extension's testQuery against it
//!   oxy extensions             list the registry
//!
//! `--local` runs the engine in this process — how the tests exercise the
//! whole core without a daemon, and how a machine without oxyd running still
//! gets answers.

use std::io::Write;
use std::time::Duration;

use oxy_core::engine::{Engine, EngineCmd, EngineEvent};
use oxy_core::{dirs, extension};
use serde_json::{json, Value};
use tokio::sync::mpsc;

const USAGE: &str =
    "oxy query [--local] TEXT | oxy test [EXT] | oxy extensions | oxy send";

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("query") => query(&args[1..]).await,
        Some("test") => test(&args[1..]).await,
        Some("extensions") => extensions().await,
        Some("send") => send_raw().await,
        _ => {
            eprintln!("{USAGE}");
            2
        }
    };
    std::process::exit(code);
}

/// An engine running in this process: the same pipeline the daemon hosts,
/// with the command channel fed by the command line.
async fn local_engine() -> (
    mpsc::Sender<EngineCmd>,
    mpsc::Receiver<EngineEvent>,
    tokio::task::JoinHandle<()>,
) {
    let (cmd_tx, cmd_rx) = mpsc::channel(64);
    let (evt_tx, evt_rx) = mpsc::channel(512);
    let (worker_tx, worker_rx) = mpsc::channel(512);
    let engine = Engine::start(
        &dirs::extensions_dir(),
        cmd_rx,
        evt_tx,
        worker_tx,
        oxy_core::native::construct,
    )
    .await;
    let task = tokio::spawn(async move { engine.run(worker_rx).await });
    (cmd_tx, evt_rx, task)
}

/// Ask a question, collect answers until every waiter has spoken or the clock
/// runs out, print the merged list.
async fn query(args: &[String]) -> i32 {
    let local = args.iter().any(|a| a == "--local");
    let text = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");

    if !local {
        match query_daemon(&text).await {
            Ok(code) => return code,
            Err(_) => {
                // No daemon is a normal state for a dev checkout, not an
                // error: fall back to the in-process engine.
            }
        }
    }

    let (tx, mut rx, _task) = local_engine().await;
    let _ = tx
        .send(EngineCmd::Query {
            text: text.clone(),
            opened: true,
        })
        .await;

    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    let mut last: Option<Value> = None;
    while let Ok(Some(event)) = tokio::time::timeout_at(deadline.into(), rx.recv()).await {
        if let EngineEvent::Results {
            epoch,
            rows,
            waiting,
            scope,
            ..
        } = &event
        {
            last = Some(json!({
                "epoch": epoch,
                "scope": scope,
                "waiting": waiting,
                "rows": rows,
            }));
            if waiting.is_empty() {
                break;
            }
        }
    }
    match last {
        Some(v) => {
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            0
        }
        None => {
            eprintln!("oxy: no answer");
            1
        }
    }
}

/// A raw client for the daemon: commands from stdin, events to stdout, one
/// JSON line each way — the same wire `Shell.qml` speaks, for debugging the
/// protocol and for integrators poking at it.
async fn send_raw() -> i32 {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let stream = match connect_daemon().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("oxy send: {e} (is oxyd running?)");
            return 1;
        }
    };
    let (reader, mut writer) = tokio::io::split(stream);
    let mut out_lines = BufReader::new(reader).lines();

    // Forward stdin lines to the socket; when stdin closes, keep reading for
    // a moment so the last command's events still print.
    tokio::spawn(async move {
        let stdin = tokio::io::stdin();
        let mut in_lines = BufReader::new(stdin).lines();
        while let Ok(Some(line)) = in_lines.next_line().await {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if writer.write_all(line.as_bytes()).await.is_err()
                || writer.write_all(b"\n").await.is_err()
                || writer.flush().await.is_err()
            {
                return;
            }
        }
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while let Ok(Ok(Some(line))) =
        tokio::time::timeout_at(deadline.into(), out_lines.next_line()).await
    {
        println!("{line}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
    0
}

/// One connection to the daemon, whichever shape the platform gives it.
async fn connect_daemon() -> std::io::Result<interprocess::local_socket::tokio::Stream> {
    use interprocess::local_socket::tokio::prelude::*;

    #[cfg(unix)]
    {
        use interprocess::local_socket::{GenericFilePath, ToFsName};
        let name = dirs::socket_name().to_fs_name::<GenericFilePath>()?;
        interprocess::local_socket::tokio::Stream::connect(name).await
    }
    #[cfg(windows)]
    {
        use interprocess::local_socket::{GenericNamespaced, ToNsName};
        let name = dirs::socket_name().to_ns_name::<GenericNamespaced>()?;
        interprocess::local_socket::tokio::Stream::connect(name).await
    }
}

/// The daemon route: connect, ask, print the first full answer.
async fn query_daemon(text: &str) -> std::io::Result<i32> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let stream = connect_daemon().await?;
    let (reader, mut writer) = tokio::io::split(stream);
    writer
        .write_all(format!("{{\"op\":\"query\",\"text\":{}}}\n", json!(text)).as_bytes())
        .await?;
    let mut lines = BufReader::new(reader).lines();
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    while let Ok(Ok(Some(line))) = tokio::time::timeout_at(deadline.into(), lines.next_line()).await
    {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("op").and_then(|o| o.as_str()) == Some("results")
            && v.get("waiting")
                .and_then(|w| w.as_array())
                .is_some_and(|w| w.is_empty())
        {
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            return Ok(0);
        }
    }
    Ok(1)
}

/// Every extension's `testQuery`, run through the in-process engine — the
/// Rust counterpart of `tests/cases.py`'s live checks.
async fn test(args: &[String]) -> i32 {
    let only = args.first().cloned();
    let settings = oxy_core::settings::Settings::load(&dirs::settings_file());
    let report = extension::load_dir(&dirs::extensions_dir(), &settings.extensions);
    let mut failures = 0;

    for ext in &report.extensions {
        if let Some(only) = &only {
            if ext.id != *only {
                continue;
            }
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
            if let EngineEvent::Results { rows, waiting, .. } = &event {
                if !waiting.contains(&ext.id) {
                    answered = Some(rows.len());
                    break;
                }
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
    if report.extensions.is_empty() {
        eprintln!(
            "oxy test: no extensions found under {}",
            dirs::extensions_dir().display()
        );
        return 1;
    }
    failures
}

async fn extensions() -> i32 {
    let settings = oxy_core::settings::Settings::load(&dirs::settings_file());
    let report = extension::load_dir(&dirs::extensions_dir(), &settings.extensions);
    let mut out = Vec::new();
    for ext in &report.extensions {
        out.push(json!({
            "id": ext.id,
            "keyword": ext.keyword,
            "title": ext.title,
            "aliases": ext.aliases,
            "view": ext.view,
            "tier": oxy_core::rank::tier_name(ext.tier),
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
