//! The worker's state: every local `run` used to carry, now fields, with the
//! state machine's moves as methods — the asks, the deliveries, the timers.
//!
//! `route.rs` holds the other half of the `impl`: the ask path's
//! native → socket → command fallback, the cache reads, and the socket push
//! handling.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tokio::time::{Instant, Sleep};

use crate::model::query::Query;
use crate::model::row::SharedRows;
use crate::provider::process;
use crate::provider::socket::SocketChan;
use crate::provider::{NativeExt, NativeOutcome};
use crate::registry::{Extension, build_command, cache_key};

use super::{
    EMPTY_ROWS, Live, Pending, RunOut, Shared, WorkerCmd, WorkerMsg, build_rows_owned, share_rows,
};

/// Everything one extension's task remembers between events — what used to be
/// `run`'s local variables. The select loop in `mod.rs` owns the channels and
/// delegates every event to a method here; `route.rs` holds the ask path.
pub(super) struct WorkerState {
    // `Arc<str>`: every Rows/Waiting/Log message clones the id — an atomic
    // bump instead of a heap copy, a few dozen times per keystroke.
    pub(super) id: Arc<str>,
    pub(super) ext: Extension,
    // Rows in the cache are valid only under the extension definition that
    // built them — computed once; the worker dies with its ext on reload.
    pub(super) ext_stamp: u64,
    pub(super) native: Option<Arc<Mutex<Box<dyn NativeExt>>>>,
    // The worker's own queue: `when` probes report back on it.
    pub(super) self_tx: mpsc::UnboundedSender<WorkerCmd>,
    pub(super) tx: mpsc::Sender<WorkerMsg>,
    pub(super) shared: Arc<Shared>,

    // ---- internal state -------------------------------------------------
    pub(super) available: bool,
    pub(super) recheck: Option<Arc<Query>>,
    pub(super) opened: bool,
    // Set on Opened(true), consumed by the first ask: providers whose data
    // may have moved while the launcher was away rescan exactly then.
    pub(super) fresh_open: bool,
    pub(super) showing: bool,
    pub(super) current_epoch: u64,
    pub(super) pending: Option<Pending>,
    pub(super) live: Option<Live>,
    pub(super) stale_shown_key: String,
    pub(super) debounce: Option<std::pin::Pin<Box<Sleep>>>,
    pub(super) refresh_at: Option<std::pin::Pin<Box<Sleep>>>,
    pub(super) proc_run: Option<JoinHandle<Option<process::Finished>>>,
    pub(super) native_run: Option<JoinHandle<NativeOutcome>>,
    pub(super) native_partial: Option<mpsc::UnboundedReceiver<Vec<Value>>>,
    // The native run's fuse — the command leg has process::run's timeout
    // and the socket leg has `sock_deadline`; without one here a hung
    // provider would hold the run forever.
    pub(super) native_deadline: Option<std::pin::Pin<Box<Sleep>>>,
    pub(super) run_epoch: u64,
    pub(super) run_key: String,
    pub(super) run_pending: Option<Pending>,
    pub(super) refreshing_run: bool,
    // The log's `ms` and `via`: when the run started and which route asked.
    pub(super) run_start: Instant,
    pub(super) run_via: &'static str,
    // When the in-flight `when` probe began — `avail` reports the wait.
    pub(super) avail_start: Instant,
    // The socket connection is an actor of its own, so a run can borrow it
    // without borrowing the worker — and every line the daemon pushes lands
    // on the push channel, not only the one answering the last question.
    pub(super) socket: Option<SocketChan>,
    // The question the socket daemon was last asked, kept after its first
    // answer: pushes for the same epoch still have a question to belong to.
    pub(super) sock_pending: Option<Pending>,
    // A socket question owed its first answer; the deadline is the old
    // `killer` timer — a daemon that never speaks resolves as a failed run.
    pub(super) sock_waiting: bool,
    pub(super) sock_deadline: Option<std::pin::Pin<Box<Sleep>>>,
    pub(super) last_connect: Instant,
}

