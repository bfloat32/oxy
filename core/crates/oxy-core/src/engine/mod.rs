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

mod activate;
mod ask;
mod builtins;
mod inline;
mod persist;
mod pipeline;
mod workers;

#[cfg(test)]
mod tests;

pub use crate::model::event::{EngineCmd, EngineEvent};

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use serde_json::json;
use tokio::sync::{RwLock, mpsc};

use builtins::{builtin_extensions, builtin_keywords};

use crate::model::query::Query;
use crate::model::row::Row;
use crate::provider::NativeExt;
use crate::provider::worker::{Shared, WorkerCmd, WorkerMsg};
use crate::registry::{Extension, known_keywords};
use crate::settings::Settings;
use crate::state::State;

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
    /// The configured local model endpoint, resolved with the settings. When
    /// it is set, the CLI list is not probed and not used.
    llm: Option<crate::provider::llm::Local>,
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

        let (state, state_moved) = State::load(
            &crate::settings::paths::frecency_file(),
            &crate::settings::paths::state_file(),
        );
        // A state file that did not parse was moved aside rather than read as
        // empty; say so, or the loss of pins and recents is invisible.
        for path in state_moved {
            let _ = evt_tx
                .send(EngineEvent::Log {
                    ev: "state.recovered".into(),
                    fields: json!({ "f": path.to_string_lossy() }),
                })
                .await;
        }
        for path in &settings.recovered {
            let _ = evt_tx
                .send(EngineEvent::Log {
                    ev: "settings.recovered".into(),
                    fields: json!({ "f": path.to_string_lossy() }),
                })
                .await;
        }

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
            llm: None,
            ask_probed: false,
            known: HashSet::new(),
        };
        engine.rebuild_known();
        engine.spawn_workers(&extensions).await;
        engine
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

    /// The clipboard read, once per open. The daemon hands the engine a
    /// oneshot this fills.
    pub fn set_clipboard_url(&mut self, url: Option<String>) {
        self.clipboard_url = url;
    }
}
