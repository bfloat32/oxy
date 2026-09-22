use std::time::Duration;

use oxy_core::engine::{EngineCmd, EngineEvent};
use serde_json::{Value, json};

use crate::cli::connect_daemon;
use crate::engine_local::local_engine;

/// Ask a question, collect answers until every waiter has spoken or the clock
/// runs out, print the merged list.
pub(crate) async fn run(args: &[String]) -> i32 {
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

/// The daemon route: connect, ask, print the first full answer.
async fn query_daemon(text: &str) -> std::io::Result<i32> {
    use tokio::io::{AsyncWriteExt, BufReader};

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
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::with_capacity(4096);
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    while let Ok(Ok(Some(line))) = tokio::time::timeout_at(
        deadline.into(),
        oxy_core::support::lines::next(&mut reader, &mut buf, oxy_core::support::lines::MAX_LINE),
    )
    .await
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