impl WorkerState {
    pub(super) fn new(
        ext: Extension,
        native: Option<Box<dyn NativeExt>>,
        self_tx: mpsc::UnboundedSender<WorkerCmd>,
        tx: mpsc::Sender<WorkerMsg>,
        shared: Arc<Shared>,
    ) -> Self {
        // `Arc<str>`: every Rows/Waiting/Log message clones the id — an atomic
        // bump instead of a heap copy, a few dozen times per keystroke.
        let id: Arc<str> = Arc::from(ext.id.as_str());
        // Rows in the cache are valid only under the extension definition that
        // built them — computed once; the worker dies with its ext on reload.
        let ext_stamp = crate::support::cache::ext_stamp(&ext);
        let native = native.map(|n| Arc::new(Mutex::new(n)));
        let available = ext.when.is_empty();
        let mut st = Self {
            id,
            ext,
            ext_stamp,
            native,
            self_tx,
            tx,
            shared,
            available,
            recheck: None,
            opened: true,
            fresh_open: true,
            showing: false,
            current_epoch: 0,
            pending: None,
            live: None,
            stale_shown_key: String::new(),
            debounce: None,
            refresh_at: None,
            proc_run: None,
            native_run: None,
            native_partial: None,
            native_deadline: None,
            run_epoch: 0,
            run_key: String::new(),
            run_pending: None,
            refreshing_run: false,
            run_start: Instant::now(),
            run_via: "proc",
            avail_start: Instant::now(),
            socket: None,
            sock_pending: None,
            sock_waiting: false,
            sock_deadline: None,
            last_connect: Instant::now() - Duration::from_secs(60),
        };
        st.probe_when();
        st
    }

