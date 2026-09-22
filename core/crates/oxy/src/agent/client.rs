//! The client half: `search` answers locally or asks the daemon when one is
//! up, `send`/`stop`/`new` are one-line control messages to it, `term`
//! hands the instruction to an interactive agent in a terminal, and
//! `ensure_daemon` is how `send` gets a daemon to talk to.

use serde_json::{Value, json};

use crate::agent::{self, previews, rows};

/// Whether a daemon is answering the socket. The path existing is not
/// enough — a stale socket file connects to nothing — so this actually
/// opens one, the way the script did.
#[cfg(unix)]
async fn socket_alive() -> bool {
    use interprocess::local_socket::{GenericFilePath, ToFsName, tokio::prelude::*};

    if !agent::socket_path().exists() {
        return false;
    }
    let Ok(name) = agent::socket_path()
        .to_string_lossy()
        .into_owned()
        .to_fs_name::<GenericFilePath>()
    else {
        return false;
    };
    tokio::time::timeout(
        std::time::Duration::from_millis(400),
        interprocess::local_socket::tokio::Stream::connect(name),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

#[cfg(not(unix))]
async fn socket_alive() -> bool {
    false
}

/// One request, one line back. Used by `search` when the daemon happens to
/// be up, and by `send`/`stop`/`new`, which need it to be.
#[cfg(unix)]
async fn talk(payload: Value, timeout: std::time::Duration) -> Option<Value> {
    use interprocess::local_socket::{GenericFilePath, ToFsName, tokio::prelude::*};
    use tokio::io::{AsyncWriteExt, BufReader};

    let inner = async {
        let name = agent::socket_path()
            .to_string_lossy()
            .into_owned()
            .to_fs_name::<GenericFilePath>()
            .ok()?;
        let stream = interprocess::local_socket::tokio::Stream::connect(name)
            .await
            .ok()?;
        let (reader, mut writer) = tokio::io::split(stream);
        let line = agent::to_wire(&payload) + "\n";
        writer.write_all(line.as_bytes()).await.ok()?;
        writer.flush().await.ok()?;
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::with_capacity(4096);
        let line = oxy_core::support::lines::next(&mut reader, &mut buf, agent::MAX_LINE)
            .await
            .ok()??;
        serde_json::from_str::<Value>(&line).ok()
    };
    tokio::time::timeout(timeout, inner).await.ok().flatten()
}

#[cfg(not(unix))]
async fn talk(_payload: Value, _timeout: std::time::Duration) -> Option<Value> {
    None
}

/// `send` needs the daemon; a socket that is not answering gets one spawned
/// and four seconds to come up.
async fn ensure_daemon() -> bool {
    if socket_alive().await {
        return true;
    }
    #[cfg(unix)]
    {
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("serve")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // Its own session: the daemon outlives the `send` that spawned it.
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        if cmd.spawn().is_err() {
            return false;
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        while std::time::Instant::now() < deadline {
            if socket_alive().await {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        }
    }
    false
}

/// The launcher's cheap path. It never starts an agent and never starts
/// the daemon: the whole cost of typing here is one row computed locally,
/// or one round trip when there is already a conversation to show.
pub async fn cmd_search(query: &str) -> i32 {
    if socket_alive().await
        && let Some(reply) = talk(json!({"epoch": 0, "query": query}), sec(2)).await
        && let Some(rows) = reply.get("rows").and_then(|r| r.as_array())
    {
        println!("{}", agent::to_wire(&json!(rows)));
        return 0;
    }
    let rows = rows::answer(query, Vec::new(), false).await;
    println!("{}", agent::to_wire(&json!(rows)));
    0
}

fn sec(n: u64) -> std::time::Duration {
    std::time::Duration::from_secs(n)
}

pub async fn cmd_send(token: &str) -> i32 {
    let token = token.trim();
    // `[0-9a-f]{16}` — the token shape `search` wrote and nothing else.
    let valid = token.len() == 16
        && token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !valid {
        return 2;
    }
    if previews::read().get(token).is_none() {
        return 2;
    }
    if !ensure_daemon().await {
        return 1;
    }
    let reply = talk(json!({"control": "send", "token": token}), sec(6)).await;
    if reply
        .and_then(|r| r.get("ok").and_then(|o| o.as_bool()))
        .unwrap_or(false)
    {
        0
    } else {
        1
    }
}

pub async fn cmd_stop() -> i32 {
    if socket_alive().await {
        let _ = talk(json!({"control": "stop"}), sec(2)).await;
    }
    0
}

pub async fn cmd_new() -> i32 {
    if socket_alive().await {
        let _ = talk(json!({"control": "new"}), sec(2)).await;
    }
    0
}

/// The way out. Anything refused here is a thing somebody should be
/// looking at while it happens, so it goes to a terminal where the agent's
/// own permission prompts exist and a human answers them.
pub async fn cmd_term(token: &str) -> i32 {
    let data = previews::read();
    let Some(entry) = data.get(token) else {
        return 2;
    };
    let instruction = entry
        .get("instruction")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let cwd = entry
        .get("cwd")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // A sentence that was going to be run here goes to whichever agent is
    // installed, because "in a terminal instead" is a request for the
    // thinking version of it and there is nothing to think with otherwise.
    let named = entry.get("agent").and_then(|v| v.as_str()).unwrap_or("");
    let agent = if named == agent::direct_agent().id {
        agent::pick_agent("")
    } else {
        agent::pick_agent(named)
    };
    let Some(agent) = agent.filter(|a| !a.is_missing()) else {
        return 2;
    };
    let inner: Vec<String> = match agent.id.as_str() {
        "claude" => vec!["claude".into(), instruction.clone()],
        "codex" => vec!["codex".into(), instruction.clone()],
        "gemini" => vec!["gemini".into(), "-i".into(), instruction.clone()],
        _ => vec![agent.bin.clone(), instruction.clone()],
    };
    // `xdg-terminal-exec` first, and not `$TERMINAL`: on Omarchy that
    // variable holds `xdg-terminal-exec` itself, which takes `--dir=` and a
    // bare command rather than `--working-directory ... -e ...`.
    let cmd: Vec<String> = if agent::on_path("xdg-terminal-exec") {
        let mut c = vec![
            "xdg-terminal-exec".into(),
            format!("--dir={cwd}"),
            "--hold".into(),
            "--".into(),
        ];
        c.extend(inner);
        c
    } else {
        let term = ["alacritty", "ghostty", "foot", "kitty"]
            .iter()
            .find(|t| agent::on_path(t))
            .map(|t| t.to_string())
            .unwrap_or_else(|| "xterm".into());
        let mut c = vec![term, "--working-directory".into(), cwd, "-e".into()];
        c.extend(inner);
        c
    };
    let mut spawn = std::process::Command::new(&cmd[0]);
    spawn
        .args(&cmd[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        spawn.process_group(0);
    }
    match spawn.spawn() {
        Ok(_) => 0,
        Err(_) => 1,
    }
}

/// `agents`: which agent CLIs are installed, the script's `%-8s %s` line.
pub async fn cmd_agents() -> i32 {
    for a in agent::agents() {
        let where_is = agent::which(&a.bin)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "not installed".into());
        println!("{:<8} {}", a.id, where_is);
    }
    0
}
