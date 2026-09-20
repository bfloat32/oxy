//! One task per extension: the port of `ExtensionProvider.qml`.
//!
//! The state machine is the same shape as the QML one — a debounce between the
//! keystroke and the run it arms, a timeout on the run, an epoch on every
//! answer so a slow one lands nowhere, and a cache keyed by the exact question
//! asked.
//!
//! The files: `mod.rs` keeps the wire types and the `select!` loop; `state`
//! is the `WorkerState` the loop drives — what used to be `run`'s locals and
//! macros; `route` is the ask path (native → socket → command), the cache
//! reads, and the socket push handling.

mod route;
mod state;

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};

use serde_json::Value;
use tokio::sync::mpsc;

use crate::model::query::Query;
use crate::model::row::{Row, SharedRows, to_row_owned};
use crate::provider::NativeExt;
use crate::registry::Extension;

use state::WorkerState;

/// The empty answer every quiet provider shares — `Vec::new` allocs nothing,
/// so the Arc around it is the only allocation this saves.
static EMPTY_ROWS: LazyLock<SharedRows> = LazyLock::new(|| Arc::new(Vec::new()));

/// What the engine asks of a worker.
pub enum WorkerCmd {
    /// A new query. The worker decides whether it claims it.
    Ask(Arc<Query>),
    /// The launcher closed: stop the refresh and any run; keep the cache.
    Cancel,
    /// Whether this provider's rows are on screen — refresh only re-asks a
    /// question whose answer is showing.
    Showing(bool),
    /// The launcher is open (refresh and availability replays gate on it).
    Opened(bool),
    /// A `when` check resolved; the query that triggered it is owed a replay.
    Available {
        ok: bool,
        replay: Option<Arc<Query>>,
    },
    /// The extension changed or left on reload: the task exits, and a fresh
    /// worker built from the new definition takes the next ask.
    Shutdown,
}

/// What a worker tells the engine.
pub enum WorkerMsg {
    /// About to work on this epoch — the spinner's reason.
    Waiting { id: Arc<str>, epoch: u64 },
    /// An answer for this epoch (`last` clears the wait). Empty is an answer.
    /// Arc'd end to end: a cache hit is one refcount bump to the engine, and
    /// the engine's bucket stores the same set the worker built.
    Rows {
        id: Arc<str>,
        epoch: u64,
        rows: SharedRows,
        last: bool,
    },
    /// Log lines, relayed to the event stream.
    Log {
        id: Arc<str>,
        ev: String,
        fields: Value,
    },
}

/// What the registry-wide `Shared` holds — the parts of one launcher that
/// outlive any single provider.
pub struct Shared {
    pub cache: std::sync::Mutex<crate::support::cache::Cache>,
    pub availability: std::sync::Mutex<crate::support::availability::Availability>,
    /// Arc'd so a read is a refcount bump — 30+ workers clone it per
    /// keystroke, and a deep `Settings` clone each time is not cheap.
    pub settings: tokio::sync::RwLock<Arc<crate::settings::Settings>>,
    pub registry: tokio::sync::RwLock<Arc<Vec<Extension>>>,
    /// The parser's current keyword set, sorted — what `hello` reports to a
    /// client that connects after a reload. A std lock: the daemon's accept
    /// loop reads it without an await.
    pub hello_keywords: std::sync::RwLock<Arc<Vec<String>>>,
}

/// A run's output, whichever route produced it.
enum RunOut {
    /// Raw row objects, run through `to_row`.
    Raw(Vec<Value>),
    /// Rows a native provider built itself.
    Built(Vec<Row>),
    /// Timed out or failed — reads as an empty answer, and over stale rows it
    /// reads as "no answer at all" so the stale ones stay.
    Failed,
}

/// Everything a run needs to know to ask its question again.
#[derive(Clone)]
struct Pending {
    epoch: u64,
    arg: String,
    filters: Arc<BTreeMap<String, String>>,
    command: String,
    key: String,
    query: Arc<Query>,
}

/// The question whose answer is on screen right now; refresh re-asks it.
struct Live {
    epoch: u64,
    pending: Pending,
}

/// One task per extension. `run` owns the loop; `Worker` is the handle the
/// engine keeps.
pub struct Worker;

/// The whole state machine, as a task. Rebuilt on reload.
///
/// `run` owns the command channel and the `select!` — every arm delegates to
/// a `WorkerState` method, so the epochs, timers and routes live in `state`.
pub async fn run(
    ext: Extension,
    native: Option<Box<dyn NativeExt>>,
    self_tx: mpsc::UnboundedSender<WorkerCmd>,
    mut rx: mpsc::UnboundedReceiver<WorkerCmd>,
    tx: mpsc::Sender<WorkerMsg>,
    shared: Arc<Shared>,
) {
    let mut st = WorkerState::new(ext, native, self_tx, tx, shared);

    loop {
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { return };
                match cmd {
                    WorkerCmd::Ask(q) => st.handle_ask(q).await,
                    WorkerCmd::Available { ok, replay } => st.on_available(ok, replay).await,
                    WorkerCmd::Opened(v) => st.on_opened(v),
                    WorkerCmd::Showing(v) => st.on_showing(v),
                    WorkerCmd::Cancel => st.cancel(),
                    WorkerCmd::Shutdown => return,
                }
            }

            _ = async { st.debounce.as_mut().unwrap().await }, if st.debounce.is_some() => {
                st.on_debounce().await;
            }

            out = async { st.proc_run.as_mut().unwrap().await }, if st.proc_run.is_some() => {
                st.on_proc_done(out).await;
            }

            push = async { st.socket.as_mut().unwrap().push.recv().await },
                if st.socket.is_some() =>
            {
                st.on_push(push).await;
            }

            _ = async { st.sock_deadline.as_mut().unwrap().await },
                if st.sock_deadline.is_some() =>
            {
                st.on_sock_deadline().await;
            }

            out = async { st.native_run.as_mut().unwrap().await }, if st.native_run.is_some() => {
                st.on_native_done(out).await;
            }

            raw = async { st.native_partial.as_mut().unwrap().recv().await },
                if st.native_partial.is_some() =>
            {
                st.on_native_partial(raw).await;
            }

            _ = async { st.refresh_at.as_mut().unwrap().await }, if st.refresh_at.is_some() => {
                st.on_refresh().await;
            }
        }
    }
}

/// Wrap a built row set for the wire: one `Arc` per row for the engine's
/// bucket, one over the vec so a cache hit or re-emit is a refcount bump.
fn share_rows(rows: Vec<Row>) -> SharedRows {
    Arc::new(rows.into_iter().map(Arc::new).collect())
}

/// Consuming form for the fresh-run path — each raw row is taken apart in
/// place, so no field is cloned on its way into the launcher row.
fn build_rows_owned(ext: &Extension, raw: Vec<Value>) -> Vec<Row> {
    let mut rows = Vec::with_capacity(raw.len().min(ext.max_rows));
    for (i, v) in raw.into_iter().enumerate().take(ext.max_rows) {
        if let Some(mut row) = to_row_owned(ext, v, i) {
            row.score = crate::support::rank::score(row.tier, row.local, 0);
            rows.push(row);
        }
    }
    rows
}
