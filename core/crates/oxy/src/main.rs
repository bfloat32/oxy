//! `oxy` — the core without the frontend.
//!
//!   oxy query [--local] TEXT   ask the engine a question, print the rows
//!   oxy test [EXT]             run each extension's testQuery against it
//!   oxy test --cases [EXT]     run each extension's *.cases.json assertions
//!   oxy extensions             list the registry
//!
//! `--local` runs the engine in this process — how the tests exercise the
//! whole core without a daemon, and how a machine without oxyd running still
//! gets answers.

#![forbid(unsafe_code)]

use std::io::Write;
use std::time::Duration;

use oxy_core::engine::{Engine, EngineCmd, EngineEvent};
use oxy_core::{dirs, extension};
use serde_json::{Value, json};
use tokio::sync::mpsc;

const USAGE: &str =
    "oxy query [--local] TEXT | oxy test [--cases] [EXT] | oxy extensions | oxy send";

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
            // Not a summon: no refresh timers arm for an answer nobody sees.
            opened: false,
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
    // `opened:false`: a CLI ask is not a summon — without it the engine
    // stays open after this process exits and every answered provider's
    // refreshMs timer keeps firing in the daemon forever.
    writer
        .write_all(
            format!(
                "{{\"op\":\"query\",\"text\":{},\"opened\":false}}\n",
                json!(text)
            )
            .as_bytes(),
        )
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
    if args.iter().any(|a| a == "--cases") {
        let only = args.iter().find(|a| !a.starts_with("--")).cloned();
        return test_cases(only).await;
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

/// `oxy test --cases [EXT]`: the shipped `*.cases.json` fixtures run through
/// the engine — the assertions `tests/cases.py` makes against the scripts,
/// made against the rows the merged pipeline actually returns. The same
/// contract holds: cases whose extension cannot run here skip rather than
/// fail, and a row assertion only checks when the row exists.
async fn test_cases(only: Option<String>) -> i32 {
    let settings = oxy_core::settings::Settings::load(&dirs::settings_file());
    let report = extension::load_dir(&dirs::extensions_dir(), &settings.extensions);
    let mut held = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;

    // One engine for the whole run — each case is just the next query.
    let (tx, mut rx, _task) = local_engine().await;
    // Every EngineCmd::Query bumps the engine's epoch by one. Counting our
    // own sends gives the epoch each case's Results must carry — a worker
    // still finishing the previous case can land a stale Results after the
    // drain, and without this check its empty `waiting` reads as an answer.
    let mut epoch = 0u64;

    for ext in &report.extensions {
        if let Some(only) = &only
            && ext.id != *only
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
                println!("FAIL {} cases: {e}", ext.id);
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
                println!("skip {} cases (when fails here)", ext.id);
                skipped += 1;
                continue;
            }
            let cmd = ext.search.split_whitespace().next().unwrap_or("");
            if cmd.is_empty()
                || !oxy_core::provider::process::check(&format!("command -v {cmd}")).await
            {
                println!("skip {} cases ({cmd} not on PATH)", ext.id);
                skipped += 1;
                continue;
            }
        }
        if !case_preflight(&ext.id).await {
            println!(
                "skip {} cases (its data service is unreachable here)",
                ext.id
            );
            skipped += 1;
            continue;
        }

        let mut problems = 0usize;
        for case in &cases {
            let query = case.get("query").and_then(|q| q.as_str()).unwrap_or("");
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
                println!("FAIL {} case: {prob}", ext.id);
                let why = case.get("why").and_then(|w| w.as_str()).unwrap_or("");
                if !why.is_empty() {
                    println!("        ({why})");
                }
            }
        }
        if problems == 0 {
            held += 1;
            println!("ok   {} cases  {} held", ext.id, cases.len());
        } else {
            failed += problems;
        }
    }
    println!("{held} held, {failed} broke, {skipped} skipped");
    if failed > 0 { 1 } else { 0 }
}

/// The services some cases describe — unreachable here means skip, the same
/// preflight cases.py runs.
async fn case_preflight(id: &str) -> bool {
    let probe = match id {
        "define" => {
            "curl -sf --max-time 6 -o /dev/null \
             https://api.dictionaryapi.dev/api/v2/entries/en/ping"
        }
        "issue" | "pr" => "gh auth status",
        // Names like saopaulo resolve through the IANA list in tzdata, not
        // the shipped label table.
        "timezone" => "timedatectl list-timezones >/dev/null 2>&1 || test -d /usr/share/zoneinfo",
        _ => return true,
    };
    oxy_core::provider::process::check(probe).await
}

