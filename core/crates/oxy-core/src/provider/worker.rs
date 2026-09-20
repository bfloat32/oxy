//! One task per extension: the port of `ExtensionProvider.qml`.
//!
//! The state machine is the same shape as the QML one — a debounce between the
//! keystroke and the run it arms, a timeout on the run, an epoch on every
//! answer so a slow one lands nowhere, and a cache keyed by the exact question
//! asked.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tokio::time::{Instant, Sleep};

use crate::extension::{Extension, build_command, cache_key};
use crate::provider::process;
use crate::provider::socket::{self, SocketChan, SocketReq};
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::query::Query;
use crate::row::{Row, to_row};

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
}

/// What a worker tells the engine.
pub enum WorkerMsg {
    /// About to work on this epoch — the spinner's reason.
    Waiting { id: String, epoch: u64 },
    /// An answer for this epoch (`last` clears the wait). Empty is an answer.
    Rows {
        id: String,
        epoch: u64,
        rows: Vec<Row>,
        last: bool,
    },
    /// Log lines, relayed to the event stream.
    Log {
        id: String,
        ev: String,
        fields: Value,
    },
}

/// What the registry-wide `Shared` holds — the parts of one launcher that
/// outlive any single provider.
pub struct Shared {
    pub cache: std::sync::Mutex<crate::cache::Cache>,
    pub availability: std::sync::Mutex<crate::availability::Availability>,
    pub settings: tokio::sync::RwLock<crate::settings::Settings>,
    pub registry: tokio::sync::RwLock<Arc<Vec<Extension>>>,
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
pub async fn run(
    ext: Extension,
    native: Option<Box<dyn NativeExt>>,
    self_tx: mpsc::UnboundedSender<WorkerCmd>,
    mut rx: mpsc::UnboundedReceiver<WorkerCmd>,
    tx: mpsc::Sender<WorkerMsg>,
    shared: Arc<Shared>,
) {
    let id = ext.id.clone();
    let native = native.map(|n| Arc::new(Mutex::new(n)));

    // ---- internal state -------------------------------------------------
    let mut available = ext.when.is_empty();
    let mut recheck: Option<Arc<Query>> = None;
    let mut opened = true;
    let mut showing = false;
    let mut current_epoch: u64 = 0;
    let mut pending: Option<Pending> = None;
    let mut live: Option<Live> = None;
    let mut stale_shown_key = String::new();
    let mut debounce: Option<std::pin::Pin<Box<Sleep>>> = None;
    let mut refresh_at: Option<std::pin::Pin<Box<Sleep>>> = None;
    let mut proc_run: Option<JoinHandle<Option<String>>> = None;
    let mut native_run: Option<JoinHandle<NativeOutcome>> = None;
    let mut native_partial: Option<mpsc::UnboundedReceiver<Vec<Value>>> = None;
    let mut run_epoch: u64 = 0;
    let mut run_key = String::new();
    let mut run_pending: Option<Pending> = None;
    let mut refreshing_run = false;
    // The socket connection is an actor of its own, so a run can borrow it
    // without borrowing the worker — and every line the daemon pushes lands
    // on the push channel, not only the one answering the last question.
    let mut socket: Option<SocketChan> = None;
    // The question the socket daemon was last asked, kept after its first
    // answer: pushes for the same epoch still have a question to belong to.
    let mut sock_pending: Option<Pending> = None;
    // A socket question owed its first answer; the deadline is the old
    // `killer` timer — a daemon that never speaks resolves as a failed run.
    let mut sock_waiting = false;
    let mut sock_deadline: Option<std::pin::Pin<Box<Sleep>>> = None;
    let mut last_connect = Instant::now() - Duration::from_secs(60);

    // `when` is checked once at spawn; a failure re-checks only when the
    // keyword is actually typed, at most every fifteen seconds.
    if !available {
        if let Some(ok) = shared.availability.lock().unwrap().get(&ext.when) {
            available = ok;
        } else {
            let when = ext.when.clone();
            let shared2 = shared.clone();
            let self2 = self_tx.clone();
            tokio::spawn(async move {
                let ok = process::check(&when).await;
                shared2.availability.lock().unwrap().put(&when, ok);
                let _ = self2.send(WorkerCmd::Available { ok, replay: None });
            });
        }
    }

    macro_rules! emit {
        ($ep:expr, $rows:expr, $last:expr) => {{
            let _ = tx
                .send(WorkerMsg::Rows {
                    id: id.clone(),
                    epoch: $ep,
                    rows: $rows,
                    last: $last,
                })
                .await;
        }};
    }
    macro_rules! plog {
        ($ev:expr, $($f:tt)*) => {{
            let _ = tx.send(WorkerMsg::Log {
                id: id.clone(),
                ev: $ev.into(),
                fields: serde_json::json!($($f)*),
            }).await;
        }};
    }

    // Every run's exit path: the cache write, the stale rule, the refresh
    // arming and the emit all live here so no caller can forget one.
    macro_rules! deliver {
        ($out:expr) => {{
            let out: RunOut = $out;
            let was_refresh = refreshing_run;
            refreshing_run = false;
            let failed = matches!(out, RunOut::Failed)
                || matches!(&out, RunOut::Raw(r) if r.is_empty())
                || matches!(&out, RunOut::Built(r) if r.is_empty());
            // An empty answer over rows that were already on screen is nearly
            // always a timeout, not the answer becoming "no rows".
            let keep_stale = failed
                && (was_refresh
                    || (!stale_shown_key.is_empty() && stale_shown_key == run_key));
            if keep_stale {
                run_pending = None;
                plog!("prov.stale", {"ep": run_epoch, "refresh": was_refresh});
                arm_refresh!();
            } else {
                match out {
                    RunOut::Failed => {
                        run_pending = None;
                        emit!(run_epoch, Vec::new(), true)
                    }
                    RunOut::Raw(mut raw) => {
                        raw.truncate(ext.max_rows);
                        if let Some(p) = run_pending.take() {
                            shared
                                .cache
                                .lock()
                                .unwrap()
                                .put(&id, &p.key, raw.clone(), ext.cache_ms);
                            live = Some(Live { epoch: p.epoch, pending: p });
                        }
                        stale_shown_key = String::new();
                        emit!(run_epoch, build_rows(&ext, &raw), true);
                    }
                    RunOut::Built(rows) => {
                        if let Some(p) = run_pending.take() {
                            live = Some(Live { epoch: p.epoch, pending: p });
                        }
                        stale_shown_key = String::new();
                        emit!(
                            run_epoch,
                            rows.into_iter().take(ext.max_rows).collect::<Vec<_>>(),
                            true
                        );
                    }
                }
                arm_refresh!();
            }
        }};
    }

    macro_rules! arm_refresh {
        () => {{
            refresh_at = if ext.refresh_ms > 0
                && opened
                && showing
                && live.as_ref().is_some_and(|l| l.epoch == current_epoch)
            {
                Some(Box::pin(tokio::time::sleep_until(
                    Instant::now() + Duration::from_millis(ext.refresh_ms),
                )))
            } else {
                None
            };
        }};
    }

    // Stopping a run is two things: aborting the in-flight task, and
    // forgetting what it was for. The debounce path does only the first —
    // `ask!` writes a new `run_pending` immediately after.
    macro_rules! abort_run {
        () => {{
            if let Some(h) = proc_run.take() {
                h.abort();
            }
            if let Some(h) = native_run.take() {
                h.abort();
            }
            native_partial = None;
            // A socket ask has no task to abort: the line already went out,
            // and its answer lands on the push channel — epoch-filtered on
            // arrival, so a late one lands nowhere.
            sock_deadline = None;
            sock_waiting = false;
            sock_pending = None;
        }};
    }
    macro_rules! cancel_run {
        () => {{
            abort_run!();
            run_pending = None;
            refreshing_run = false;
        }};
    }

    // The question, by whichever route is up: native first, then a connected
    // socket, then the command — a daemon that died degrades to the slow path
    // rather than going silent.
    macro_rules! ask {
        ($p:expr, $refresh:expr) => {{
            let p: Pending = $p;
            run_epoch = p.epoch;
            run_key = p.key.clone();
            refreshing_run = $refresh;
            run_pending = Some(p.clone());
            if let Some(n) = native.clone() {
                let (ptx, prx) = mpsc::unbounded_channel();
                native_partial = Some(prx);
                let ctx = Ctx {
                    query: p.query.clone(),
                    arg: p.arg.clone(),
                    filters: p.filters.clone(),
                    settings: Arc::new(shared.settings.read().await.clone()),
                    registry: shared.registry.read().await.clone(),
                };
                native_run = Some(tokio::spawn(
                    async move { n.lock().await.query(ctx, ptx).await },
                ));
            } else if socket.is_some() {
                let sent = socket
                    .as_mut()
                    .unwrap()
                    .req
                    .send(SocketReq {
                        epoch: p.epoch,
                        arg: p.arg.clone(),
                        filters: p.filters.clone(),
                    })
                    .is_ok();
                if sent {
                    sock_pending = Some(p.clone());
                    sock_waiting = true;
                    sock_deadline = Some(Box::pin(tokio::time::sleep_until(
                        Instant::now() + Duration::from_millis(ext.timeout_ms),
                    )));
                    run_pending = Some(p);
                } else {
                    // The daemon's end of the socket is gone: drop the route
                    // and fall through to the command, the way a dead socket
                    // always degraded to the slow path.
                    socket = None;
                    sock_pending = None;
                    if !p.command.is_empty() {
                        let cmd = p.command.clone();
                        let tmo = Duration::from_millis(ext.timeout_ms);
                        proc_run = Some(tokio::spawn(async move { process::run(&cmd, tmo).await }));
                        run_pending = Some(p);
                    } else {
                        if stale_shown_key.is_empty() || stale_shown_key != p.key {
                            emit!(p.epoch, Vec::new(), true);
                        }
                        run_pending = None;
                    }
                }
            } else if !p.command.is_empty() {
                let cmd = p.command.clone();
                let tmo = Duration::from_millis(ext.timeout_ms);
                proc_run = Some(tokio::spawn(async move { process::run(&cmd, tmo).await }));
            } else {
                // A socket-only extension whose daemon is not listening answers
                // nothing rather than leaving the spinner up — unless stale
                // rows are already up, which are the better answer.
                if stale_shown_key.is_empty() || stale_shown_key != p.key {
                    emit!(p.epoch, Vec::new(), true);
                }
                run_pending = None;
            }
        }};
    }

    // The Ask handler as a macro so the availability replay runs the same
    // code — `handle_ask!(query)`.
    macro_rules! handle_ask {
        ($q:expr) => {{
            let q: Arc<Query> = $q;
            current_epoch = q.epoch;
            refreshing_run = false;
            refresh_at = None;
            stale_shown_key = String::new();

            // `when` guards the command and socket legs — the things that
            // need a program on the box. A native provider is its own answer:
            // it runs regardless and declines through Fallback, at which
            // point the check matters again.
            if !available && native.is_none() {
                let due = shared.availability.lock().unwrap().get(&ext.when).is_none();
                if !ext.when.is_empty() && q.routes_to(&ext.keyword, &ext.aliases) && due {
                    recheck = Some(q.clone());
                    let when = ext.when.clone();
                    let shared2 = shared.clone();
                    let self2 = self_tx.clone();
                    tokio::spawn(async move {
                        let ok = process::check(&when).await;
                        shared2.availability.lock().unwrap().put(&when, ok);
                        let _ = self2.send(WorkerCmd::Available { ok, replay: None });
                    });
                }
                emit!(q.epoch, Vec::new(), true);
            } else if q.scope.is_empty() && !ext.always {
                // Unscoped and not opted in: stay quiet.
                emit!(q.epoch, Vec::new(), true);
            } else if !q.routes_to(&ext.keyword, &ext.aliases) {
                emit!(q.epoch, Vec::new(), true);
            } else {
                let arg = q.arg_for(&ext.keyword, &ext.aliases);
                if arg.chars().count() < ext.min_chars {
                    emit!(q.epoch, Vec::new(), true);
                } else {
                    let filters = Arc::new(q.extras(&ext.keyword, &ext.aliases));
                    let command = if ext.search.is_empty() {
                        String::new()
                    } else {
                        let settings = shared.settings.read().await;
                        build_command(&ext, &arg, &filters, settings.settings_for(&ext.id))
                    };
                    let key = cache_key(&ext, &command, &arg, &filters);
                    let p = Pending {
                        epoch: q.epoch,
                        arg,
                        filters,
                        command,
                        key,
                        query: q.clone(),
                    };

                    let mut answered = false;
                    if ext.cache_ms > 0 {
                        // The guards are bound to their own statements so
                        // they die before any `.await` below — a MutexGuard
                        // is not Send and holding one across the emit would
                        // poison the whole task's future.
                        let fresh = shared.cache.lock().unwrap().get(&id, &p.key);
                        if let Some(hit) = fresh {
                            let rows = build_rows(&ext, &hit);
                            live = Some(Live {
                                epoch: q.epoch,
                                pending: p.clone(),
                            });
                            emit!(q.epoch, rows, true);
                            arm_refresh!();
                            answered = true;
                        }
                        if !answered {
                            let stale = shared.cache.lock().unwrap().get_stale(&id, &p.key);
                            if let Some(stale) = stale {
                                stale_shown_key = p.key.clone();
                                emit!(q.epoch, build_rows(&ext, &stale), false);
                            }
                        }
                    }

                    if !answered {
                        // Connecting is driven by queries, not a retry timer.
                        if !ext.socket.is_empty()
                            && socket.is_none()
                            && last_connect.elapsed() > Duration::from_millis(3000)
                        {
                            last_connect = Instant::now();
                            if let Ok(chan) = socket::connect(
                                &ext.socket,
                                Duration::from_millis(2000)
                                    .min(Duration::from_millis(ext.timeout_ms.max(1))),
                            )
                            .await
                            {
                                socket = Some(chan);
                            }
                        }
                        let _ = tx
                            .send(WorkerMsg::Waiting {
                                id: id.clone(),
                                epoch: q.epoch,
                            })
                            .await;
                        pending = Some(p);
                        debounce = Some(Box::pin(tokio::time::sleep_until(
                            Instant::now() + Duration::from_millis(ext.debounce_ms),
                        )));
                    }
                }
            }
        }};
    }

    loop {
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { return };
                match cmd {
                    WorkerCmd::Ask(q) => handle_ask!(q),
                    WorkerCmd::Available { ok, replay } => {
                        available = ok;
                        if ok {
                            let q = replay.or_else(|| recheck.take());
                            if let Some(q) = q
                                && q.epoch == current_epoch {
                                    handle_ask!(q);
                                }
                        }
                    }
                    WorkerCmd::Opened(v) => {
                        opened = v;
                        if !v {
                            pending = None;
                            live = None;
                            refresh_at = None;
                            debounce = None;
                            cancel_run!();
                        }
                    }
                    WorkerCmd::Showing(v) => {
                        showing = v;
                        if v { arm_refresh!(); } else { refresh_at = None; }
                    }
                    WorkerCmd::Cancel => {
                        pending = None;
                        live = None;
                        refresh_at = None;
                        debounce = None;
                        cancel_run!();
                    }
                }
            }

            _ = async { debounce.as_mut().unwrap().await }, if debounce.is_some() => {
                debounce = None;
                if let Some(p) = pending.take() {
                    // A run already in flight is told to stop; the newer
                    // question takes the slot.
                    abort_run!();
                    ask!(p, false);
                }
            }

            out = async { proc_run.as_mut().unwrap().await }, if proc_run.is_some() => {
                proc_run = None;
                let out = match out.ok().flatten() {
                    Some(t) => RunOut::Raw(crate::row::parse_rows(&t)),
                    None => RunOut::Failed,
                };
                if run_epoch == current_epoch {
                    deliver!(out);
                } else {
                    plog!("prov.drop", {"at": "finish", "ep": run_epoch});
                }
            }

            push = async { socket.as_mut().unwrap().push.recv().await },
                if socket.is_some() =>
            {
                match push {
                    // Every line the daemon sends for the live epoch is a
                    // complete answer — an answer, or a later refinement of
                    // one. `do:` streams its card this way.
                    Some(p) if opened && p.epoch == current_epoch => {
                        sock_deadline = None;
                        sock_waiting = false;
                        if run_pending.is_none() {
                            run_pending = sock_pending
                                .clone()
                                .filter(|q| q.epoch == p.epoch);
                        }
                        run_epoch = p.epoch;
                        run_key = sock_pending
                            .as_ref()
                            .filter(|q| q.epoch == p.epoch)
                            .map(|q| q.key.clone())
                            .unwrap_or_default();
                        deliver!(RunOut::Raw(p.rows));
                    }
                    Some(_) => {}
                    // The daemon hung up. The deadline, if a question is
                    // owed, still resolves it; the next ask reconnects.
                    None => socket = None,
                }
            }

            _ = async { sock_deadline.as_mut().unwrap().await },
                if sock_deadline.is_some() =>
            {
                sock_deadline = None;
                if sock_waiting {
                    sock_waiting = false;
                    // The old killer: a refresh keeps its rows, stale keeps
                    // stale, anything else is an empty answer.
                    deliver!(RunOut::Failed);
                }
            }

            out = async { native_run.as_mut().unwrap().await }, if native_run.is_some() => {
                native_run = None;
                // Flush anything the provider pushed on its way out.
                if let Some(mut prx) = native_partial.take() {
                    while let Ok(raw) = prx.try_recv() {
                        if run_epoch == current_epoch {
                            emit!(run_epoch, build_rows(&ext, &raw), false);
                        }
                    }
                }
                match out {
                    Ok(NativeOutcome::Rows(raw)) => {
                        if run_epoch == current_epoch { deliver!(RunOut::Raw(raw)); }
                    }
                    Ok(NativeOutcome::Built(rows)) => {
                        if run_epoch == current_epoch { deliver!(RunOut::Built(rows)); }
                    }
                    Ok(NativeOutcome::Empty) => {
                        if run_epoch == current_epoch { deliver!(RunOut::Raw(Vec::new())); }
                    }
                    Ok(NativeOutcome::Fallback) => {
                        // The native provider declined: the declared socket or
                        // command answers instead — unless `when` said the box
                        // lacks what they need.
                        if let Some(p) = run_pending.take() {
                            if !available {
                                if run_epoch == current_epoch {
                                    deliver!(RunOut::Raw(Vec::new()));
                                }
                            } else if !p.command.is_empty() {
                                let cmd = p.command.clone();
                                let tmo = Duration::from_millis(ext.timeout_ms);
                                proc_run = Some(tokio::spawn(async move {
                                    process::run(&cmd, tmo).await
                                }));
                                run_pending = Some(p);
                            } else if socket.is_some() {
                                let sent = socket
                                    .as_mut()
                                    .unwrap()
                                    .req
                                    .send(SocketReq {
                                        epoch: p.epoch,
                                        arg: p.arg.clone(),
                                        filters: p.filters.clone(),
                                    })
                                    .is_ok();
                                if sent {
                                    sock_pending = Some(p.clone());
                                    sock_waiting = true;
                                    sock_deadline =
                                        Some(Box::pin(tokio::time::sleep_until(
                                            Instant::now()
                                                + Duration::from_millis(ext.timeout_ms),
                                        )));
                                    run_pending = Some(p);
                                } else {
                                    socket = None;
                                    sock_pending = None;
                                    if run_epoch == current_epoch {
                                        deliver!(RunOut::Raw(Vec::new()));
                                    }
                                }
                            } else if run_epoch == current_epoch {
                                deliver!(RunOut::Raw(Vec::new()));
                            }
                        }
                    }
                    Err(_) => {
                        if run_epoch == current_epoch { deliver!(RunOut::Failed); }
                    }
                }
            }

            raw = async { native_partial.as_mut().unwrap().recv().await },
                if native_partial.is_some() =>
            {
                match raw {
                    Some(raw) if run_epoch == current_epoch => {
                        emit!(run_epoch, build_rows(&ext, &raw), false);
                    }
                    Some(_) => {}
                    None => native_partial = None,
                }
            }

            _ = async { refresh_at.as_mut().unwrap().await }, if refresh_at.is_some() => {
                refresh_at = None;
                // A real query queued or running re-arms this when it lands;
                // two runs for one extension is the pile-up the debounce
                // exists to prevent.
                if opened
                    && showing
                    && pending.is_none()
                    && proc_run.is_none()
                    && native_run.is_none()
                    && !sock_waiting
                    && live.as_ref().is_some_and(|l| l.epoch == current_epoch)
                {
                    let p = live.take().map(|l| l.pending).unwrap();
                    ask!(p, true);
                }
            }
        }
    }
}

/// Build launchable rows out of raw provider output — the `build()` port.
fn build_rows(ext: &Extension, raw: &[Value]) -> Vec<Row> {
    let mut rows = Vec::new();
    for (i, v) in raw.iter().enumerate().take(ext.max_rows) {
        if let Some(mut row) = to_row(ext, v, i) {
            row.score = crate::rank::score(row.tier, row.local, 0);
            rows.push(row);
        }
    }
    rows
}
