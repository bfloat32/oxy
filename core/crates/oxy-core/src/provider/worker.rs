//! One task per extension: the port of `ExtensionProvider.qml`.
//!
//! The state machine is the same shape as the QML one — a debounce between the
//! keystroke and the run it arms, a timeout on the run, an epoch on every
//! answer so a slow one lands nowhere, and a cache keyed by the exact question
//! asked.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};
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
use crate::row::{Row, SharedRows, to_row_owned};

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
    pub cache: std::sync::Mutex<crate::cache::Cache>,
    pub availability: std::sync::Mutex<crate::availability::Availability>,
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
pub async fn run(
    ext: Extension,
    native: Option<Box<dyn NativeExt>>,
    self_tx: mpsc::UnboundedSender<WorkerCmd>,
    mut rx: mpsc::UnboundedReceiver<WorkerCmd>,
    tx: mpsc::Sender<WorkerMsg>,
    shared: Arc<Shared>,
) {
    // `Arc<str>`: every Rows/Waiting/Log message clones the id — an atomic
    // bump instead of a heap copy, a few dozen times per keystroke.
    let id: Arc<str> = Arc::from(ext.id.as_str());
    // Rows in the cache are valid only under the extension definition that
    // built them — computed once; the worker dies with its ext on reload.
    let ext_stamp = crate::cache::ext_stamp(&ext);
    let native = native.map(|n| Arc::new(Mutex::new(n)));

    // ---- internal state -------------------------------------------------
    let mut available = ext.when.is_empty();
    let mut recheck: Option<Arc<Query>> = None;
    let mut opened = true;
    // Set on Opened(true), consumed by the first ask: providers whose data
    // may have moved while the launcher was away rescan exactly then.
    let mut fresh_open = true;
    let mut showing = false;
    let mut current_epoch: u64 = 0;
    let mut pending: Option<Pending> = None;
    let mut live: Option<Live> = None;
    let mut stale_shown_key = String::new();
    let mut debounce: Option<std::pin::Pin<Box<Sleep>>> = None;
    let mut refresh_at: Option<std::pin::Pin<Box<Sleep>>> = None;
    let mut proc_run: Option<JoinHandle<Option<process::Finished>>> = None;
    let mut native_run: Option<JoinHandle<NativeOutcome>> = None;
    let mut native_partial: Option<mpsc::UnboundedReceiver<Vec<Value>>> = None;
    let mut run_epoch: u64 = 0;
    let mut run_key = String::new();
    let mut run_pending: Option<Pending> = None;
    let mut refreshing_run = false;
    // The log's `ms` and `via`: when the run started and which route asked.
    let mut run_start = Instant::now();
    let mut run_via = "proc";
    // When the in-flight `when` probe began — `avail` reports the wait.
    let mut avail_start = Instant::now();
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
            avail_start = Instant::now();
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
                plog!("prov.stale", {"ep": run_epoch, "refresh": was_refresh,
                    "ms": run_start.elapsed().as_millis() as u64});
                arm_refresh!();
            } else {
                match out {
                    RunOut::Failed => {
                        run_pending = None;
                        emit!(run_epoch, EMPTY_ROWS.clone(), true)
                    }
                    RunOut::Raw(mut raw) => {
                        raw.truncate(ext.max_rows);
                        plog!("prov.done", {"ep": run_epoch, "via": run_via,
                            "refresh": was_refresh, "rows": raw.len(),
                            "ms": run_start.elapsed().as_millis() as u64});
                        // Built once, owned: this delivery, the cache entry,
                        // and every later hit share the same Arc'd row set —
                        // to_row never runs twice on one answer.
                        let rows = share_rows(build_rows_owned(&ext, raw));
                        if let Some(p) = run_pending.take() {
                            if ext.cache_ms > 0 {
                                shared.cache.lock().unwrap().put(
                                    &id,
                                    &p.key,
                                    rows.clone(),
                                    ext.cache_ms,
                                    ext_stamp,
                                );
                            }
                            live = Some(Live { epoch: p.epoch, pending: p });
                        }
                        stale_shown_key = String::new();
                        emit!(run_epoch, rows, true);
                    }
                    RunOut::Built(rows) => {
                        plog!("prov.done", {"ep": run_epoch, "via": run_via,
                            "refresh": was_refresh, "rows": rows.len(),
                            "ms": run_start.elapsed().as_millis() as u64});
                        if let Some(p) = run_pending.take() {
                            live = Some(Live { epoch: p.epoch, pending: p });
                        }
                        stale_shown_key = String::new();
                        emit!(
                            run_epoch,
                            Arc::new(
                                rows.into_iter()
                                    .take(ext.max_rows)
                                    .map(Arc::new)
                                    .collect::<Vec<_>>()
                            ),
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
            run_start = Instant::now();
            if let Some(n) = native.clone() {
                run_via = "native";
                plog!("prov.start", {"ep": p.epoch, "via": "native",
                    "q": crate::clip(&p.arg, 240)});
                let (ptx, prx) = mpsc::unbounded_channel();
                native_partial = Some(prx);
                let ctx = Ctx {
                    query: p.query.clone(),
                    arg: p.arg.clone(),
                    filters: p.filters.clone(),
                    // Arc bump — no `Settings` deep clone per run.
                    settings: shared.settings.read().await.clone(),
                    registry: shared.registry.read().await.clone(),
                    fresh_open,
                };
                fresh_open = false;
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
                    run_via = "sock";
                    plog!("prov.start", {"ep": p.epoch, "via": "sock",
                        "q": crate::clip(&p.arg, 240)});
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
                        run_via = "proc";
                        plog!("prov.start", {"ep": p.epoch, "via": "proc",
                            "cmd": crate::clip(&p.command, 240)});
                        let cmd = p.command.clone();
                        let tmo = Duration::from_millis(ext.timeout_ms);
                        proc_run = Some(tokio::spawn(async move { process::run(&cmd, tmo).await }));
                        run_pending = Some(p);
                    } else {
                        if stale_shown_key.is_empty() || stale_shown_key != p.key {
                            emit!(p.epoch, EMPTY_ROWS.clone(), true);
                        }
                        run_pending = None;
                    }
                }
            } else if !p.command.is_empty() {
                run_via = "proc";
                plog!("prov.start", {"ep": p.epoch, "via": "proc",
                    "cmd": crate::clip(&p.command, 240)});
                let cmd = p.command.clone();
                let tmo = Duration::from_millis(ext.timeout_ms);
                proc_run = Some(tokio::spawn(async move { process::run(&cmd, tmo).await }));
            } else {
                // A socket-only extension whose daemon is not listening answers
                // nothing rather than leaving the spinner up — unless stale
                // rows are already up, which are the better answer.
                if stale_shown_key.is_empty() || stale_shown_key != p.key {
                    emit!(p.epoch, EMPTY_ROWS.clone(), true);
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
                    avail_start = Instant::now();
                    let when = ext.when.clone();
                    let shared2 = shared.clone();
                    let self2 = self_tx.clone();
                    tokio::spawn(async move {
                        let ok = process::check(&when).await;
                        shared2.availability.lock().unwrap().put(&when, ok);
                        let _ = self2.send(WorkerCmd::Available { ok, replay: None });
                    });
                }
                emit!(q.epoch, EMPTY_ROWS.clone(), true);
            } else if q.scope.is_empty() && !ext.always {
                // Unscoped and not opted in: stay quiet.
                emit!(q.epoch, EMPTY_ROWS.clone(), true);
            } else if !q.routes_to(&ext.keyword, &ext.aliases) {
                emit!(q.epoch, EMPTY_ROWS.clone(), true);
            } else {
                let arg = q.arg_for(&ext.keyword, &ext.aliases);
                if arg.chars().count() < ext.min_chars {
                    emit!(q.epoch, EMPTY_ROWS.clone(), true);
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
                        let fresh = shared.cache.lock().unwrap().get(&id, &p.key, ext_stamp);
                        if let Some(hit) = fresh {
                            live = Some(Live {
                                epoch: q.epoch,
                                pending: p.clone(),
                            });
                            plog!("prov.done", {"ep": q.epoch, "via": "cache",
                                "ms": 0u64, "rows": hit.len()});
                            emit!(q.epoch, hit, true);
                            arm_refresh!();
                            answered = true;
                        }
                        if !answered {
                            let stale = shared
                                .cache
                                .lock()
                                .unwrap()
                                .get_stale(&id, &p.key, ext_stamp);
                            if let Some(stale) = stale {
                                stale_shown_key = p.key.clone();
                                plog!("prov.done", {"ep": q.epoch, "via": "stale",
                                    "ms": 0u64, "rows": stale.len()});
                                emit!(q.epoch, stale, false);
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
                                Some((id.clone(), tx.clone())),
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
                        plog!("avail", {"ok": ok,
                            "recheck": recheck.is_some(),
                            "ms": avail_start.elapsed().as_millis() as u64});
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
                        if v {
                            fresh_open = true;
                        } else {
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
                    WorkerCmd::Shutdown => return,
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
                let ms = run_start.elapsed().as_millis() as u64;
                let out = match out.ok().flatten() {
                    // The deadline killed it — the timeout line carries the
                    // configured limit, the way the QML's killer reported it.
                    Some(f) if f.timed_out => {
                        plog!("prov.timeout", {"ep": run_epoch,
                            "ms": ext.timeout_ms, "via": "proc",
                            "refresh": refreshing_run});
                        RunOut::Failed
                    }
                    Some(f) => {
                        // A nonzero exit whose answer arrives anyway is not a
                        // failure worth a line — exiting badly AND saying
                        // nothing is the case that used to pass for "no
                        // results", so it is logged and the rows still land.
                        if let Some(code) = f.code.filter(|c| *c != 0) {
                            plog!("prov.fail", {"ep": run_epoch, "code": code,
                                "ms": ms});
                        }
                        RunOut::Raw(crate::row::parse_rows(&f.stdout))
                    }
                    None => {
                        plog!("prov.fail", {"ep": run_epoch, "ms": ms});
                        RunOut::Failed
                    }
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
                    Some(p) => {
                        plog!("prov.drop", {"at": "push", "got": p.epoch,
                            "ep": current_epoch});
                    }
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
                    plog!("prov.timeout", {"ep": run_epoch,
                        "ms": ext.timeout_ms, "via": "sock",
                        "refresh": refreshing_run});
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
                            emit!(run_epoch, share_rows(build_rows_owned(&ext, raw)), false);
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
                                run_via = "proc";
                                run_start = Instant::now();
                                plog!("prov.start", {"ep": p.epoch, "via": "proc",
                                    "cmd": crate::clip(&p.command, 240)});
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
                                    run_via = "sock";
                                    run_start = Instant::now();
                                    plog!("prov.start", {"ep": p.epoch, "via": "sock",
                                        "q": crate::clip(&p.arg, 240)});
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
                        emit!(run_epoch, share_rows(build_rows_owned(&ext, raw)), false);
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
            row.score = crate::rank::score(row.tier, row.local, 0);
            rows.push(row);
        }
    }
    rows
}
