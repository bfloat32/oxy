//! `oxyd` — the daemon half of the Rust core. One local socket, one engine,
//! as many frontends as want to listen.
//!
//! The wire is JSON lines, one command in or one event out per line:
//!
//! ```text
//! → {"op":"query","text":"fire","opened":true}
//! ← {"op":"results","epoch":7,"rows":[…],"waiting":[],"stale":[],…}
//! ```
//!
//! The protocol is documented in `docs/PROTOCOL.md`; the short of it is that
//! the frontend is a teletype: it sends what the box says and draws what it
//! is sent, and every ordering decision lives in the engine.

#![forbid(unsafe_code)]

use std::path::Path;
use std::sync::Arc;

use oxy_core::dirs;
use oxy_core::engine::{Engine, EngineCmd, EngineEvent};
use oxy_core::native;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc};

#[cfg(unix)]
use interprocess::local_socket::{GenericFilePath, ListenerOptions, ToFsName, tokio::prelude::*};
#[cfg(windows)]
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName, tokio::prelude::*};

fn socket_name() -> std::io::Result<interprocess::local_socket::Name<'static>> {
    #[cfg(unix)]
    {
        dirs::socket_name().to_fs_name::<GenericFilePath>()
    }
    #[cfg(windows)]
    {
        dirs::socket_name().to_ns_name::<GenericNamespaced>()
    }
}

/// A URL you just copied, offered as the first row of an empty box. Only an
/// explicit scheme counts: a bare `github.com/x` is a plausible thing to have
/// copied for any other reason.
#[cfg(unix)]
fn url_in_clipboard(text: &str) -> Option<String> {
    let value = text.trim();
    if value.is_empty() || value.len() >= 480 || value.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    let lower = value.to_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return None;
    }
    let rest = &value[value.find("://").unwrap() + 3..];
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() || !host.contains('.') {
        return None;
    }
    Some(value.to_string())
}

/// Read the clipboard once per open. `head -c` bounds the read: a clipboard
/// holding 40MB of screenshot costs one closed pipe rather than a string the
/// daemon then has to hold.
#[cfg(unix)]
async fn read_clipboard() -> Option<String> {
    let out = oxy_core::provider::process::run(
        "wl-paste -n -t text/plain 2>/dev/null | head -c 512",
        std::time::Duration::from_secs(2),
    )
    .await?;
    url_in_clipboard(&out.stdout)
}

#[cfg(not(unix))]
async fn read_clipboard() -> Option<String> {
    None
}

/// One daemon boot's id — `Date.now().toString(36)` the way Logger.qml
/// minted it, so one log file can tell two sessions' lines apart.
fn new_sid() -> String {
    let mut n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if n == 0 {
        return "0".into();
    }
    let mut buf = Vec::new();
    while n > 0 {
        buf.push(char::from_digit((n % 36) as u32, 36).unwrap_or('0'));
        n /= 36;
    }
    buf.iter().rev().collect()
}

/// One line of the event log: `{ts, sid, ev, ...fields}` — the shape
/// Logger.qml wrote, appended by whichever daemon holds the file.
fn log_line(sid: &str, ev: &str, fields: &Value) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut obj = match fields {
        Value::Object(m) => m.clone(),
        _ => Default::default(),
    };
    obj.insert("ts".into(), json!(ts));
    obj.insert("sid".into(), json!(sid));
    obj.insert("ev".into(), json!(ev));
    Value::Object(obj).to_string() + "\n"
}

