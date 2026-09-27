//! The `serve` subcommand — one daemon holding one agent process and the
//! conversation around it, answering newline-delimited JSON on the local
//! socket. The wire contract is the script's:
//!
//! ```text
//! → {"epoch": 12, "query": "text"}\n
//! ← {"epoch": 12, "query": "text", "rows": [ ... ]}\n
//! → {"control": "send", "token": "…"}  /  "stop"  /  "new"  /  "ping"
//! ← {"ok": true}\n
//! ```
//!
//! One question gets an immediate answer and later pushes for the same epoch
//! as the run streams — the launcher keeps reading between questions, so a
//! refinement lands without another ask. Each client's last epoch is echoed
//! back unchanged, so an answer that arrives after the user moved on is
//! dropped by the launcher instead of repopulating the card.

#[cfg(unix)]
use tokio::sync::mpsc;

/// Everything the loop can wake up for: a client line, a client gone, a
/// chunk of the running child's stream, its pipe closing, or the periodic
/// housekeeping tick.
#[cfg(unix)]
pub enum Event {
    Line(u64, String),
    Gone(u64),
    Chunk(Vec<u8>),
    RunEof,
}

#[cfg(unix)]
use std::collections::HashMap;
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use serde_json::{Value, json};
#[cfg(unix)]
use tokio::io::{AsyncWriteExt, BufReader};

#[cfg(unix)]
use crate::agent::run::{Run, Turn};
#[cfg(unix)]
use crate::agent::{self, plan, previews, rows};

/// One connected launcher: what it last asked, and the channel its writer
/// task drains onto the socket.
#[cfg(unix)]
struct Client {
    epoch: i64,
    query: String,
    out: mpsc::UnboundedSender<String>,
}

#[cfg(unix)]
struct Daemon {
    turns: Vec<Turn>,
    run: Option<Run>,
    session: String,
    clients: HashMap<u64, Client>,
    next_client: u64,
    last_touch: f64,
    /// The last time a card asked what to draw. The socket cannot answer
    /// this: it is deliberately kept open across the launcher being closed,
    /// so a connection proves nothing and only a question does.
    last_query: f64,
    /// Coalesced pushes: an agent's stream is dozens of events a second and
    /// every push makes the launcher rebuild its rows. Four a second is
    /// already faster than anybody reads; the end of a run always goes out
    /// at once.
    dirty: bool,
    last_push: f64,
}

#[cfg(unix)]
impl Daemon {
    fn new() -> Daemon {
        Daemon {
            turns: Vec::new(),
            run: None,
            session: String::new(),
            clients: HashMap::new(),
            next_client: 0,
            last_touch: agent::now(),
            last_query: 0.0,
            dirty: false,
            last_push: 0.0,
        }
    }

