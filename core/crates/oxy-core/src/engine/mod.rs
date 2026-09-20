//! The engine: one task that owns the query pipeline — the port of
//! `Launcher.qml`'s orchestration half.
//!
//! Text in, merged rows out. Workers answer for their extensions; the engine
//! answers inline for the four that are the launcher itself (help, recents,
//! paste, settings, actions), merges every bucket through the ranker, applies
//! frecency and pins, and decides what Enter does.
//!
//! Everything arrives on one channel and everything leaves on another, so the
//! daemon's IPC loop is plumbing rather than logic.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use serde_json::{Map, Value, json};
use tokio::sync::{RwLock, mpsc};

use crate::model::query::Query;
use crate::model::row::{Action, Row};
use crate::provider::NativeExt;
use crate::provider::worker::{self, Shared, WorkerCmd, WorkerMsg};
use crate::registry::{Extension, known_keywords};
use crate::settings::Settings;
use crate::state::{self, State};
use crate::support::rank;

/// What the client sends the engine.
pub enum EngineCmd {
    /// The launcher was summoned: the box's text, plus everything a summon
    /// re-sends — the registry for chips and the ask hint.
    Open { text: String },
    /// The text in the box, and whether the launcher is open.
    Query { text: String, opened: bool },
    /// What the clipboard held when the launcher opened (`wl-paste` output).
    Clipboard { url: Option<String> },
    /// The launcher closed: stop refreshes, keep caches.
    Close,
    /// Enter on a row. `action` picks a non-primary action; `shift`/`ctrl`
    /// mirror the frontend's modifier meanings.
    Activate {
        key: String,
        action: Option<usize>,
        shift: bool,
        ctrl: bool,
    },
    /// Ctrl+P on a row.
    Pin { key: String },
    /// The selection landed on a row: run its `previewExec` if it has one.
    Select { key: String },
    /// Leaving the launcher keeps whatever the preview changed.
    CommitPreview,
    /// A slider's `setExec` with the value substituted.
    Set { key: String, value: f64 },
    /// A form row's submission: extension id and its field values.
    SaveSettings {
        id: String,
        values: Map<String, Value>,
    },
    /// Reload extensions and settings from disk.
    Reload,
    /// Register a native provider for the extension id — the daemon injects
    /// what it compiled in.
    RegisterNative {
        ext_id: String,
        native: Box<dyn NativeExt>,
    },
    /// Ctrl+Enter: stream an `askProviders` answer for this question.
    Ask { text: String },
    /// Escape out of the answer panel: kill the stream.
    StopAsk,
    /// An action the row did not declare — a form's `exec` with its `{field}`
    /// tokens substituted, built by the frontend because only it held the
    /// answers. Boxed: it is the fat variant, and this enum crosses a channel
    /// on every client command.
    Act { key: String, action: Box<Action> },
    /// The event log asks for a line of its own (the frontend's own events).
    Log { ev: String, fields: Value },
    /// Liveness.
    Ping,
}

/// What the engine pushes back.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum EngineEvent {
    /// First contact: version, and the keywords the parser validates against.
    Hello {
        version: String,
        keywords: Vec<String>,
    },
    /// The merged list for the current epoch.
    Results {
        epoch: u64,
        /// Shared rows — a publish is refcount bumps, not a deep copy of
        /// every row the merged list carries.
        rows: Vec<Arc<Row>>,
        /// Provider ids still owed an answer.
        waiting: Vec<Arc<str>>,
        /// Provider ids whose visible rows are one or more keystrokes old.
        stale: Vec<Arc<str>>,
        scope: String,
        #[serde(rename = "scopeLabel")]
        scope_label: String,
        #[serde(rename = "helpMode")]
        help_mode: bool,
        #[serde(rename = "recentMode")]
        recent_mode: bool,
        /// The layout the first row asked for.
        view: String,
        /// The armed `confirm` prompt, while one is armed — the empty state
        /// draws it instead of "nothing matches".
        #[serde(default)]
        confirm: String,
    },
    /// Type this into the box (`fill`, follow-up `query`).
    Type {
        text: String,
        /// A step deeper into a flow: the shell pushes where it was onto its
        /// back-trail first. A `fill` is not a step, so it stays false.
        #[serde(default)]
        flow: bool,
        /// What the action set in motion lands later (a player starting, a
        /// daemon answering): the shell re-asks a few times so the list
        /// catches up, the way `followUpTimer` did.
        #[serde(default)]
        poll: bool,
    },
    /// A footer line that is not an answer ("Recent queries cleared").
    Notice {
        text: String,
    },
    /// The extension registry, for help/settings views and source chips —
    /// plus whether Ctrl+Enter has an `askProviders` command behind it.
    Registry {
        extensions: Vec<Value>,
        /// `{"available": bool, "model": title}` — the first provider whose
        /// `when` answered, probed once rather than per question.
        ask: Value,
    },
    /// A log line the daemon appends to the event file.
    Log {
        ev: String,
        fields: Value,
    },
    /// An `ask` run started: the question and which provider answered.
    AnswerStart {
        question: String,
        provider: String,
    },
    /// One line of a streaming `ask` answer.
    Answer {
        line: String,
    },
    /// The stream ended; `error` is why, when it failed before answering.
    AnswerDone {
        error: String,
    },
    Pong,
    /// The daemon should close the launcher after this.
    Close,
}

/// Provider ids whose rows are never frecency-recorded: their keys change
/// per keystroke, so recording them teaches nothing.
const NO_FRECENCY: &[&str] = &["calc", "web"];

/// The constructor the daemon injects: extension `native` name → provider.
pub type NativeCtor = Box<dyn Fn(&str) -> Option<Box<dyn NativeExt>> + Send + Sync>;

pub struct Engine {
    cmd_rx: mpsc::Receiver<EngineCmd>,
    evt_tx: mpsc::Sender<EngineEvent>,

    shared: Arc<Shared>,
    settings: Settings,
    extensions: Arc<Vec<Extension>>,
    workers: HashMap<Arc<str>, mpsc::UnboundedSender<WorkerCmd>>,
    natives: HashMap<String, Box<dyn NativeExt>>,
    native_for: NativeCtor,
    worker_tx: mpsc::Sender<WorkerMsg>,

    state: State,
    state_path: std::path::PathBuf,
    frecency_path: std::path::PathBuf,

    epoch: u64,
    raw: String,
    /// The parsed form of `raw`, kept so `publish` does not re-parse the same
    /// text on every provider answer.
    query: Arc<Query>,
    opened: bool,
    /// When the current open began — the log's `close` reports the summon.
    opened_at: Option<std::time::Instant>,
    /// Workers whose rows are on screen right now — `refreshMs` re-asks only
    /// a question whose answer is showing, so the engine tells each worker
    /// on the transition, not per publish.
    showing: HashSet<Arc<str>>,
    /// provider → the rows it last answered, and the epoch they belong to.
    /// Arc'd twice over: the worker's Rows message, the cache entry, and this
    /// bucket can all be the same allocation, and merging clones handles.
    buckets: HashMap<Arc<str>, (u64, crate::model::row::SharedRows)>,
    /// The definition each running worker was built with — a reload compares
    /// stamps, because a worker answers with the extension it was born as.
    worker_defs: HashMap<Arc<str>, u64>,
    waiting: HashSet<Arc<str>>,
    /// The merged list as it stands — what `activate` resolves keys against.
    rows: Vec<Arc<Row>>,

    /// The enter held for a placeholder row to resolve, and when it was
    /// asked — a keypress that fires a minute late is worse than one dropped,
    /// so the hold expires the way `holdEnter`'s three seconds did.
    pending_activate: Option<(String, Option<usize>, Instant)>,
    /// The action armed by a `confirm`, keyed by its namespaced id.
    pending_confirm: Option<String>,
    /// The row whose `previewExec` last ran — landing on it again does not
    /// run it twice.
    previewed: Option<String>,
    /// The first previewed row's `revertExec`, kept across later previews:
    /// the state to return to is the one you arrived in, not the one you
    /// last previewed — `Launcher.qml`'s `previewRevert`.
    preview_revert: String,

    /// A URL found on the clipboard at open, offered by `paste`.
    clipboard_url: Option<String>,
    /// The in-flight `ask` stream; a new question or a close kills it.
    ask_task: Option<tokio::task::JoinHandle<()>>,
    /// The first `askProviders` entry whose `when` answered — probed once
    /// per settings load, the way `checkAsk` ran once per config load.
    ask_provider: Option<crate::settings::AskProvider>,
    ask_probed: bool,
    /// Rows of a form being filled in (engine-side state for SaveSettings).
    known: HashSet<String>,
}