    /// `when` is checked once at spawn; a failure re-checks only when the
    /// keyword is actually typed, at most every fifteen seconds.
    fn probe_when(&mut self) {
        if !self.available {
            if let Some(ok) = self
                .shared
                .availability
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&self.ext.when)
            {
                self.available = ok;
            } else {
                self.avail_start = Instant::now();
                let when = self.ext.when.clone();
                let shared2 = self.shared.clone();
                let self2 = self.self_tx.clone();
                tokio::spawn(async move {
                    let ok = process::check(&when).await;
                    shared2
                        .availability
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .put(&when, ok);
                    let _ = self2.send(WorkerCmd::Available { ok, replay: None });
                });
            }
        }
    }

    pub(super) async fn emit(&self, epoch: u64, rows: SharedRows, last: bool) {
        let _ = self
            .tx
            .send(WorkerMsg::Rows {
                id: self.id.clone(),
                epoch,
                rows,
                last,
            })
            .await;
    }

    pub(super) async fn plog(&self, ev: &str, fields: Value) {
        let _ = self
            .tx
            .send(WorkerMsg::Log {
                id: self.id.clone(),
                ev: ev.into(),
                fields,
            })
            .await;
    }

    /// Every run's exit path: the cache write, the stale rule, the refresh
    /// arming and the emit all live here so no caller can forget one.
    pub(super) async fn deliver(&mut self, out: RunOut) {
        let was_refresh = self.refreshing_run;
        self.refreshing_run = false;
        let failed = matches!(out, RunOut::Failed)
            || matches!(&out, RunOut::Raw(r) if r.is_empty())
            || matches!(&out, RunOut::Built(r) if r.is_empty());
        // An empty answer over rows that were already on screen is nearly
        // always a timeout, not the answer becoming "no rows".
        let keep_stale = failed
            && (was_refresh
                || (!self.stale_shown_key.is_empty() && self.stale_shown_key == self.run_key));
        if keep_stale {
            self.run_pending = None;
            self.plog(
                "prov.stale",
                json!({"ep": self.run_epoch, "refresh": was_refresh,
                    "ms": self.run_start.elapsed().as_millis() as u64}),
            )
            .await;
            self.arm_refresh();
        } else {
            match out {
                RunOut::Failed => {
                    self.run_pending = None;
                    self.emit(self.run_epoch, EMPTY_ROWS.clone(), true).await
                }
                RunOut::Raw(mut raw) => {
                    raw.truncate(self.ext.max_rows);
                    self.plog(
                        "prov.done",
                        json!({"ep": self.run_epoch, "via": self.run_via,
                            "refresh": was_refresh, "rows": raw.len(),
                            "ms": self.run_start.elapsed().as_millis() as u64}),
                    )
                    .await;
                    // Built once, owned: this delivery, the cache entry,
                    // and every later hit share the same Arc'd row set —
                    // to_row never runs twice on one answer.
                    let rows = share_rows(build_rows_owned(&self.ext, raw));
                    if let Some(p) = self.run_pending.take() {
                        if self.ext.cache_ms > 0 {
                            self.shared
                                .cache
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .put(
                                    &self.id,
                                    &p.key,
                                    rows.clone(),
                                    self.ext.cache_ms,
                                    self.ext_stamp,
                                );
                        }
                        self.live = Some(Live {
                            epoch: p.epoch,
                            pending: p,
                        });
                    }
                    self.stale_shown_key = String::new();
                    self.emit(self.run_epoch, rows, true).await;
                }
                RunOut::Built(rows) => {
                    self.plog(
                        "prov.done",
                        json!({"ep": self.run_epoch, "via": self.run_via,
                            "refresh": was_refresh, "rows": rows.len(),
                            "ms": self.run_start.elapsed().as_millis() as u64}),
                    )
                    .await;
                    if let Some(p) = self.run_pending.take() {
                        self.live = Some(Live {
                            epoch: p.epoch,
                            pending: p,
                        });
                    }
                    self.stale_shown_key = String::new();
                    self.emit(
                        self.run_epoch,
                        Arc::new(
                            rows.into_iter()
                                .take(self.ext.max_rows)
                                .map(Arc::new)
                                .collect::<Vec<_>>(),
                        ),
                        true,
                    )
                    .await;
                }
            }
            self.arm_refresh();
        }
    }

    pub(super) fn arm_refresh(&mut self) {
        self.refresh_at = if self.ext.refresh_ms > 0
            && self.opened
            && self.showing
            && self
                .live
                .as_ref()
                .is_some_and(|l| l.epoch == self.current_epoch)
        {
            Some(Box::pin(tokio::time::sleep_until(
                Instant::now() + Duration::from_millis(self.ext.refresh_ms),
            )))
        } else {
            None
        };
    }

    /// Stopping a run is two things: aborting the in-flight task, and
    /// forgetting what it was for. The debounce path does only the first —
    /// `ask` writes a new `run_pending` immediately after.
    pub(super) fn abort_run(&mut self) {
        if let Some(h) = self.proc_run.take() {
            h.abort();
        }
        if let Some(h) = self.native_run.take() {
            h.abort();
        }
        self.native_partial = None;
        self.native_deadline = None;
        // A socket ask has no task to abort: the line already went out,
        // and its answer lands on the push channel — epoch-filtered on
        // arrival, so a late one lands nowhere.
        self.sock_deadline = None;
        self.sock_waiting = false;
        self.sock_pending = None;
    }

    pub(super) fn cancel_run(&mut self) {
        self.abort_run();
        self.run_pending = None;
        self.refreshing_run = false;
    }

    /// The Ask handler — the availability replay runs the same code.
    pub(super) async fn handle_ask(&mut self, q: Arc<Query>) {
        self.current_epoch = q.epoch;
        self.refreshing_run = false;
        self.refresh_at = None;
        self.stale_shown_key = String::new();

        // `when` guards the command and socket legs — the things that
        // need a program on the box. A native provider is its own answer:
        // it runs regardless and declines through Fallback, at which
        // point the check matters again.
        if !self.available && self.native.is_none() {
            let due = self
                .shared
                .availability
                .lock()
                .unwrap()
                .get(&self.ext.when)
                .is_none();
            if !self.ext.when.is_empty() && q.routes_to(&self.ext.keyword, &self.ext.aliases) && due
            {
                self.recheck = Some(q.clone());
                self.avail_start = Instant::now();
                let when = self.ext.when.clone();
                let shared2 = self.shared.clone();
                let self2 = self.self_tx.clone();
                tokio::spawn(async move {
                    let ok = process::check(&when).await;
                    shared2
                        .availability
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .put(&when, ok);
                    let _ = self2.send(WorkerCmd::Available { ok, replay: None });
                });
            }
            self.emit(q.epoch, EMPTY_ROWS.clone(), true).await;
        } else if q.scope.is_empty() && !self.ext.always {
            // Unscoped and not opted in: stay quiet.
            self.emit(q.epoch, EMPTY_ROWS.clone(), true).await;
        } else if !q.routes_to(&self.ext.keyword, &self.ext.aliases) {
            self.emit(q.epoch, EMPTY_ROWS.clone(), true).await;
        } else {
            let arg = q.arg_for(&self.ext.keyword, &self.ext.aliases);
            if arg.chars().count() < self.ext.min_chars {
                self.emit(q.epoch, EMPTY_ROWS.clone(), true).await;
            } else {
                let filters = Arc::new(q.extras(&self.ext.keyword, &self.ext.aliases));
                let command = if self.ext.search.is_empty() {
                    String::new()
                } else {
                    let settings = self.shared.settings.read().await;
                    build_command(
                        &self.ext,
                        &arg,
                        &filters,
                        settings.settings_for(&self.ext.id),
                    )
                };
                let key = cache_key(&self.ext, &command, &arg, &filters);
                let p = Pending {
                    epoch: q.epoch,
                    arg,
                    filters,
                    command,
                    key,
                    query: q.clone(),
                };

                let mut answered = false;
                if self.ext.cache_ms > 0 {
                    answered = self.check_cache(&p).await;
                }

                if !answered {
                    // Connecting is driven by queries, not a retry timer.
                    self.try_connect().await;
                    let _ = self
                        .tx
                        .send(WorkerMsg::Waiting {
                            id: self.id.clone(),
                            epoch: q.epoch,
                        })
                        .await;
                    self.pending = Some(p);
                    self.debounce = Some(Box::pin(tokio::time::sleep_until(
                        Instant::now() + Duration::from_millis(self.ext.debounce_ms),
                    )));
                }
            }
        }
    }

    /// A `when` check resolved; the query that triggered it is owed a replay.
    pub(super) async fn on_available(&mut self, ok: bool, replay: Option<Arc<Query>>) {
        self.available = ok;
        self.plog(
            "avail",
            json!({"ok": ok,
                "recheck": self.recheck.is_some(),
                "ms": self.avail_start.elapsed().as_millis() as u64}),
        )
        .await;
        if ok {
            let q = replay.or_else(|| self.recheck.take());
            if let Some(q) = q
                && q.epoch == self.current_epoch
            {
                self.handle_ask(q).await;
            }
        }
    }

    /// The launcher is open (refresh and availability replays gate on it).
    pub(super) fn on_opened(&mut self, v: bool) {
        self.opened = v;
        if v {
            self.fresh_open = true;
        } else {
            self.pending = None;
            self.live = None;
            self.refresh_at = None;
            self.debounce = None;
            self.cancel_run();
        }
    }

    /// Whether this provider's rows are on screen — refresh only re-asks a
    /// question whose answer is showing.
    pub(super) fn on_showing(&mut self, v: bool) {
        self.showing = v;
        if v {
            self.arm_refresh();
        } else {
            self.refresh_at = None;
        }
    }

    /// The launcher closed: stop the refresh and any run; keep the cache.
    pub(super) fn cancel(&mut self) {
        self.pending = None;
        self.live = None;
        self.refresh_at = None;
        self.debounce = None;
        self.cancel_run();
    }

    /// The debounce fired: the queued question becomes a run.
    pub(super) async fn on_debounce(&mut self) {
        self.debounce = None;
        if let Some(p) = self.pending.take() {
            // A run already in flight is told to stop; the newer
            // question takes the slot.
            self.abort_run();
            self.ask(p, false).await;
        }
    }

    /// A `search` command finished — timed out, failed, or answered.
    pub(super) async fn on_proc_done(
        &mut self,
        out: Result<Option<process::Finished>, tokio::task::JoinError>,
    ) {
        self.proc_run = None;
        let ms = self.run_start.elapsed().as_millis() as u64;
        let out = match out.ok().flatten() {
            // The deadline killed it — the timeout line carries the
            // configured limit, the way the QML's killer reported it.
            Some(f) if f.timed_out => {
                self.plog(
                    "prov.timeout",
                    json!({"ep": self.run_epoch,
                        "ms": self.ext.timeout_ms, "via": "proc",
                        "refresh": self.refreshing_run}),
                )
                .await;
                RunOut::Failed
            }
            Some(f) => {
                // A nonzero exit whose answer arrives anyway is not a
                // failure worth a line — exiting badly AND saying
                // nothing is the case that used to pass for "no
                // results", so it is logged and the rows still land.
                if let Some(code) = f.code.filter(|c| *c != 0) {
                    self.plog(
                        "prov.fail",
                        json!({"ep": self.run_epoch, "code": code, "ms": ms}),
                    )
                    .await;
                }
                RunOut::Raw(crate::model::row::parse_rows(&f.stdout))
            }
            None => {
                self.plog("prov.fail", json!({"ep": self.run_epoch, "ms": ms}))
                    .await;
                RunOut::Failed
            }
        };
        if self.run_epoch == self.current_epoch {
            self.deliver(out).await;
        } else {
            self.plog("prov.drop", json!({"at": "finish", "ep": self.run_epoch}))
                .await;
        }
    }

    /// The native provider's task finished — rows, a decline, or a crash.
    pub(super) async fn on_native_done(
        &mut self,
        out: Result<NativeOutcome, tokio::task::JoinError>,
    ) {
        self.native_run = None;
        self.native_deadline = None;
        // Flush anything the provider pushed on its way out.
        if let Some(mut prx) = self.native_partial.take() {
            while let Ok(raw) = prx.try_recv() {
                if self.run_epoch == self.current_epoch {
                    self.emit(
                        self.run_epoch,
                        share_rows(build_rows_owned(&self.ext, raw)),
                        false,
                    )
                    .await;
                }
            }
        }
        match out {
            Ok(NativeOutcome::Rows(raw)) => {
                if self.run_epoch == self.current_epoch {
                    self.deliver(RunOut::Raw(raw)).await;
                }
            }
            Ok(NativeOutcome::Built(rows)) => {
                if self.run_epoch == self.current_epoch {
                    self.deliver(RunOut::Built(rows)).await;
                }
            }
            Ok(NativeOutcome::Empty) => {
                if self.run_epoch == self.current_epoch {
                    self.deliver(RunOut::Raw(Vec::new())).await;
                }
            }
            Ok(NativeOutcome::Fallback) => self.fallback().await,
            Err(_) => {
                if self.run_epoch == self.current_epoch {
                    self.deliver(RunOut::Failed).await;
                }
            }
        }
    }

    /// A native provider pushed a partial answer mid-run — each send replaces
    /// the provider's bucket for the epoch without clearing its spinner.
    pub(super) async fn on_native_partial(&mut self, raw: Option<Vec<Value>>) {
        match raw {
            Some(raw) if self.run_epoch == self.current_epoch => {
                self.emit(
                    self.run_epoch,
                    share_rows(build_rows_owned(&self.ext, raw)),
                    false,
                )
                .await;
            }
            Some(_) => {}
            None => self.native_partial = None,
        }
    }

    /// The refresh timer fired: re-ask the question on screen, unless a real
    /// query is queued or running — that re-arms the timer when it lands.
    pub(super) async fn on_refresh(&mut self) {
        self.refresh_at = None;
        // A real query queued or running re-arms this when it lands;
        // two runs for one extension is the pile-up the debounce
        // exists to prevent.
        if self.opened
            && self.showing
            && self.pending.is_none()
            && self.proc_run.is_none()
            && self.native_run.is_none()
            && !self.sock_waiting
            && self
                .live
                .as_ref()
                .is_some_and(|l| l.epoch == self.current_epoch)
        {
            let p = self.live.take().map(|l| l.pending).unwrap();
            self.ask(p, true).await;
        }
    }
}