/// The row a case asserts on: the serialized wire row, plus the names the
/// fixtures were written against. The wire carries `iconGlyph` and a `key`
/// of `ext:<id>:<row-id>`; the scripts that own the cases say `glyph` and
/// `id` — so those names are mapped back here rather than the cases being
/// rewritten for the wire's spelling.
fn case_view(row: &oxy_core::row::Row, ext_id: &str) -> Value {
    let mut v = serde_json::to_value(row).unwrap_or(Value::Null);
    let Some(obj) = v.as_object_mut() else {
        return v;
    };
    let prefix = format!("ext:{ext_id}:");
    if let Some(id) = row.key.strip_prefix(&prefix) {
        obj.insert("id".into(), json!(id));
    }
    if !obj.contains_key("glyph")
        && let Some(g) = obj.get("iconGlyph").cloned()
    {
        obj.insert("glyph".into(), g);
    }
    if !obj.contains_key("icon")
        && let Some(i) = obj.get("iconSource").cloned()
    {
        obj.insert("icon".into(), i);
    }
    v
}

/// A field is absent when missing, "", [] or {}. 0 and false are real.
fn present(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(_) => true,
    }
}

/// `matches` patterns were written against Python's `str()`: a bool reads
/// `True`, a list `['a', 'b']`, a dict `{'k': 'v'}` — so the row's value is
/// rendered the same way before the regex sees it.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Null => "None".into(),
        Value::Number(n) => n.to_string(),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => py_repr_str(s),
        other => py_str(other),
    }
}

fn py_repr_str(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// One case's assertions against the rows its extension answered — a port of
/// `tests/cases.py`'s `check_case`. Row assertions only check when the row
/// exists; `minRows` is how a case demands one.
fn check_case(case: &Value, rows: &[Value]) -> Vec<String> {
    let mut out = Vec::new();
    let n = rows.len();
    if let Some(min) = case.get("minRows").and_then(|v| v.as_u64())
        && (n as u64) < min
    {
        out.push(format!("{n} rows, wanted at least {min}"));
        return out;
    }
    if let Some(max) = case.get("maxRows").and_then(|v| v.as_u64())
        && (n as u64) > max
    {
        out.push(format!("{n} rows, wanted at most {max}"));
        return out;
    }

    let idx = case.get("row").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let Some(row) = rows.get(idx) else {
        return out;
    };

    if let Some(view) = case.get("view").and_then(|v| v.as_str())
        && row.get("view").and_then(|v| v.as_str()) != Some(view)
    {
        out.push(format!(
            "row {idx} view is {:?}, wanted {view:?}",
            row.get("view")
        ));
        return out;
    }
    for f in case
        .get("fields")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
    {
        if let Some(name) = f.as_str()
            && !present(row.get(name))
        {
            out.push(format!("row {idx} {name} is empty"));
        }
    }
    for f in case
        .get("absent")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
    {
        if let Some(name) = f.as_str()
            && present(row.get(name))
        {
            out.push(format!(
                "row {idx} {name} is {:?}, wanted absent",
                row.get(name)
            ));
        }
    }
    if let Some(caps) = case.get("atMost").and_then(|v| v.as_object()) {
        for (f, cap) in caps {
            let len = match row.get(f) {
                Some(Value::String(s)) => Some(s.len()),
                Some(Value::Array(a)) => Some(a.len()),
                Some(Value::Object(o)) => Some(o.len()),
                _ => None,
            };
            if let (Some(len), Some(cap)) = (len, cap.as_u64())
                && len as u64 > cap
            {
                out.push(format!("row {idx} {f} has {len}, wanted at most {cap}"));
            }
        }
    }
    if let Some(pats) = case.get("matches").and_then(|v| v.as_object()) {
        for (f, pat) in pats {
            let Some(pat) = pat.as_str() else { continue };
            let text = py_str(row.get(f).unwrap_or(&Value::Null));
            let matched = fancy_regex::Regex::new(pat)
                .ok()
                .and_then(|re| re.is_match(&text).ok())
                .unwrap_or(false);
            if !matched {
                let short: String = text.chars().take(120).collect();
                out.push(format!("row {idx} {f} is {short:?}, wanted /{pat}/"));
            }
        }
    }
    out
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