impl Engine {
    /// Build the engine: load settings, registry and state; spawn a worker
    /// per extension; return the handle and the event receiver.
    pub async fn start(
        extensions_dir: &Path,
        cmd_rx: mpsc::Receiver<EngineCmd>,
        evt_tx: mpsc::Sender<EngineEvent>,
        worker_tx: mpsc::Sender<WorkerMsg>,
        native_for: impl Fn(&str) -> Option<Box<dyn NativeExt>> + Send + Sync + 'static,
    ) -> Engine {
        let settings = Settings::load(&crate::settings::paths::settings_file());
        let load_t0 = std::time::Instant::now();
        let report = crate::registry::load_dir(extensions_dir, &settings.extensions);
        // The two birth events go through the same `log` gate every other
        // line does — emit_log does not exist yet, so the check is open.
        if settings.log {
            let _ = evt_tx
                .send(EngineEvent::Log {
                    ev: "sess".into(),
                    fields: json!({ "v": crate::PLUGIN_VERSION }),
                })
                .await;
            let _ = evt_tx
                .send(EngineEvent::Log {
                    ev: "ext.load".into(),
                    fields: json!({
                        "n": report.extensions.len(),
                        "ms": load_t0.elapsed().as_millis() as u64,
                    }),
                })
                .await;
        }
        if settings.log {
            for (path, why) in &report.bad {
                let _ = evt_tx
                    .send(EngineEvent::Log {
                        ev: "ext.bad".into(),
                        fields: json!({ "f": path.to_string_lossy(), "why": why }),
                    })
                    .await;
            }
        }
        let mut extensions = report.extensions;
        extensions.extend(builtin_extensions(&settings.quicklinks));

        let shared = Arc::new(Shared {
            cache: std::sync::Mutex::new(crate::support::cache::Cache::default()),
            availability: std::sync::Mutex::new(
                crate::support::availability::Availability::default(),
            ),
            settings: RwLock::new(Arc::new(settings.clone())),
            registry: RwLock::new(Arc::new(Vec::new())),
            hello_keywords: std::sync::RwLock::new(Arc::new(Vec::new())),
        });
        *shared.registry.write().await = Arc::new(extensions.clone());
        let extensions = Arc::new(extensions);

        let state = State::load(
            &crate::settings::paths::frecency_file(),
            &crate::settings::paths::state_file(),
        );

        let mut engine = Engine {
            cmd_rx,
            evt_tx,
            shared: shared.clone(),
            settings,
            extensions: extensions.clone(),
            workers: HashMap::new(),
            natives: HashMap::new(),
            native_for: Box::new(native_for),
            worker_tx,
            state,
            state_path: crate::settings::paths::state_file(),
            frecency_path: crate::settings::paths::frecency_file(),
            epoch: 0,
            raw: String::new(),
            query: Arc::new(Query::parse("", 0, None)),
            opened: false,
            opened_at: None,
            showing: HashSet::new(),
            buckets: HashMap::new(),
            worker_defs: HashMap::new(),
            waiting: HashSet::new(),
            rows: Vec::new(),
            pending_activate: None,
            pending_confirm: None,
            previewed: None,
            preview_revert: String::new(),
            clipboard_url: None,
            ask_task: None,
            ask_provider: None,
            ask_probed: false,
            known: HashSet::new(),
        };
        engine.rebuild_known();
        engine.spawn_workers(&extensions).await;
        engine
    }

    /// One worker per extension that can answer, and no worker for one that
    /// changed or left. A worker answers with the `Extension` it was built
    /// with, so a reload that only *added* workers would leave an edited
    /// extension answering with its old definition forever — the QML's
    /// Instantiator rebuilt every provider on registry change.
    async fn spawn_workers(&mut self, extensions: &Arc<Vec<Extension>>) {
        // The definitions this reload wants running, stamped so "same id,
        // new file" is a change, not a match.
        let wanted: HashMap<&str, u64> = extensions
            .iter()
            .filter(|e| !(e.native.is_empty() && e.search.is_empty() && e.socket.is_empty()))
            .map(|e| (e.id.as_str(), crate::registry::def_stamp(e)))
            .collect();

        // Departed or redefined: tell the task to exit, drop the handle and
        // the rows it built — a changed extension's stale answer is the old
        // definition's, which is exactly what the reload replaced.
        let stale: Vec<Arc<str>> = self
            .workers
            .keys()
            .filter(|id| {
                wanted
                    .get(id.as_ref())
                    .is_none_or(|stamp| self.worker_defs.get(*id) != Some(stamp))
            })
            .cloned()
            .collect();
        for id in stale {
            if let Some(tx) = self.workers.remove(&id) {
                let _ = tx.send(WorkerCmd::Shutdown);
            }
            self.worker_defs.remove(&id);
            self.showing.remove(&id);
            self.waiting.remove(&id);
            self.buckets.remove(&id);
        }

        for ext in extensions.iter() {
            if self.workers.contains_key(ext.id.as_str()) {
                continue;
            }
            if ext.native.is_empty() && ext.search.is_empty() && ext.socket.is_empty() {
                continue;
            }
            let native = self
                .natives
                .remove(&ext.id)
                .or_else(|| (self.native_for)(&ext.native));
            let (tx, rx) = mpsc::unbounded_channel::<WorkerCmd>();
            // Availability replays send to the worker's own queue.
            let self_tx = tx.clone();
            tokio::spawn(worker::run(
                ext.clone(),
                native,
                self_tx,
                rx,
                self.worker_tx.clone(),
                self.shared.clone(),
            ));
            // A worker born while the launcher is closed starts closed —
            // `Opened` is otherwise only sent on transitions, and a spawn
            // during reload is not one.
            if !self.opened {
                let _ = tx.send(WorkerCmd::Opened(false));
            }
            let id: Arc<str> = Arc::from(ext.id.as_str());
            self.worker_defs
                .insert(id.clone(), crate::registry::def_stamp(ext));
            self.workers.insert(id, tx);
        }
    }

    /// The daemon's handle for `hello`: reads the live keyword slot, so a
    /// client connecting after a reload is told the set as it stands.
    pub fn shared(&self) -> Arc<crate::provider::worker::Shared> {
        self.shared.clone()
    }

    /// The keyword set the parser validates against, for the hello a new
    /// client is owed — live: a reload updates the shared slot, so a client
    /// that connects later hears the set as it stands, not as it was at boot.
    pub fn keywords(&self) -> Vec<String> {
        self.shared
            .hello_keywords
            .read()
            .map(|k| (**k).clone())
            .unwrap_or_default()
    }

    /// The keyword set the parser validates against. Quicklink keywords
    /// arrive through the quicklinks extension's aliases — one routing table,
    /// not two — so this is exactly `known_keywords`.
    fn rebuild_known(&mut self) {
        self.known = known_keywords(&self.extensions, &builtin_keywords());
        // `hello` tells a freshly connected client the set as it stands now,
        // not as it stood at boot — reloads land here.
        let mut sorted: Vec<String> = self.known.iter().cloned().collect();
        sorted.sort();
        *self.shared.hello_keywords.write().unwrap() = Arc::new(sorted);
    }