fn append_log(path: &Path, line: &str) {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Logger.qml rotated at 1MB into a single `.old`: the file can hold at
    // most ~2MB of history, and a tail-of-tails is still on disk for forensics.
    if std::fs::metadata(path)
        .map(|m| m.len() > 1_048_576)
        .unwrap_or(false)
    {
        let mut old = path.as_os_str().to_os_string();
        old.push(".old");
        let _ = std::fs::rename(path, old);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Parse a command line into an `EngineCmd`. Unknown ops are ignored rather
/// than fatal: a newer frontend talking to an older daemon degrades, not
/// dies. Takes the already-parsed `Value` — the caller looked at `op` first,
/// and a query line is parsed exactly once per keystroke.
fn parse_cmd(v: Value) -> Option<EngineCmd> {
    let op = v.get("op")?.as_str()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("");
    Some(match op {
        "query" => EngineCmd::Query {
            text: s("text").to_string(),
            opened: v.get("opened").and_then(|x| x.as_bool()).unwrap_or(true),
        },
        "open" => EngineCmd::Open {
            text: s("text").to_string(),
        },
        "clipboard" => EngineCmd::Clipboard {
            url: v.get("url").and_then(|x| x.as_str()).map(String::from),
        },
        "close" => EngineCmd::Close,
        "activate" => EngineCmd::Activate {
            key: s("key").to_string(),
            action: v.get("action").and_then(|x| x.as_u64()).map(|n| n as usize),
            shift: v.get("shift").and_then(|x| x.as_bool()).unwrap_or(false),
            ctrl: v.get("ctrl").and_then(|x| x.as_bool()).unwrap_or(false),
        },
        "act" => EngineCmd::Act {
            key: s("key").to_string(),
            // A synthesized action (a form's exec with {field} filled in):
            // not one of the row's declared actions, so it arrives whole.
            action: Box::new(
                serde_json::from_value(v.get("action").cloned().unwrap_or(Value::Null)).ok()?,
            ),
        },
        "pin" => EngineCmd::Pin {
            key: s("key").to_string(),
        },
        "select" => EngineCmd::Select {
            key: s("key").to_string(),
        },
        "commitpreview" => EngineCmd::CommitPreview,
        "set" => EngineCmd::Set {
            key: s("key").to_string(),
            value: v.get("value").and_then(|x| x.as_f64()).unwrap_or(0.0),
        },
        "savesettings" => EngineCmd::SaveSettings {
            id: s("id").to_string(),
            values: v
                .get("values")
                .and_then(|x| x.as_object().cloned())
                .unwrap_or_default(),
        },
        "ask" => EngineCmd::Ask {
            text: s("text").to_string(),
        },
        "stopask" => EngineCmd::StopAsk,
        "reload" => EngineCmd::Reload,
        "log" => EngineCmd::Log {
            ev: s("ev").to_string(),
            fields: v.get("fields").cloned().unwrap_or(Value::Null),
        },
        "ping" => EngineCmd::Ping,
        _ => return None,
    })
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let name = match socket_name() {
        Ok(n) => n,
        Err(e) => {
            eprintln!("oxyd: socket name: {e}");
            std::process::exit(1);
        }
    };
    // One daemon owns the address. A file left by a dead one is replaced; a
    // socket a live daemon is answering on is not — two frontends spawning at
    // once, or a hand-run oxyd beside a running one, would otherwise split
    // the state in two and orphan whichever daemon bound first.
    {
        use interprocess::local_socket::tokio::Stream;
        let live = tokio::time::timeout(
            std::time::Duration::from_millis(800),
            Stream::connect(name.clone()),
        )
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false);
        if live {
            eprintln!("oxyd: already running on {}", dirs::socket_name());
            return;
        }
    }
    #[cfg(unix)]
    let _ = std::fs::remove_file(dirs::socket_name());
    let listener = match ListenerOptions::new().name(name).create_tokio() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("oxyd: listen: {e} (is another oxyd running?)");
            std::process::exit(1);
        }
    };

    let (cmd_tx, cmd_rx) = mpsc::channel::<EngineCmd>(256);
    let (evt_tx, mut evt_rx) = mpsc::channel::<EngineEvent>(512);
    let (worker_tx, worker_rx) = mpsc::channel(512);
    // `Arc<[u8]>`: broadcast clones the payload per subscriber — a refcount
    // bump instead of a fresh `String` for every client on every event.
    let (bcast, _) = broadcast::channel::<Arc<[u8]>>(256);

    // Fan events out: every client hears every event, and `log` lines land in
    // the file the same way Logger.qml wrote them.
    let bcast_tx = bcast.clone();
    let log_path = dirs::log_file();
    let sid = new_sid();
    tokio::spawn(async move {
        while let Some(event) = evt_rx.recv().await {
            if let EngineEvent::Log { ev, fields } = &event {
                append_log(&log_path, &log_line(&sid, ev, fields));
            }
            // `to_vec` writes straight into the buffer; `into_boxed_slice` +
            // `Arc::from` share that allocation — no second copy per event.
            if let Ok(mut buf) = serde_json::to_vec(&event) {
                buf.push(b'\n');
                let _ = bcast_tx.send(Arc::from(buf.into_boxed_slice()));
            }
        }
    });

    let extensions_dir = dirs::extensions_dir();
    let engine = Engine::start(&extensions_dir, cmd_rx, evt_tx, worker_tx, |name| {
        native::construct(name)
    })
    .await;

    // The engine's keywords, for the hello it owes each new client.
    let hello = {
        let known = engine.keywords();
        json!({
            "op": "hello",
            "version": env!("CARGO_PKG_VERSION"),
            "keywords": known,
        })
        .to_string()
            + "\n"
    };
    let hello = Arc::new(hello);

    // Watch the extensions dir and oxy.json: a file landing or changing
    // reloads the registry and settings, the way the FileView +
    // Settings.qml watchers did. `settings.json` writes (form saves) change
    // the signature too — one extra reload is harmless.
    let reload_tx = cmd_tx.clone();
    let watch_dir = extensions_dir.clone();
    let settings_path = dirs::settings_file();
    tokio::spawn(async move {
        let mut last_ext = signature(&watch_dir);
        let mut last_cfg = file_signature(&settings_path);
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            let sig = signature(&watch_dir);
            if sig != last_ext {
                last_ext = sig;
                let _ = reload_tx.send(EngineCmd::Reload).await;
                continue;
            }
            let csig = file_signature(&settings_path);
            if csig != last_cfg {
                last_cfg = csig;
                let _ = reload_tx.send(EngineCmd::Reload).await;
            }
        }
    });

    // The login environment, captured once and replayed onto every command
    // the daemon ever spawns — profile PATH/mise/nix survive the session env
    // the frontend handed us. Runs beside the engine boot; commands spawned
    // in the first few ms fall back to the inherited env.
    let env_tx = cmd_tx.clone();
    tokio::spawn(async move {
        let t0 = std::time::Instant::now();
        let vars = oxy_core::provider::process::capture_login_env().await;
        let _ = env_tx
            .send(EngineCmd::Log {
                ev: "env".into(),
                fields: json!({"vars": vars, "ms": t0.elapsed().as_millis() as u64}),
            })
            .await;
    });

    // The engine runs on its own task so accept() never waits on it.
    let engine_cmd = cmd_tx.clone();
    tokio::spawn(async move { engine.run(worker_rx).await });

    eprintln!("oxyd: listening on {}", dirs::socket_name());
    loop {
        let Ok(stream) = listener.accept().await else {
            continue;
        };
        let cmd_tx = engine_cmd.clone();
        let events = bcast.subscribe();
        let hello = hello.clone();
        tokio::spawn(async move {
            let (reader, mut writer) = stream.split();
            // The hello first: a client that knows the keyword set parses the
            // first query correctly even before the registry event lands.
            if writer.write_all(hello.as_bytes()).await.is_err() {
                return;
            }
            let _ = writer.flush().await;
            serve_with(reader, &mut writer, cmd_tx, events).await;
        });
    }
}