    fn busy(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.turn.state == "running")
    }

    /// Take a run out of the conversation as though it had never been in
    /// it. Its session goes with it: resuming from a run nobody saw is how
    /// the next sentence inherits a context the card never showed.
    fn forget_run(&mut self) {
        self.run = None;
    }

    /// A conversation lasts as long as somebody is in it. When no card has
    /// asked for VISIT seconds the launcher is closed or somewhere else,
    /// and what is held here stops being a chat and becomes a list of old
    /// runs on the opening screen — the one thing this card should never
    /// be. Nothing is written down: `do:` opens on its examples again.
    fn end_visit(&mut self) {
        if self.busy() || (self.turns.is_empty() && self.session.is_empty() && self.run.is_none()) {
            return;
        }
        if agent::now() - self.last_query <= agent::VISIT {
            return;
        }
        self.turns.clear();
        self.run = None;
        self.session.clear();
    }

    /// The transcript the cards draw — finished turns plus the run's own.
    fn transcript(&self) -> Vec<Value> {
        let mut out: Vec<Value> = self.turns.iter().map(|t| t.view()).collect();
        if let Some(run) = &self.run {
            out.push(run.turn.view());
        }
        out
    }

    async fn rows(&self, query: &str) -> Vec<Value> {
        rows::answer(query, self.transcript(), self.busy()).await
    }

    /// Rows go out without being asked for, which is what makes this
    /// stream rather than poll. The epoch each client last sent is echoed
    /// back unchanged, so an answer that arrives after the user moved on
    /// is dropped by the launcher instead of repopulating the card.
    async fn push(&mut self) {
        self.last_push = agent::now();
        let mut dead = Vec::new();
        for (id, client) in &self.clients {
            if client.epoch < 0 {
                continue;
            }
            let payload = json!({
                "epoch": client.epoch,
                "query": client.query,
                "rows": self.rows(&client.query).await,
            });
            if client.out.send(agent::to_wire(&payload)).is_err() {
                dead.push(*id);
            }
        }
        for id in dead {
            self.clients.remove(&id);
        }
    }

    fn send_to(&self, id: u64, payload: Value) {
        if let Some(client) = self.clients.get(&id) {
            let _ = client.out.send(agent::to_wire(&payload));
        }
    }

    async fn handle(&mut self, id: u64, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            return;
        };
        if !msg.is_object() {
            return;
        }

        match msg.get("control").and_then(|c| c.as_str()) {
            Some("send") => {
                let token = msg.get("token").and_then(|t| t.as_str()).unwrap_or("");
                let ok = self.start(token).await;
                return self.send_to(id, json!({"ok": ok}));
            }
            Some("stop") => {
                if let Some(run) = &mut self.run {
                    run.kill("stopped").await;
                    self.push().await;
                }
                return self.send_to(id, json!({"ok": true}));
            }
            Some("new") => {
                if let Some(run) = &mut self.run {
                    run.kill("stopped").await;
                }
                self.turns.clear();
                self.run = None;
                self.session.clear();
                self.push().await;
                return self.send_to(id, json!({"ok": true}));
            }
            Some("ping") => return self.send_to(id, json!({"ok": true})),
            _ => {}
        }

        let Some(epoch) = msg.get("epoch").and_then(|e| e.as_i64()) else {
            return;
        };
        let query = str_field(&msg, "query");
        // Asked before the heartbeat is refreshed, so a question arriving
        // after a long silence is answered with a clean card rather than
        // with the conversation it is about to end.
        self.end_visit();
        let Some(client) = self.clients.get_mut(&id) else {
            return;
        };
        client.epoch = epoch;
        client.query.clone_from(&query);
        // A question the launcher actually asked is the heartbeat.
        self.last_query = agent::now();
        if let Some(run) = &mut self.run {
            run.seen = self.last_query;
            run.watched = true;
        }
        let rows = self.rows(&query).await;
        self.send_to(id, json!({"epoch": epoch, "query": query, "rows": rows}));
    }

    /// `send <token>`'s server half: the one place the "Enter runs what you
    /// were shown" promise is enforced, so it fails closed.
    async fn start(&mut self, token: &str) -> bool {
        let data = previews::read();
        let Some(entry) = data.get(token).cloned() else {
            return false;
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
        // The preview said which of the two this is, and it is re-read
        // rather than trusted: the plan is computed again from the
        // sentence, so a token can only ever start the steps that sentence
        // still means.
        let named = entry.get("agent").and_then(|v| v.as_str()).unwrap_or("");
        let direct =
            named == agent::direct_agent().id && plan::plan_for(&instruction).await.is_some();
        let agent = if direct {
            agent::direct_agent()
        } else {
            match agent::pick_agent(named) {
                Some(a) => a,
                None => return false,
            }
        };
        if agent.is_missing() {
            return false;
        }
        if let Some(mut old) = self.run.take() {
            // Sending while one is going replaces it. The alternative is a
            // queue nobody can see, and a second Enter that appears to do
            // nothing.
            old.kill("replaced").await;
            if !old.session.is_empty() {
                self.session.clone_from(&old.session);
            }
            self.turns.push(old.turn);
        }
        previews::mark_sent(token);
        let turn = Turn::new(agent.clone(), instruction, cwd, token.to_string());
        while self.turns.len() + 1 > agent::MAX_TURNS {
            self.turns.remove(0);
        }
        let resume = if agent.id == "claude" {
            self.session.clone()
        } else {
            String::new()
        };
        let mut run = Run::new(turn, resume);
        // Enter comes from a card that was asking a moment ago; a shell has
        // not asked at all. That difference is the whole of whether this
        // run is something the user is doing or something a script is doing.
        run.watched = agent::now() - self.last_query <= agent::GRACE;
        if let Err(e) = run.start(self_sink()).await {
            run.turn.state = "failed".into();
            run.turn.note = e;
            run.turn.finished = agent::now();
        }
        self.run = Some(run);
        self.push().await;
        true
    }
}