    /// The main loop: one select over the command channel and the worker
    /// channel, so all the ordering is visible in one place.
    pub async fn run(mut self, mut worker_rx: mpsc::Receiver<WorkerMsg>) {
        // First contact, the same line the daemon writes per connect — an
        // in-process client gets the same wire shape a socket client does.
        self.emit(EngineEvent::Hello {
            version: crate::PLUGIN_VERSION.to_string(),
            keywords: self.keywords(),
        })
        .await;
        loop {
            tokio::select! {
                cmd = self.cmd_rx.recv() => {
                    let Some(cmd) = cmd else { return };
                    self.handle_cmd(cmd).await;
                }
                msg = worker_rx.recv() => {
                    let Some(msg) = msg else { continue };
                    // One keystroke's answers land in a burst — apply what has
                    // already arrived, then publish once. The QML collected a
                    // pass the same way: 46 merges and broadcasts for a single
                    // character was the measured cost of publishing per answer.
                    let mut dirty = self.handle_worker(msg).await;
                    for _ in 0..256 {
                        match worker_rx.try_recv() {
                            Ok(m) => dirty |= self.handle_worker(m).await,
                            Err(_) => break,
                        }
                    }
                    if dirty {
                        let q = self.query.clone();
                        self.publish(self.epoch, &q).await;
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------- commands

    async fn handle_cmd(&mut self, cmd: EngineCmd) {
        match cmd {
            EngineCmd::Open { text } => {
                // A summon re-sends the registry: the chips and the ask hint
                // want extension titles before the first answer lands.
                // Reopen wakes the workers too — without `Opened(true)` they
                // keep the closed flag from the last close: refresh never
                // re-arms and socket pushes are dropped on the floor.
                if !self.opened {
                    for tx in self.workers.values() {
                        let _ = tx.send(WorkerCmd::Opened(true));
                    }
                }
                self.opened = true;
                self.opened_at = Some(std::time::Instant::now());
                self.emit_log(
                    "open",
                    json!({ "q": crate::clip(&text, 120), "ep": self.epoch + 1 }),
                )
                .await;
                self.emit_registry().await;
                self.on_query(&text).await;
            }
            EngineCmd::Query { text, opened } => {
                if opened != self.opened {
                    for tx in self.workers.values() {
                        let _ = tx.send(WorkerCmd::Opened(opened));
                    }
                }
                self.opened = opened;
                self.on_query(&text).await;
            }
            EngineCmd::Close => self.on_close().await,
            EngineCmd::Activate {
                key,
                action,
                shift,
                ctrl,
            } => self.on_activate(&key, action, shift, ctrl).await,
            EngineCmd::Pin { key } => self.on_pin(&key).await,
            EngineCmd::Select { key } => self.on_select(&key),
            EngineCmd::CommitPreview => {
                // Enter on a previewed row commits it: no revert on the way
                // out, whatever it changed stays changed.
                self.previewed = None;
                self.preview_revert.clear();
            }
            EngineCmd::Set { key, value } => self.on_set(&key, value).await,
            EngineCmd::SaveSettings { id, values } => self.on_save_settings(&id, values).await,
            EngineCmd::Reload => self.on_reload().await,
            EngineCmd::RegisterNative { ext_id, native } => {
                self.natives.insert(ext_id, native);
            }
            EngineCmd::Clipboard { url } => {
                self.clipboard_url = url;
                // The read lands after the first paint, so an empty box
                // redraws to gain the row.
                if self.settings.recents && self.raw.trim().is_empty() {
                    let query = Arc::new(Query::parse(&self.raw, self.epoch, Some(&self.known)));
                    let rows = self.answer_paste(&query);
                    self.put_inline("paste", query.epoch, rows);
                    self.publish(query.epoch, &query).await;
                }
            }
            EngineCmd::Ask { text } => self.on_ask(&text).await,
            EngineCmd::StopAsk => self.stop_ask(),
            EngineCmd::Act { key, action } => {
                if let Some(row) = self.find_row(&key) {
                    self.run_action(&row, &action).await;
                }
            }
            EngineCmd::Log { ev, fields } => self.emit_log(ev, fields).await,
            EngineCmd::Ping => self.emit(EngineEvent::Pong).await,
        }
    }

    // -------------------------------------------------------------- queries

    async fn on_query(&mut self, text: &str) {
        // A confirmation belongs to the query that raised it. Typing anything
        // else is walking away from the question.
        if let Some(armed) = self.pending_confirm.clone()
            && text.trim() != format!("/{armed}")
        {
            self.pending_confirm = None;
        }

        self.epoch += 1;
        self.raw = text.to_string();
        let query = Arc::new(Query::parse(text, self.epoch, Some(&self.known)));
        self.query = query.clone();
        self.emit_log(
            "query",
            json!({ "ep": query.epoch, "q": crate::clip(text, 240), "s": query.scope }),
        )
        .await;

        // The inline answerers are the launcher asking itself: synchronous,
        // in the same pass, so their rows are never late.
        let help = self.answer_help(&query);
        let recents = self.answer_recents(&query);
        let paste = self.answer_paste(&query);
        let settings = self.answer_settings(&query).await;
        let actions = self.answer_actions(&query);
        self.put_inline("help", query.epoch, help);
        self.put_inline("recents", query.epoch, recents);
        self.put_inline("paste", query.epoch, paste);
        self.put_inline("settings", query.epoch, settings);
        self.put_inline("actions", query.epoch, actions);

        // Everyone else is a worker.
        for tx in self.workers.values() {
            let _ = tx.send(WorkerCmd::Ask(query.clone()));
        }
        self.waiting.clear();
        for ext in self.extensions.iter() {
            if !self.claims(&query, ext) {
                continue;
            }
            // The Arc<str> the worker map holds is what `waiting` carries —
            // so `WorkerMsg::Waiting`'s insert compares against the same key.
            if let Some(id) = self
                .workers
                .get_key_value(ext.id.as_str())
                .map(|(k, _)| k.clone())
            {
                self.waiting.insert(id);
            }
        }

        self.publish(query.epoch, &query).await;
    }

    /// Does this extension get asked this query at all? The same gates the
    /// worker applies, kept here so `waiting` matches what will answer.
    fn claims(&self, query: &Query, ext: &Extension) -> bool {
        if query.scope.is_empty() && !ext.always {
            return false;
        }
        query.routes_to(&ext.keyword, &ext.aliases)
    }

    async fn on_close(&mut self) {
        self.opened = false;
        self.emit_log(
            "close",
            json!({
                "ms": self.opened_at.map(|t| t.elapsed().as_millis() as u64).unwrap_or(0),
                "ep": self.epoch,
            }),
        )
        .await;
        self.opened_at = None;
        self.clipboard_url = None;
        self.stop_ask();
        // A held Enter must not fire into the next summon.
        self.pending_activate = None;
        self.pending_confirm = None;
        for tx in self.workers.values() {
            let _ = tx.send(WorkerCmd::Opened(false));
            let _ = tx.send(WorkerCmd::Showing(false));
        }
        self.showing.clear();
        // Whatever a preview changed goes back.
        self.unpreview();
    }

    // ------------------------------------------------------- inline answers

    /// `?` on its own, `h:`, `help:` or a bare `:`: every keyword the
    /// launcher knows, in the order a reader expects.
    fn answer_help(&self, query: &Query) -> Vec<Row> {
        let t = self.raw.trim();
        let help = matches!(t, "?" | ":" | "h:" | "help:")
            || (query.scope == "h" || query.scope == "help") && query.text.is_empty();
        if !help {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        let mut push =
            |group: &str, keyword: &str, title: &str, aliases: Vec<String>, glyph: &str| {
                let name = keyword.to_lowercase();
                if name.is_empty() || !seen.insert(name.clone()) {
                    return;
                }
                let mut row = fill_row(FillSpec {
                    provider: "help",
                    key: &format!("help:{name}"),
                    group,
                    title,
                    subtitle: &aliases.join(", "),
                    accessory: &format!("{name}:"),
                    glyph,
                    fill: &format!("{name}:"),
                    verb: "Use Keyword",
                });
                // Listed in the order the list was built, so the rank only
                // preserves it.
                row.score = rank::score(rank::TIER_FORCED, 90000 - out.len() as i64 * 200, 0);
                out.push(row);
            };

        // Built-ins first, then extensions, then quicklinks: most general to
        // most personal.
        for (kw, title, aliases) in builtin_help() {
            let mut alias = aliases.iter().map(|s| s.to_string()).collect::<Vec<_>>();
            for s in crate::model::query::sigils_for(kw) {
                alias.push(s.to_string());
            }
            push("Built In", kw, title, alias, "");
        }
        push("Built In", "settings", "Settings", vec![], "");
        for ext in self.extensions.iter() {
            if ext.builtin {
                continue;
            }
            let mut alias = ext.aliases.clone();
            for s in crate::model::query::sigils_for(&ext.keyword) {
                alias.push(s.to_string());
            }
            push("Extensions", &ext.keyword, &ext.title, alias, &ext.glyph);
        }
        for link in &self.settings.quicklinks {
            if link.keyword.is_empty() {
                continue;
            }
            push(
                "Quicklinks",
                &link.keyword,
                &link.title,
                vec![],
                &link.glyph,
            );
        }
        out
    }

    /// What you ran last, on an empty box — off unless `recents` is set.
    fn answer_recents(&self, _query: &Query) -> Vec<Row> {
        if !(self.settings.recents && self.raw.trim().is_empty()) {
            return Vec::new();
        }
        self.state
            .recents
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let mut row = fill_row(FillSpec {
                    provider: "recents",
                    key: &format!("past:{entry}"),
                    group: "Recent",
                    title: entry,
                    subtitle: "",
                    accessory: "",
                    glyph: "",
                    fill: entry,
                    verb: "Search Again",
                });
                row.score = rank::score(rank::TIER_FORCED, 90000 - i as i64 * 200, 0);
                row
            })
            .collect()
    }

    /// A URL on the clipboard, offered as the first row of an empty box.
    fn answer_paste(&self, _query: &Query) -> Vec<Row> {
        let Some(url) = self.clipboard_url.clone() else {
            return Vec::new();
        };
        if !(self.settings.recents && self.raw.trim().is_empty()) {
            return Vec::new();
        }
        let mut row = Row::new(format!("paste:{url}"), "paste");
        row.group = "Clipboard".into();
        row.title = url.clone();
        row.detail = "On the clipboard".into();
        row.accessory = "Open".into();
        // A link glyph, so the row reads as something you copied before the
        // URL itself has been read at all.
        row.icon_glyph = "\u{f0c1}".into();
        row.score = rank::score(rank::TIER_FORCED, 95000, 0);
        row.exec = format!(
            "omarchy-launch-browser {}",
            crate::support::quote::quote(&url)
        );
        let search = self
            .settings
            .engine(&self.settings.default_engine)
            .map(|e| e.url.replace("{}", &crate::settings::url_encode(&url)))
            .unwrap_or_default();
        row.actions = Some(vec![
            Action {
                title: "Open Link".into(),
                shortcut: "↵".into(),
                exec: row.exec.clone(),
                ..Action::default()
            },
            Action {
                title: "Search For It".into(),
                exec: format!(
                    "omarchy-launch-browser {}",
                    crate::support::quote::quote(&search)
                ),
                ..Action::default()
            },
        ]);
        vec![row]
    }

    /// `settings:` — extensions that declared fields, then the form for the
    /// one picked.
    async fn answer_settings(&self, query: &Query) -> Vec<Row> {
        if query.scope != "settings" {
            return Vec::new();
        }
        let arg = query.arg_for("settings", &[]).trim().to_lowercase();

        // A picked extension with fields is a form, not a list.
        if let Some(ext) = self.extensions.iter().find(|e| e.id == arg)
            && !ext.settings.is_empty()
        {
            return vec![self.settings_form(ext)];
        }

        self.extensions
            .iter()
            .filter(|ext| !ext.settings.is_empty())
            .filter(|ext| {
                arg.is_empty()
                    || ext.id.starts_with(&arg)
                    || ext.title.to_lowercase().contains(&arg)
            })
            .enumerate()
            .map(|(i, ext)| {
                let saved = self.settings.settings_for(&ext.id);
                let filled = ext
                    .settings
                    .iter()
                    .filter(|s| {
                        saved
                            .and_then(|m| m.get(&s.key))
                            .and_then(|v| v.as_str())
                            .is_some_and(|v| !v.is_empty())
                    })
                    .count();
                let mut row = Row::new(format!("settings:{}", ext.id), "settings");
                row.group = "Settings".into();
                row.title = ext.title.clone();
                row.detail = format!("{} of {} set", filled, ext.settings.len());
                row.accessory = format!("{}:", ext.keyword);
                row.icon_glyph = ext.glyph.clone();
                row.score = rank::score(rank::TIER_FORCED, 90000 - i as i64 * 200, 0);
                // A query and nothing else: choosing one is a step further
                // in, and Escape is what undoes it.
                let mut act = Action {
                    title: "Edit".into(),
                    shortcut: "↵".into(),
                    ..Action::default()
                };
                act.extra
                    .insert("query".into(), json!(format!("settings:{}", ext.id)));
                row.actions = Some(vec![act]);
                row
            })
            .collect()
    }

    /// The form row for one extension.
    fn settings_form(&self, ext: &Extension) -> Row {
        let saved = self.settings.settings_for(&ext.id);
        let fields: Vec<Value> = ext
            .settings
            .iter()
            .filter(|s| !s.key.is_empty())
            .map(|s| {
                // What is saved wins over what the extension suggested: the
                // suggestion is a default, and a default that overwrote an
                // answer would not be one.
                let value = saved
                    .and_then(|m| m.get(&s.key))
                    .and_then(|v| v.as_str())
                    .unwrap_or(&s.value)
                    .to_string();
                json!({
                    "name": s.key,
                    "label": if s.label.is_empty() { &s.key } else { &s.label },
                    "value": value,
                    "placeholder": s.placeholder,
                    "secret": s.secret,
                })
            })
            .collect();

        let mut row = Row::new(format!("settings:form:{}", ext.id), "settings");
        row.group = "Settings".into();
        row.view = "form".into();
        row.title = ext.title.clone();
        row.subtitle = "Saved to ~/.config/omarchy/oxy.json".into();
        row.extra.insert("submit".into(), json!("Save"));
        row.extra.insert("fields".into(), json!(fields));
        row.extra.insert("ext".into(), json!(ext.id));
        let mut act = Action::default();
        act.extra.insert("query".into(), json!("settings:"));
        row.actions = Some(vec![act]);
        row.score = rank::score(rank::TIER_FORCED, 99000, 0);
        row
    }

    /// `/` — everything the launcher can do to itself, plus the actions
    /// extensions declare.
    fn answer_actions(&self, query: &Query) -> Vec<Row> {
        if query.scope != "command" {
            return Vec::new();
        }
        let arg = query.arg_for("command", &[]).trim().to_lowercase();
        let mut out = Vec::new();

        let all = self.all_actions();
        for (i, action) in all.iter().enumerate() {
            let entry = crate::support::score::Entry {
                id: String::new(),
                name: action.title.clone(),
                generic_name: action.subtitle.clone(),
                comment: String::new(),
                keywords: action.keywords.clone(),
                payload: Value::Null,
            };
            let fuzzy = if arg.is_empty() {
                0
            } else {
                crate::support::score::fuzzy(&entry, &arg)
            };
            // A confirm armed on this action keeps it in the list even though
            // the retyped `/id` no longer matches its keywords.
            let confirming = self
                .pending_confirm
                .as_ref()
                .is_some_and(|id| *id == action.id);
            if fuzzy < 0 && !confirming {
                continue;
            }

            let mut row = Row::new(format!("action:{}", action.id), "actions");
            row.group = "Commands".into();
            row.title = action.title.clone();
            row.subtitle = action.subtitle.clone();
            row.icon_glyph = action.glyph.clone();
            row.extra
                .insert("keepOpen".into(), json!(action.exec.is_empty()));
            // Declared order always — a fuzzy score is not a better one.
            row.tier = rank::TIER_FORCED;
            row.local = 900 - i as i64;
            row.score = rank::score(rank::TIER_FORCED, row.local, 0);
            let mut act = action.clone();
            act.shortcut = "↵".into();
            row.actions = Some(vec![act]);
            out.push(row);
        }
        out
    }

    /// The built-in actions plus each extension's, namespaced by id.
    fn all_actions(&self) -> Vec<Action> {
        let mut out = builtin_actions();
        for ext in self.extensions.iter() {
            for (j, a) in ext.actions.iter().enumerate() {
                if a.title.is_empty() {
                    continue;
                }
                let mut action = a.clone();
                action.id = format!(
                    "{}.{}",
                    ext.id,
                    if a.id.is_empty() {
                        j.to_string()
                    } else {
                        a.id.clone()
                    }
                );
                if action.subtitle.is_empty() {
                    action.subtitle = ext.title.clone();
                }
                if action.glyph.is_empty() {
                    action.glyph = ext.glyph.clone();
                }
                // The extension's id and the action's own id are keywords too,
                // so `/spotify auth` reaches it — and so does `/spotify.auth`,
                // which is what a `confirm` re-types.
                let mut kw = vec![ext.keyword.clone(), ext.id.clone(), a.id.clone()];
                kw.extend(a.keywords.clone());
                action.keywords = kw;
                out.push(action);
            }
        }
        out
    }

    // ------------------------------------------------------------- merging

    /// An inline provider answered: write its bucket and rebuild.
    fn put_inline(&mut self, provider: &'static str, epoch: u64, rows: Vec<Row>) {
        self.buckets.insert(
            Arc::from(provider),
            (epoch, Arc::new(rows.into_iter().map(Arc::new).collect())),
        );
    }

    /// A worker answered: write its bucket, clear its wait. Returns whether a
    /// publish is owed — `run` batches a keystroke's burst and publishes once.
    async fn handle_worker(&mut self, msg: WorkerMsg) -> bool {
        match msg {
            WorkerMsg::Rows {
                id,
                epoch,
                rows,
                last,
            } => {
                if epoch != self.epoch {
                    self.emit_log("drop", json!({ "id": id, "got": epoch, "ep": self.epoch }))
                        .await;
                    return false;
                }
                self.buckets.insert(id.clone(), (epoch, rows));
                if last {
                    self.waiting.remove(&*id);
                }
                true
            }
            WorkerMsg::Waiting { id, epoch } => {
                if epoch == self.epoch {
                    self.waiting.insert(id);
                }
                false
            }
            WorkerMsg::Log { id, ev, fields } => {
                let mut f = fields.as_object().cloned().unwrap_or_default();
                f.insert("id".into(), json!(id));
                self.emit_log(ev, Value::Object(f)).await;
                false
            }
        }
    }

    /// Merge every bucket, apply frecency and pins, publish the result — and
    /// fire the Enter a placeholder was holding. `query` is the already-parsed
    /// question — publish runs once per provider answer, so re-parsing the
    /// same text each time would be per-keystroke waste.
    async fn publish(&mut self, epoch: u64, query: &Query) {
        let t0 = std::time::Instant::now();
        let buckets: Vec<(&str, &[Arc<Row>])> = self
            .buckets
            .iter()
            .map(|(id, (_, rows))| (&**id, rows.as_slice()))
            .collect();
        let mut rows = rank::merge(&buckets, &query.scope, 60);

        let mut changed = false;
        if self.settings.frecency {
            let now = now_ms();
            changed = state::frecency_apply(&mut rows, &self.state.frecency, now, &self.raw);
        }
        // `|` not `||` — pins_apply's flag writes must run even when frecency
        // already scored; it returns true only when a *score* moved.
        changed = state::pins_apply(&mut rows, &self.state.pins) || changed;
        if changed {
            rows.sort_by_cached_key(|r| rank::sort_key(r));
        }

        // Which providers' visible rows are older than the question.
        let stale: Vec<Arc<str>> = self
            .buckets
            .iter()
            .filter(|(_, (ep, rows))| *ep != epoch && !rows.is_empty())
            .map(|(id, _)| id.clone())
            .collect();

        let view = rows
            .first()
            .map(|r| {
                if r.view.is_empty() {
                    "list".to_string()
                } else {
                    r.view.clone()
                }
            })
            .unwrap_or_else(|| "list".to_string());
        let scope_label = self.scope_label(query);
        let help_mode = matches!(self.raw.trim(), "?" | ":" | "h:" | "help:");
        let recent_mode = self.settings.recents && self.raw.trim().is_empty();
        // The armed action's prompt, while one is armed: the empty state
        // draws it rather than pretending the box is a fresh search.
        let confirm = self
            .pending_confirm
            .as_ref()
            .and_then(|id| self.all_actions().into_iter().find(|a| a.id == *id))
            .map(|a| a.confirm)
            .unwrap_or_default();

        self.rows = rows;

        // `refreshMs`'s gate: a worker re-asks only while its rows are on
        // screen. Sent on transitions only — a resend per publish would be a
        // message per provider per keystroke for nothing. Closed is closed:
        // a `query` op with `opened:false` (the CLI) must not flip a worker's
        // showing bit for rows nobody is looking at.
        if self.opened {
            let mut visible: HashSet<&str> = HashSet::new();
            for row in &self.rows {
                visible.insert(row.provider_id.as_str());
            }
            for (id, tx) in &self.workers {
                let on = visible.contains(id.as_ref());
                if on != self.showing.contains(id) {
                    let _ = tx.send(WorkerCmd::Showing(on));
                    if on {
                        self.showing.insert(id.clone());
                    } else {
                        self.showing.remove(id);
                    }
                }
            }
        }

        // Rows arriving without a single `previewExec` among them are the
        // preview leaving: whatever it changed goes back.
        if self.previewed.is_some() && !self.rows.iter().any(|r| !r.preview_exec.is_empty()) {
            self.unpreview();
        }
        let row_count = self.rows.len();
        let _ = self
            .evt_tx
            .send(EngineEvent::Results {
                epoch,
                rows: self.rows.clone(),
                waiting: self.waiting.iter().cloned().collect(),
                stale,
                scope: query.scope.clone(),
                scope_label,
                help_mode,
                recent_mode,
                view,
                confirm,
            })
            .await;
        self.emit_log(
            "rebuild",
            json!({ "ep": epoch, "ms": t0.elapsed().as_millis() as u64, "rows": row_count }),
        )
        .await;

        // A queued Enter fires as soon as its placeholder resolves — and a
        // held one expires rather than firing against a query the user has
        // long since moved past.
        if let Some((key, action, armed)) = self.pending_activate.clone() {
            if armed.elapsed() > std::time::Duration::from_secs(3) {
                self.pending_activate = None;
            } else {
                // An empty key asks for whatever leads the list now.
                let target = if key.is_empty() {
                    self.rows.iter().find(|r| !r.pending)
                } else {
                    self.rows.iter().find(|r| r.key == key && !r.pending)
                };
                if let Some(row) = target {
                    let key = row.key.clone();
                    self.pending_activate = None;
                    Box::pin(self.on_activate(&key, action, false, false)).await;
                }
            }
        }
    }

    /// What the active filter is called, for the header chip.
    fn scope_label(&self, query: &Query) -> String {
        // Help mode's chip names what the list is, not the filter it parsed.
        if matches!(self.raw.trim(), "?" | ":" | "h:" | "help:") {
            return "Keywords".into();
        }
        if query.scope.is_empty() {
            return String::new();
        }
        if query.scope == "settings" {
            return "Settings".into();
        }
        for ext in self.extensions.iter() {
            // The synthesized quicklinks extension's aliases are the links'
            // keywords — the chip names the link, not its transport, so only
            // its own keyword counts as an extension match.
            if ext.id == "quicklinks" {
                if ext.keyword == query.scope {
                    return ext.title.clone();
                }
                continue;
            }
            if ext.keyword == query.scope || ext.aliases.contains(&query.scope) {
                return ext.title.clone();
            }
        }
        for link in &self.settings.quicklinks {
            if link.keyword.to_lowercase() == query.scope {
                return link.title.clone();
            }
        }
        for (kw, title, aliases) in builtin_help() {
            if kw == &query.scope || aliases.contains(&query.scope.as_str()) {
                return title.to_string();
            }
        }
        query.scope.clone()
    }

    // ------------------------------------------------------------ activating

    fn find_row(&self, key: &str) -> Option<Arc<Row>> {
        self.rows.iter().find(|r| r.key == key).cloned()
    }

    async fn on_activate(
        &mut self,
        key: &str,
        action_index: Option<usize>,
        shift: bool,
        ctrl: bool,
    ) {
        // Shift+Enter is the second action, by keystroke rather than through
        // the panel — "ask ChatGPT" instead of Google.
        let action_index = action_index.or(shift.then_some(1));
        // An empty key is Enter on a list that has not caught up: it resolves
        // to whatever leads once something does.
        let key = if key.is_empty() {
            match self.rows.iter().find(|r| !r.pending) {
                Some(row) => row.key.clone(),
                None => {
                    self.pending_activate = Some((String::new(), action_index, Instant::now()));
                    return;
                }
            }
        } else {
            key.to_string()
        };
        let Some(row) = self.find_row(&key) else {
            // Enter with nothing under it is held, not dropped: the answer may
            // be one debounce away.
            self.pending_activate = Some((key, action_index, Instant::now()));
            return;
        };

        // Enter on a stale row ran the query the row was built from rather
        // than the one on screen. Held instead, and run when the answer lands.
        if let Some((ep, rows)) = self.buckets.get(row.provider_id.as_str())
            && *ep != self.epoch
            && !rows.is_empty()
        {
            self.pending_activate = Some((key, action_index, Instant::now()));
            return;
        }

        // A keyword from `?`, or a query you ran before: the point of picking
        // one is to carry on typing, so nothing runs and nothing closes.
        if !row.fill.is_empty() {
            self.emit(EngineEvent::Type {
                text: row.fill.clone(),
                flow: false,
                poll: false,
            })
            .await;
            return;
        }

        // Ctrl+C copies; Ctrl+Enter asks. Both are frontend keys in the old
        // launcher, surfaced here as modifiers on activate.
        if ctrl {
            let text = row
                .extra
                .get("copyText")
                .and_then(|v| v.as_str())
                .map(String::from)
                .or_else(|| {
                    if row.detail.is_empty() {
                        Some(row.title.clone())
                    } else {
                        Some(row.detail.clone())
                    }
                })
                .unwrap_or_default();
            if !text.is_empty() {
                crate::provider::process::run_detached(&format!(
                    "printf %s {} | wl-copy",
                    crate::support::quote::quote(&text)
                ));
                self.emit(EngineEvent::Notice {
                    text: "Copied".into(),
                })
                .await;
            }
            return;
        }

        let action = action_index
            .and_then(|i| row.actions.as_ref().and_then(|a| a.get(i)).cloned())
            .or_else(|| row.actions.as_ref().and_then(|a| a.first()).cloned());

        if let Some(action) = action {
            return self.run_action(&row, &action).await;
        }

        if row.pending {
            self.pending_activate = Some((key, action_index, Instant::now()));
            return;
        }

        // A normal row: record, preview-commit, run, close.
        self.remember(&row);
        self.remember_query();
        self.remember_mru(&row.extra);
        self.emit_log(
            "act",
            json!({ "ep": self.epoch, "id": row.key, "src": row.provider_id, "t": row.title }),
        )
        .await;
        self.previewed = None;
        self.preview_revert.clear();

        // A row that behaves like a chat: Enter sends, and the box is emptied
        // for the next thing while what was sent stays on the card.
        if row
            .extra
            .get("keepOpen")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            if !row.exec.is_empty() {
                crate::provider::process::run_detached(&row.exec);
            }
            if let Some(clear) = row.extra.get("clearTo").and_then(|v| v.as_str()) {
                self.emit(EngineEvent::Type {
                    text: clear.to_string(),
                    flow: false,
                    poll: false,
                })
                .await;
            }
            return;
        }

        // Close first: launching while an exclusive-focus layer surface is
        // still mapped puts the new window behind it, and Omarchy's launch
        // OSD would render underneath this overlay. The socket write is
        // ordered, so the unmap starts at worst one frame before the spawn.
        self.emit(EngineEvent::Close).await;
        if !row.exec.is_empty() {
            crate::provider::process::run_detached(&row.exec);
        }
    }

    /// An action on a row: the panel's choice, or Enter on a row that carries
    /// one.
    async fn run_action(&mut self, row: &Row, action: &Action) {
        // A confirmation asks in the box: a dialog would take the keyboard
        // from an overlay that holds it exclusively.
        if !action.confirm.is_empty() && self.pending_confirm.as_deref() != Some(action.id.as_str())
        {
            self.pending_confirm = Some(action.id.clone());
            let text = format!("/{} ", action.id);
            self.emit(EngineEvent::Type {
                text: text.clone(),
                flow: false,
                poll: false,
            })
            .await;
            // Re-ask the text the box will hold, not the text that fired the
            // action: on_query's walking-away rule clears an arm whose text
            // differs, and a client that ignores `type` still sees the prompt.
            self.on_query(&text).await;
            return;
        }
        self.pending_confirm = None;
        self.emit_log("action", json!({ "id": action.id, "ep": self.epoch }))
            .await;

        // Effects the engine performs itself; anything with `exec` instead is
        // a shell command, which is how an extension adds one.
        match action.effect.as_str() {
            "clear.recents" => {
                self.state.recents.clear();
                self.save_state();
                self.emit(EngineEvent::Notice {
                    text: "Recent queries cleared".into(),
                })
                .await;
                self.on_query(&self.raw.clone()).await;
                return;
            }
            "clear.pins" => {
                self.state.pins.clear();
                self.save_state();
                self.emit(EngineEvent::Notice {
                    text: "Pins cleared".into(),
                })
                .await;
                self.on_query(&self.raw.clone()).await;
                return;
            }
            "clear.all" => {
                self.state.recents.clear();
                self.state.pins.clear();
                self.save_state();
                self.emit(EngineEvent::Notice {
                    text: "History and pins cleared".into(),
                })
                .await;
                self.on_query(&self.raw.clone()).await;
                return;
            }
            "reload.extensions" => {
                self.on_reload().await;
                self.emit(EngineEvent::Notice {
                    text: "Extensions reloaded".into(),
                })
                .await;
                return;
            }
            "open.settings" => {
                self.emit(EngineEvent::Type {
                    text: "settings:".into(),
                    flow: false,
                    poll: false,
                })
                .await;
                return;
            }
            "stats" => {
                let cached = self.shared.cache.lock().unwrap().entries();
                let checks = self.shared.availability.lock().unwrap().stats();
                self.emit(EngineEvent::Notice {
                    text: format!(
                        "{} extensions · {} answers cached · {} checks, {} ok",
                        self.extensions.len(),
                        cached,
                        checks.0,
                        checks.1
                    ),
                })
                .await;
                return;
            }
            _ => {}
        }

        let follow_up = action
            .extra
            .get("query")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_default();
        let stay_open = action.keep_open;

        self.remember(row);
        self.remember_query();
        self.remember_mru(&action.extra);

        if let Some(row_key) = action.extra.get("row").and_then(|v| v.as_str()) {
            // A self-reference: activate the row it names.
            let key = row_key.to_string();
            return Box::pin(self.on_activate(&key, None, false, false)).await;
        }

        if !action.exec.is_empty() {
            let exec = action.exec.clone();
            if follow_up.is_empty() && !stay_open {
                self.emit(EngineEvent::Close).await;
            }
            crate::provider::process::run_detached(&exec);
        }

        if !follow_up.is_empty() {
            // An exec behind the follow-up lands whenever it lands — a player
            // starting, a daemon answering — so the box is typed now and the
            // query re-asked a few times while it catches up. A follow-up with
            // nothing running is just somewhere to go.
            self.emit(EngineEvent::Type {
                text: follow_up,
                flow: true,
                poll: !action.exec.is_empty(),
            })
            .await;
        } else if stay_open {
            // The action changed something the current answer shows: re-ask
            // on the poll cadence rather than once, so a slow effect still
            // arrives.
            self.emit(EngineEvent::Type {
                text: self.raw.clone(),
                flow: false,
                poll: true,
            })
            .await;
        }
        // An action carrying nothing else does nothing; the launcher stays put.
    }

    // ---------------------------------------------------------------- pins

    async fn on_pin(&mut self, key: &str) {
        let Some(row) = self.find_row(key) else {
            return;
        };
        state::pin_toggle(&mut self.state.pins, &row.key);
        self.save_state();
        self.emit(EngineEvent::Notice {
            text: if state::pin_has(&self.state.pins, &row.key) {
                "Pinned".into()
            } else {
                "Unpinned".into()
            },
        })
        .await;
        let epoch = self.epoch;
        let query = self.query.clone();
        self.publish(epoch, &query).await;
    }

    // ------------------------------------------------------------- preview

    fn on_select(&mut self, key: &str) {
        let Some(row) = self.find_row(key) else {
            return;
        };
        if row.preview_exec.is_empty() {
            return;
        }
        // Landing on the same row again does not run it twice. Moving to a
        // different preview does not undo the last one — a preview chain is
        // undone once, from the state you arrived in.
        if self.previewed.as_deref() == Some(row.key.as_str()) {
            return;
        }
        if self.preview_revert.is_empty() && !row.revert_exec.is_empty() {
            self.preview_revert = row.revert_exec.clone();
        }
        crate::provider::process::run_detached(&row.preview_exec);
        self.previewed = Some(row.key.clone());
    }

    fn unpreview(&mut self) {
        self.previewed = None;
        if !self.preview_revert.is_empty() {
            let revert = std::mem::take(&mut self.preview_revert);
            crate::provider::process::run_detached(&revert);
        }
    }

    // ----------------------------------------------------------------- ask

    /// Ctrl+Enter: the first `askProviders` entry whose `when` answers is run
    /// with the question, and its stdout streams back as `Answer` events.
    async fn on_ask(&mut self, question: &str) {
        self.stop_ask();
        if question.trim().is_empty() {
            return;
        }

        // The probe ran at registry emit, once per settings load — a wrong
        // guess costs more than a hundred milliseconds, but probing on every
        // question costs it every time.
        if !self.ask_probed {
            self.probe_ask().await;
        }
        let spec = self.ask_provider.clone();

        self.emit(EngineEvent::AnswerStart {
            question: question.to_string(),
            provider: spec.as_ref().map(|p| p.title.clone()).unwrap_or_default(),
        })
        .await;

        let Some(spec) = spec else {
            self.emit(EngineEvent::AnswerDone {
                error: "No ask provider is installed".into(),
            })
            .await;
            return;
        };

        let command = spec
            .command
            .replace("{model}", &spec.model)
            .replace("{query}", &crate::support::quote::quote(question));
        // stdbuf so a line-buffered model actually streams; stderr folded in
        // so a real failure is visible rather than silent.
        let body = format!("stdbuf -oL {command} < /dev/null 2>&1");
        let Some(mut child) = crate::provider::process::spawn_stream(&body) else {
            self.emit(EngineEvent::AnswerDone {
                error: "Could not start the ask command".into(),
            })
            .await;
            return;
        };

        let evt = self.evt_tx.clone();
        self.ask_task = Some(tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt;
            let mut answered = false;
            if let Some(stdout) = child.stdout.take() {
                let mut lines = tokio::io::BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    answered = true;
                    if evt.send(EngineEvent::Answer { line }).await.is_err() {
                        return;
                    }
                }
            }
            let code = child.wait().await.ok().and_then(|s| s.code()).unwrap_or(-1);
            // An exit code only becomes the error when nothing else did —
            // a model that answered and then exited 1 still answered.
            let error = if code != 0 && !answered {
                format!("That command exited {code}. Check askProviders in oxy.json.")
            } else {
                String::new()
            };
            let _ = evt.send(EngineEvent::AnswerDone { error }).await;
        }));
    }