/// `serve`, with the split already done so `hello` could be written first.
async fn serve_with<R, W>(
    reader: R,
    writer: &mut W,
    cmd_tx: mpsc::Sender<EngineCmd>,
    mut events: broadcast::Receiver<Arc<[u8]>>,
) where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { return };
                let line = line.trim();
                // One parse per line — `op` is read off the same `Value` the
                // command is built from.
                let Ok(v) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                // Once per open, never per keystroke: a URL on the clipboard
                // is the first row of an empty box.
                let is_open =
                    v.get("op").and_then(|x| x.as_str()) == Some("open");
                if let Some(cmd) = parse_cmd(v) {
                    if is_open {
                        let tx = cmd_tx.clone();
                        tokio::spawn(async move {
                            if let Some(url) = read_clipboard().await {
                                let _ = tx.send(EngineCmd::Clipboard { url: Some(url) }).await;
                            }
                        });
                    }
                    if cmd_tx.send(cmd).await.is_err() {
                        return;
                    }
                }
            }
            event = events.recv() => {
                match event {
                    // A slow client skips what it missed; a closed channel
                    // means the engine is gone and there is nothing to serve.
                    Ok(event) => {
                        if writer.write_all(&event).await.is_err() {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    }
}

/// A single file's change signature — len + mtime, so an edit or a delete
/// (None metadata → 0) both register. Same fixed-key hasher as `signature`.
fn file_signature(path: &Path) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match std::fs::metadata(path) {
        Ok(meta) => {
            h.write_u64(meta.len());
            h.write_u64(
                meta.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            );
        }
        Err(_) => h.write_u64(u64::MAX),
    }
    h.finish()
}

/// What a reload watches: every extension file's name, size and mtime. A
/// summon whose signature matches pays one stat and keeps every worker it
/// already has. The hash is XOR-accumulated so readdir order doesn't matter,
/// and `DefaultHasher::new` uses fixed keys, so two polls are comparable.
fn signature(dir: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut acc = 0u64;
    if let Ok(read) = std::fs::read_dir(dir) {
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let mut h = std::collections::hash_map::DefaultHasher::new();
            if let Some(name) = path.file_name() {
                name.as_encoded_bytes().hash(&mut h);
            }
            h.write_u64(meta.len());
            h.write_u64(
                meta.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            );
            acc ^= h.finish();
        }
    }
    acc
}