/// `str(msg.get("query") or "")` — the falsy values collapse to "", the
/// rest read the way Python's `str()` would spell them.
#[cfg(unix)]
fn str_field(msg: &Value, key: &str) -> String {
    match msg.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) if n.as_f64() != Some(0.0) => n.to_string(),
        Some(Value::Bool(true)) => "True".into(),
        _ => String::new(),
    }
}

/// The daemon task's inbox, filled by client readers and the run's reader
/// thread; `Run::start` wants it by value for the thread it spawns.
#[cfg(unix)]
type Sink = mpsc::UnboundedSender<Event>;

// Set by `serve` before the loop starts — the reader-thread sink `start`
// hands out. One daemon per process, so one static is honest.
#[cfg(unix)]
static SINK: std::sync::OnceLock<Sink> = std::sync::OnceLock::new();

#[cfg(unix)]
fn self_sink() -> Sink {
    SINK.get().expect("serve sets the sink").clone()
}

/// `oxy-agent serve`. Unix sockets are the whole socket story: on Windows
/// there is no filesystem socket to bind, so this says so and leaves.
#[cfg(not(unix))]
pub async fn serve() -> i32 {
    eprintln!("oxy-agent serve needs a unix socket; this build cannot listen");
    1
}

#[cfg(unix)]
pub async fn serve() -> i32 {
    use interprocess::local_socket::{
        GenericFilePath, ListenerOptions, ToFsName, tokio::prelude::*,
    };

    let _ = std::fs::create_dir_all(agent::state_dir());
    let path = agent::socket_path();
    // Held for the life of the process: two serves racing would otherwise
    // both pass a liveness probe, and the second's unlink would orphan the
    // first's live socket. A dead daemon's lock is free the moment it dies.
    let _instance_lock = {
        let lock_path = agent::state_dir().join("oxy-agent.lock");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path);
        match file.map(|f| f.try_lock().map(|()| f)) {
            Ok(Ok(f)) => f,
            Ok(Err(std::fs::TryLockError::WouldBlock)) => {
                eprintln!("oxy-agent serve: already running on {}", path.display());
                return 0;
            }
            _ => {
                eprintln!("oxy-agent serve: cannot lock {}", lock_path.display());
                return 1;
            }
        }
    };
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            eprintln!("oxy-agent serve: cannot unlink {}: {e}", path.display());
            return 1;
        }
    }
    let path_text = path.to_string_lossy().into_owned();
    let name = match path_text.to_fs_name::<GenericFilePath>() {
        Ok(n) => n,
        Err(e) => {
            eprintln!("oxy-agent serve: bad socket name: {e}");
            return 1;
        }
    };
    let listener = match ListenerOptions::new().name(name).create_tokio() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("oxy-agent serve: cannot listen: {e}");
            return 1;
        }
    };
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    let (tx, mut inbox) = mpsc::unbounded_channel::<Event>();
    let _ = SINK.set(tx.clone());

    let mut d = Daemon::new();
    let mut tick = tokio::time::interval(Duration::from_millis(400));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // SIGTERM runs the cleanup below rather than skipping it: a daemon
    // killed without unlinking its socket leaves a path that accepts
    // nothing, and the launcher then waits out a connect on every
    // keystroke.
    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();

    let rc = 'outer: loop {
        tokio::select! {
            conn = listener.accept() => {
                if let Ok(stream) = conn {
                    d.next_client += 1;
                    let id = d.next_client;
                    accept_client(id, stream, &tx, &mut d.clients);
                    d.last_touch = agent::now();
                }
            }
            msg = inbox.recv() => {
                match msg {
                    Some(Event::Line(id, line)) => {
                        d.last_touch = agent::now();
                        d.handle(id, &line).await;
                    }
                    Some(Event::Gone(id)) => {
                        d.clients.remove(&id);
                    }
                    Some(Event::Chunk(chunk)) => {
                        if let Some(run) = &mut d.run {
                            run.absorb_chunk(&chunk);
                            if run.check_finished() {
                                d.dirty = true;
                            }
                            d.dirty = true;
                        }
                    }
                    Some(Event::RunEof) => {
                        if let Some(run) = &mut d.run
                            && run.check_finished()
                        {
                            d.dirty = true;
                        }
                    }
                    None => break 'outer 0,
                }
            }
            _ = tick.tick() => {}
            _ = async {
                match sigterm.as_mut() {
                    Some(s) => s.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                break 'outer 0;
            }
        }

        housekeeping(&mut d).await;

        if !d.clients.is_empty() || d.busy() || agent::now() - d.last_touch <= agent::IDLE_EXIT {
            continue;
        }
        break 'outer 0;
    };

    // The cleanup the script's `finally` ran: the run goes first, then the
    // socket path, so nothing connects to a listener that is not there.
    if let Some(run) = &mut d.run {
        run.kill("daemon exiting").await;
    }
    let _ = std::fs::remove_file(agent::socket_path());
    rc
}