    /// Kill the stream, if one is running. Idempotent.
    fn stop_ask(&mut self) {
        if let Some(task) = self.ask_task.take() {
            task.abort();
        }
    }

    /// The first `askProviders` entry whose `when` answers, in list order —
    /// `probeNextProvider`'s port. Runs once per settings load; the probes are
    /// serial because a wrong guess costs more than the wait does.
    async fn probe_ask(&mut self) {
        self.ask_probed = true;
        self.ask_provider = None;
        for provider in self.settings.ask_ordered() {
            if provider.when.is_empty() || crate::provider::process::check(&provider.when).await {
                self.ask_provider = Some(provider.clone());
                break;
            }
        }
    }

    /// The registry event, sent on every open transition and every reload:
    /// the frontend's source chips, keyword help and ask hint all read it.
    async fn emit_registry(&mut self) {
        if !self.ask_probed {
            self.probe_ask().await;
        }
        let ask = match &self.ask_provider {
            Some(p) => json!({ "available": true, "model": p.title }),
            None => json!({ "available": false, "model": "" }),
        };
        self.emit(EngineEvent::Registry {
            extensions: self
                .extensions
                .iter()
                .map(|e| {
                    json!({
                        "id": e.id, "title": e.title, "keyword": e.keyword,
                        "aliases": e.aliases, "glyph": e.glyph, "accent": e.accent,
                        "view": e.view,
                    })
                })
                .collect(),
            ask,
        })
        .await;
    }

