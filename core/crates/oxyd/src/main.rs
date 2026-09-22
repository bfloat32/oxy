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

mod clipboard;
mod logfile;
mod server;
mod watch;
mod wire;

use std::sync::Arc;

use oxy_core::engine::{Engine, EngineCmd, EngineEvent};
use oxy_core::provider::native;
use oxy_core::settings::paths as dirs;
use serde_json::json;
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
    // the state in two and orphan whichever daemon bound first. The lock is
    // held for the life of the process, which is what makes probe + unlink +
    // bind atomic: without it two concurrent spawns could both pass the
    // probe, and the second's unlink would orphan the first's live socket.
    let _instance_lock = {
        let lock_path = dirs::state_home().join("oxyd.lock");
        let _ = std::fs::create_dir_all(dirs::state_home());
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path);
        match file.map(|f| f.try_lock().map(|()| f)) {
            Ok(Ok(f)) => f,
            Ok(Err(std::fs::TryLockError::WouldBlock)) => {
                eprintln!("oxyd: already running on {}", dirs::socket_name());
                return;
            }
            _ => {
                eprintln!("oxyd: cannot lock {}", lock_path.display());
                return;
            }
        }
    };
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
    {
        use std::os::unix::fs::PermissionsExt;
        let sock_path = dirs::socket_name();
        if let Some(parent) = std::path::Path::new(&sock_path).parent() {
            let _ = std::fs::create_dir_all(parent);
            // The state-dir fallback is ours — owner-only traversal. The
            // XDG_RUNTIME_DIR case is already 0700 by spec and is never
            // ours to chmod, so it is deliberately skipped.
            if parent.starts_with(dirs::state_home()) {
                let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
            }
        }
        let _ = std::fs::remove_file(&sock_path);
    }
    let listener = match ListenerOptions::new().name(name).create_tokio() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("oxyd: listen: {e} (is another oxyd running?)");
            std::process::exit(1);
        }
    };
    // The socket file is the access boundary — 0600 means only this user can
    // drive the launcher, whichever directory the address fell back into.
    // Windows has no socket file: the named pipe takes the process's default
    // DACL — the creating user, SYSTEM and Administrators — the same user-only
    // boundary on a single-user box, but not a session one: anything running
    // as this user, in any session, can drive the launcher (including `act`).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ =
            std::fs::set_permissions(dirs::socket_name(), std::fs::Permissions::from_mode(0o600));
    }

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
    let sid = logfile::new_sid();
    tokio::spawn(async move {
        while let Some(event) = evt_rx.recv().await {
            if let EngineEvent::Log { ev, fields } = &event {
                logfile::append_log(&log_path, &logfile::log_line(&sid, ev, fields));
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

    let hello_src = engine.shared();

    watch::spawn(
        cmd_tx.clone(),
        extensions_dir.clone(),
        dirs::settings_file(),
    );

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
    server::serve(listener, engine_cmd, bcast, hello_src).await;
}