/// The watchdogs and the push cadence — the part of the script's loop body
/// that ran after every `select`.
#[cfg(unix)]
async fn housekeeping(d: &mut Daemon) {
    if d.busy() {
        if let Some(run) = &mut d.run
            && run.check_finished()
        {
            d.dirty = true;
        }
        // Nobody is watching. Escape has its own command and does not wait
        // for this; this is the launcher being closed or killed while a run
        // is going, which must not leave an agent running with nowhere to
        // show what it is doing.
        if let Some(run) = &mut d.run
            && agent::now() - run.seen > agent::GRACE
        {
            if run.watched {
                // A card was here and is gone.
                run.kill("stopped when the launcher closed").await;
            } else {
                // No card ever asked about this one: a `send` typed at a
                // shell, or the extension's own cases. It ran, it is dead,
                // and it never belonged to a conversation.
                run.kill("").await;
                d.forget_run();
            }
            d.dirty = true;
        }
    } else if let Some(run) = &mut d.run {
        if !run.watched {
            // It finished before the watchdog got to it. Same rule: a run
            // nothing ever asked about leaves nothing behind.
            d.forget_run();
        } else if !run.session.is_empty() {
            d.session.clone_from(&run.session);
        }
    }

    d.end_visit();

    if d.dirty && (agent::now() - d.last_push >= 0.25 || !d.busy()) {
        d.dirty = false;
        d.push().await;
    }
}

/// Register one accepted connection: a reader task feeding the inbox and a
/// writer task draining the client's out channel onto the socket.
#[cfg(unix)]
fn accept_client(
    id: u64,
    stream: interprocess::local_socket::tokio::Stream,
    tx: &Sink,
    clients: &mut HashMap<u64, Client>,
) {
    let (reader, mut writer) = tokio::io::split(stream);
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    clients.insert(
        id,
        Client {
            epoch: -1,
            query: String::new(),
            out: out_tx,
        },
    );

    let line_tx = tx.clone();
    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::with_capacity(4096);
        loop {
            match oxy_core::support::lines::next(&mut reader, &mut buf, agent::MAX_LINE).await {
                Ok(Some(line)) => {
                    if line_tx.send(Event::Line(id, line)).is_err() {
                        return;
                    }
                }
                _ => {
                    let _ = line_tx.send(Event::Gone(id));
                    return;
                }
            }
        }
    });

    tokio::spawn(async move {
        while let Some(line) = out_rx.recv().await {
            if writer
                .write_all(format!("{line}\n").as_bytes())
                .await
                .is_err()
                || writer.flush().await.is_err()
            {
                return;
            }
        }
    });
}