    // ----------------------------------------------------------------- set

    async fn on_set(&mut self, key: &str, value: f64) {
        let Some(row) = self.find_row(key) else {
            return;
        };
        if row.set_exec.is_empty() {
            return;
        }
        let rendered = row.set_exec.replace("{value}", &format!("{value}"));
        crate::provider::process::run_detached(&rendered);
        // Sliders keep the launcher open; the next refresh draws the truth.
    }

    // ------------------------------------------------------------- settings

    /// The form's answers, written through the file's own text so nothing the
    /// user wrote is lost and nothing we defaulted to is recorded as chosen.
    async fn on_save_settings(&mut self, id: &str, values: Map<String, Value>) {
        let path = crate::settings::paths::settings_file();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        let trailing_newline = text.ends_with('\n');
        let Ok(mut raw) = serde_json::from_str::<Value>(&text) else {
            return;
        };
        let entry = raw
            .as_object_mut()
            .unwrap()
            .entry("extensionSettings".to_string())
            .or_insert_with(|| json!({}));
        if !entry.is_object() {
            *entry = json!({});
        }
        entry
            .as_object_mut()
            .unwrap()
            .insert(id.to_string(), Value::Object(values));
        let mut text = serde_json::to_string_pretty(&raw).unwrap_or_default();
        // Keep the file's ending: a trailing newline stays, its absence
        // stays absent — the write changes one subtree, not the file shape.
        if trailing_newline && !text.ends_with('\n') {
            text.push('\n');
        }
        // Atomic, the way every file this launcher owns is written.
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, &text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
        self.settings = Settings::load(&path);
        *self.shared.settings.write().await = Arc::new(self.settings.clone());
        // Back to the list, on the poll cadence: the write above re-reads
        // the file here, but the providers reading it next need the passes.
        self.emit(EngineEvent::Type {
            text: "settings:".into(),
            flow: true,
            poll: true,
        })
        .await;
    }

    // -------------------------------------------------------------- reload

    async fn on_reload(&mut self) {
        self.settings = Settings::load(&crate::settings::paths::settings_file());
        *self.shared.settings.write().await = Arc::new(self.settings.clone());
        let load_t0 = std::time::Instant::now();
        let report = crate::registry::load_dir(
            &crate::settings::paths::extensions_dir(),
            &self.settings.extensions,
        );
        self.emit_log(
            "ext.load",
            json!({
                "n": report.extensions.len(),
                "ms": load_t0.elapsed().as_millis() as u64,
            }),
        )
        .await;
        for (path, why) in &report.bad {
            self.emit_log(
                "ext.bad",
                json!({ "f": path.to_string_lossy(), "why": why }),
            )
            .await;
        }
        let mut extensions = report.extensions;
        extensions.extend(builtin_extensions(&self.settings.quicklinks));
        self.extensions = Arc::new(extensions);
        *self.shared.registry.write().await = self.extensions.clone();
        self.rebuild_known();
        // Reconcile workers with the new definitions: arrivals spawn,
        // departures and redefinitions are shut down and respawned — the
        // cache survives in `Shared`, keyed by command, so an unchanged
        // answer still hits.
        self.spawn_workers(&self.extensions.clone()).await;
        // The ask probe's answer may have changed with the settings.
        self.ask_probed = false;
        self.emit_registry().await;
        self.on_query(&self.raw.clone()).await;
    }

    // --------------------------------------------------------------- state

    /// Rows whose key changes on every keystroke teach nothing: a calculator
    /// answer is keyed by its expression and a web search by its terms.
    fn remember(&mut self, row: &Row) {
        if !self.settings.frecency || row.key.is_empty() {
            return;
        }
        if NO_FRECENCY.contains(&row.provider_id.as_str()) {
            return;
        }
        state::frecency_record(&mut self.state.frecency, &row.key, now_ms(), &self.raw);
        self.save_state();
    }

    /// A provider-declared recency write: `"remember": {"file", "value"}`
    /// on the row or the action. Runs before the exec it rides with, the way
    /// `oxy-emoji --used` ran before the copy.
    fn remember_mru(&self, extra: &serde_json::Map<String, Value>) {
        let Some(r) = extra.get("remember").and_then(|v| v.as_object()) else {
            return;
        };
        let (Some(file), Some(value)) = (
            r.get("file").and_then(|v| v.as_str()),
            r.get("value").and_then(|v| v.as_str()),
        ) else {
            return;
        };
        let keep = r.get("keep").and_then(|v| v.as_u64()).unwrap_or(24) as usize;
        let dir = self
            .state_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| crate::settings::paths::state_home().join("omarchy"));
        state::mru_record(&dir, file, value, keep);
    }

    /// The question, not the answer.
    fn remember_query(&mut self) {
        if !self.settings.recents {
            return;
        }
        // A `/` action is an instruction, not a question.
        if self.raw.trim_start().starts_with('/') {
            return;
        }
        state::recents_record(&mut self.state.recents, &self.raw);
        self.save_state();
    }

    fn save_state(&mut self) {
        // What decayed to nothing goes now, on the same cadence the QML used:
        // the file holds only keys that still mean a launch.
        crate::state::frecency_prune(&mut self.state.frecency, now_ms());
        let _ = self.state.save(&self.frecency_path, &self.state_path);
    }

    // --------------------------------------------------------------- emit

    async fn emit(&self, event: EngineEvent) {
        let _ = self.evt_tx.send(event).await;
    }

    async fn emit_log(&self, ev: impl Into<String>, fields: Value) {
        if self.settings.log {
            let _ = self
                .evt_tx
                .send(EngineEvent::Log {
                    ev: ev.into(),
                    fields,
                })
                .await;
        }
    }

    /// The clipboard read, once per open. The daemon hands the engine a
    /// oneshot this fills.
    pub fn set_clipboard_url(&mut self, url: Option<String>) {
        self.clipboard_url = url;
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Everything a fill row needs — named fields so the two call sites stay
/// readable instead of juggling positional `&str`s.
struct FillSpec<'a> {
    provider: &'a str,
    key: &'a str,
    group: &'a str,
    title: &'a str,
    subtitle: &'a str,
    accessory: &'a str,
    glyph: &'a str,
    fill: &'a str,
    /// What Enter is called — "Use Keyword" for help rows, "Search Again"
    /// for recents, matching `Launcher.qml`'s two fill sites.
    verb: &'a str,
}

/// A row whose only business is putting text in the box.
fn fill_row(spec: FillSpec<'_>) -> Row {
    let mut row = Row::new(spec.key, spec.provider);
    row.group = spec.group.into();
    row.title = spec.title.into();
    row.subtitle = spec.subtitle.into();
    row.accessory = spec.accessory.into();
    row.icon_glyph = spec.glyph.into();
    row.fill = spec.fill.into();
    // A self-reference, so the footer names what Enter does and Ctrl+K on the
    // row leads back through activate.
    let mut act = Action {
        title: spec.verb.into(),
        shortcut: "↵".into(),
        ..Action::default()
    };
    act.extra.insert("row".into(), json!(spec.key));
    row.actions = Some(vec![act]);
    row
}

// ------------------------------------------------------------ builtin data

/// The four built-in scopes, as `Help.js` listed them.
fn builtin_help() -> &'static [(&'static str, &'static str, &'static [&'static str])] {
    &[
        ("calc", "Calculator", &["math"]),
        ("run", "Commands", &["command", "commands"]),
        ("apps", "Applications", &["app", "launch"]),
        ("web", "Web Search", &["search", "google", "ddg"]),
        ("h", "Keywords", &["help", "?", ":"]),
    ]
}

/// What the parser knows before the registry has spoken.
fn builtin_keywords() -> HashSet<String> {
    [
        "calc", "math", "run", "command", "commands", "web", "search", "google", "ddg", "apps",
        "app", "launch", "settings", "action", "h", "help",
        // Extra filters the built-ins read alongside their own keyword.
        "format", "in", "type",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// The built-ins, declared as extensions so one set of rules routes them —
/// including the worker's debounce/timeout/cache machinery.
///
/// `links` supplies the quicklink keywords: a link's own keyword addresses
/// it — `later:hi` scopes to `later` and the provider answers for that link,
/// the way `Quicklinks.js` treated `query.scope === link.keyword`. The
/// keywords ride on the extension as aliases, so one routing table covers
/// both the engine's `claims` and the worker's gate — and a quicklinks edit
/// is an `aliases` change, which the worker reconcile picks up.
fn builtin_extensions(links: &[crate::settings::Quicklink]) -> Vec<Extension> {
    let mut out = Vec::new();
    let synth = |id: &str,
                 title: &str,
                 keyword: &str,
                 aliases: &[&str],
                 native: &str,
                 always: bool,
                 min_chars: usize,
                 debounce: u64,
                 max_rows: usize,
                 tier: &str,
                 view: &str,
                 builtin: bool| {
        Extension {
            id: id.to_string(),
            title: title.to_string(),
            keyword: keyword.to_string(),
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            filters: vec![],
            search: String::new(),
            when: String::new(),
            glyph: String::new(),
            subtitle: title.to_string(),
            min_chars,
            debounce_ms: debounce,
            timeout_ms: 4000,
            max_rows,
            tier: rank::tier(tier),
            view: view.to_string(),
            always,
            cache_ms: 0,
            refresh_ms: 0,
            socket: String::new(),
            native: native.to_string(),
            actions: vec![],
            accent: String::new(),
            settings: vec![],
            test_query: String::new(),
            source: std::path::PathBuf::from("<builtin>"),
            builtin,
        }
    };
    out.push(synth(
        "calc",
        "Calculator",
        "calc",
        &["math"],
        "calc",
        true,
        1,
        90,
        4,
        "calc",
        "hero",
        true,
    ));
    // `min_chars: 0` makes the bare keyword a browse mode — `apps:` lists
    // apps alphabetically, `run:` lists every command — the way the script
    // build's scoped query functions behaved on an empty argument. The
    // providers still decline a truly empty query (`query.empty`), so the
    // unscoped empty box stays recents-only.
    out.push(synth(
        "apps",
        "Applications",
        "apps",
        &["app", "launch"],
        "apps",
        true,
        0,
        0,
        20,
        "substring",
        "list",
        true,
    ));
    // The script build capped commands and quicklinks only at the merge
    // limit — all 24 commands and every link are reachable.
    out.push(synth(
        "commands",
        "Commands",
        "run",
        &["commands"],
        "commands",
        true,
        0,
        0,
        60,
        "substring",
        "list",
        true,
    ));
    out.push(synth(
        "quicklinks",
        "Quicklinks",
        "quicklinks",
        &[],
        "quicklinks",
        true,
        0,
        0,
        60,
        "substring",
        "list",
        true,
    ));
    if let Some(ql) = out.iter_mut().find(|e| e.id == "quicklinks") {
        ql.aliases = links
            .iter()
            .map(|l| l.keyword.to_lowercase())
            .filter(|k| !k.is_empty())
            .collect();
    }
    out.push(synth(
        "web",
        "Web",
        "web",
        &["search", "google", "ddg"],
        "web",
        true,
        1,
        0,
        1,
        "web",
        "list",
        true,
    ));
    out
}

/// The built-in actions, as `Actions.js` declared them.
fn builtin_actions() -> Vec<Action> {
    let mk =
        |id: &str, title: &str, subtitle: &str, effect: &str, confirm: &str, keywords: &[&str]| {
            Action {
                id: id.to_string(),
                title: title.to_string(),
                subtitle: subtitle.to_string(),
                effect: effect.to_string(),
                confirm: confirm.to_string(),
                keywords: keywords.iter().map(|s| s.to_string()).collect(),
                ..Action::default()
            }
        };
    let mut out = vec![
        mk(
            "clear",
            "Clear Recent Queries",
            "History",
            "clear.recents",
            "",
            &["clear", "recent", "recents", "history", "forget"],
        ),
        mk(
            "clear-pins",
            "Clear Pinned Results",
            "History",
            "clear.pins",
            "",
            &["clear", "pins", "pinned", "unpin"],
        ),
        // Recents come back on their own; pins do not. One keypress should
        // not throw a pin away silently.
        mk(
            "clear-all",
            "Clear Everything",
            "History",
            "clear.all",
            "Clear recent queries and every pin?",
            &["clear", "all", "reset", "everything", "wipe"],
        ),
        mk(
            "reload",
            "Reload Extensions",
            "Launcher",
            "reload.extensions",
            "",
            &["reload", "refresh", "extensions", "rescan"],
        ),
        mk(
            "settings",
            "Extension Settings",
            "Launcher",
            "open.settings",
            "",
            &["settings", "config", "preferences", "options"],
        ),
        mk(
            "stats",
            "Launcher Stats",
            "Launcher",
            "stats",
            "",
            &["stats", "statistics", "diagnostics", "cache"],
        ),
    ];
    let mut logs = mk(
        "logs",
        "Open Event Log",
        "Launcher",
        "",
        "",
        &["log", "logs", "events", "diagnostics", "debug", "trace"],
    );
    logs.exec = "omarchy-launch-editor ~/.local/state/omarchy/oxy-log.jsonl".into();
    out.push(logs);
    let mut config = mk(
        "config",
        "Edit oxy.json",
        "Launcher",
        "",
        "",
        &["config", "json", "edit", "file"],
    );
    config.exec = "omarchy-launch-editor ~/.config/omarchy/oxy.json".into();
    out.push(config);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::known_keywords;
    use crate::settings::Quicklink;

    fn link(keyword: &str) -> Quicklink {
        Quicklink {
            title: format!("link {keyword}"),
            subtitle: String::new(),
            keyword: keyword.into(),
            tags: vec![],
            url: format!("https://{keyword}.example/{{}}"),
            open: String::new(),
            glyph: String::new(),
        }
    }

    /// N1: a quicklink answers under its own keyword — `later:hi` scopes to
    /// `later`, which the quicklinks extension claims through its aliases, so
    /// the worker is asked and `arg_for` hands it `hi`.
    #[test]
    fn quicklink_keyword_routes_to_provider() {
        let links = vec![link("later")];
        let exts = builtin_extensions(&links);
        let ql = exts.iter().find(|e| e.id == "quicklinks").unwrap();
        assert!(
            ql.aliases.iter().any(|a| a == "later"),
            "the link's keyword must ride the extension's aliases"
        );

        let known = known_keywords(&exts, &builtin_keywords());
        let q = Query::parse("later:hi", 1, Some(&known));
        assert_eq!(q.scope, "later");
        assert!(q.routes_to(&ql.keyword, &ql.aliases));
        assert_eq!(q.arg_for(&ql.keyword, &ql.aliases), "hi");
    }

    /// The engine's half of N1: `claims` consults the same aliases, so the
    /// dispatcher and the worker gate can never disagree about a link scope.
    #[test]
    fn quicklink_alias_survives_reload_and_disable() {
        // Editing quicklinks changes the synthetic extension's aliases, which
        // def_stamp fingerprints — a reload must see it as a redefinition.
        let before = builtin_extensions(&[link("later")]);
        let after = builtin_extensions(&[link("other")]);
        let ql_before = before.iter().find(|e| e.id == "quicklinks").unwrap();
        let ql_after = after.iter().find(|e| e.id == "quicklinks").unwrap();
        assert_ne!(
            crate::registry::def_stamp(ql_before),
            crate::registry::def_stamp(ql_after),
            "a quicklinks edit must read as a changed definition"
        );
    }

    /// Third-audit nit: `later:`'s chip names the link ("Added Later"), not
    /// the transport ("Quicklinks") — the synthesized extension's aliases are
    /// link keywords, so only its own keyword may claim the label.
    #[test]
    fn scope_label_names_the_link_not_quicklinks() {
        let mut engine = bare_engine();
        engine.extensions = Arc::new(builtin_extensions(&[link("later")]));
        engine.settings.quicklinks = vec![link("later")];
        let known = known_keywords(&engine.extensions, &builtin_keywords());
        let q = Query::parse("later:hi", 1, Some(&known));
        assert_eq!(engine.scope_label(&q), "link later");
        let q = Query::parse("quicklinks:x", 1, Some(&known));
        assert_eq!(engine.scope_label(&q), "Quicklinks");
    }

    /// A minimal engine for the in-process flows — no workers, and state
    /// paths under the temp dir so `save_state`'s writes land nowhere real.
    fn bare_engine() -> Engine {
        let scratch = std::env::temp_dir().join(format!("oxy-core-test-{}", std::process::id()));
        let (_cmd_tx, cmd_rx) = mpsc::channel(8);
        let (evt_tx, _evt_rx) = mpsc::channel(64);
        let (worker_tx, _worker_rx) = mpsc::channel(64);
        let shared = Arc::new(Shared {
            cache: std::sync::Mutex::new(crate::support::cache::Cache::default()),
            availability: std::sync::Mutex::new(
                crate::support::availability::Availability::default(),
            ),
            settings: RwLock::new(Arc::new(Settings::default())),
            registry: RwLock::new(Arc::new(Vec::new())),
            hello_keywords: std::sync::RwLock::new(Arc::new(Vec::new())),
        });
        let mut engine = Engine {
            cmd_rx,
            evt_tx,
            shared,
            settings: Settings::default(),
            extensions: Arc::new(builtin_extensions(&[])),
            workers: HashMap::new(),
            natives: HashMap::new(),
            native_for: Box::new(|_| None),
            worker_tx,
            state: crate::state::State {
                frecency: HashMap::new(),
                recents: Vec::new(),
                pins: HashMap::new(),
            },
            state_path: scratch.join("oxy-state.json"),
            frecency_path: scratch.join("oxy-frecency.json"),
            epoch: 0,
            raw: String::new(),
            query: Arc::new(Query::parse("", 0, None)),
            opened: true,
            opened_at: None,
            showing: HashSet::new(),
            buckets: HashMap::new(),
            worker_defs: HashMap::new(),
            waiting: HashSet::new(),
            rows: Vec::new(),
            pending_activate: None,
            pending_confirm: None,
            previewed: None,
            preview_revert: String::new(),
            clipboard_url: None,
            ask_task: None,
            ask_provider: None,
            ask_probed: true,
            known: HashSet::new(),
        };
        engine.rebuild_known();
        engine
    }

    /// N13: the first Enter on `/clear-all` arms the confirm, and the engine's
    /// own re-ask of `/clear-all ` must not clear it — that re-ask used to
    /// run the old text through the walking-away rule and the arm died one
    /// line after it was set.
    #[tokio::test]
    async fn confirmed_action_arms_and_confirms() {
        let mut engine = bare_engine();
        let action = builtin_actions()
            .into_iter()
            .find(|a| a.id == "clear-all")
            .unwrap();
        let row = Row::new("action:clear-all", "actions");

        engine.on_query("/clear").await;
        engine.run_action(&row, &action).await;
        assert_eq!(
            engine.pending_confirm.as_deref(),
            Some("clear-all"),
            "the arm must survive the engine re-asking the confirm text"
        );
        assert_eq!(engine.raw, "/clear-all ");

        // The second Enter is the answer: the action runs and disarms.
        engine.run_action(&row, &action).await;
        assert!(engine.pending_confirm.is_none());
    }

    /// N16: a preview chain reverts to the state you arrived in — the first
    /// row's `revertExec`, not the last previewed row's. Moving between
    /// previews runs nothing but the next preview.
    #[tokio::test]
    async fn preview_revert_is_the_first_rows() {
        let mut engine = bare_engine();
        let mut a = Row::new("a", "probe");
        a.preview_exec = "test-nop preview-a".into();
        a.revert_exec = "test-nop revert-a".into();
        let mut b = Row::new("b", "probe");
        b.preview_exec = "test-nop preview-b".into();
        b.revert_exec = "test-nop revert-b".into();
        engine.rows = vec![Arc::new(a), Arc::new(b)];

        engine.on_select("a");
        assert_eq!(engine.preview_revert, "test-nop revert-a");
        engine.on_select("b");
        assert_eq!(
            engine.preview_revert, "test-nop revert-a",
            "the revert to run is the first previewed row's — the state you \
             arrived in, not the one you last previewed"
        );
        engine.unpreview();
        assert!(engine.preview_revert.is_empty());
        assert!(engine.previewed.is_none());
    }

    /// N2's fingerprint: any field a gate or a row build reads flips it —
    /// spot-check the ones the audit measured (search, when, keyword).
    #[test]
    fn def_stamp_covers_gate_fields() {
        let exts = builtin_extensions(&[]);
        let base = exts.iter().find(|e| e.id == "commands").unwrap();
        let mut changed = base.clone();
        changed.search = "echo different".into();
        assert_ne!(
            crate::registry::def_stamp(base),
            crate::registry::def_stamp(&changed)
        );
        let mut changed = base.clone();
        changed.when = "command -v definitely-absent-thing".into();
        assert_ne!(
            crate::registry::def_stamp(base),
            crate::registry::def_stamp(&changed)
        );
        let mut changed = base.clone();
        changed.debounce_ms += 1;
        assert_ne!(
            crate::registry::def_stamp(base),
            crate::registry::def_stamp(&changed)
        );
    }
}
