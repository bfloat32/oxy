//! The ask path: the question goes out by whichever route is up — native
//! first, then a connected socket, then the command — plus the cache reads
//! that can answer before a run exists and the socket push handling.
//!
//! These are `WorkerState` methods; the struct itself lives in `state.rs`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::sync::{Mutex, mpsc};
use tokio::time::Instant;

use crate::provider::socket::{self, SocketPush, SocketReq};
use crate::provider::{Ctx, NativeExt, process};

use super::state::WorkerState;
use super::{EMPTY_ROWS, Live, Pending, RunOut};

impl WorkerState {
    /// The question, by whichever route is up: native first, then a connected
    /// socket, then the command — a daemon that died degrades to the slow path
    /// rather than going silent.
    pub(super) async fn ask(&mut self, p: Pending, refresh: bool) {
        self.run_epoch = p.epoch;
        self.run_key = p.key.clone();
        self.refreshing_run = refresh;
        self.run_pending = Some(p.clone());
        self.run_start = Instant::now();
        if let Some(n) = self.native.clone() {
            self.start_native(p, n).await;
        } else if self.socket.is_some() {
            if self.start_socket(&p, false).await {
                self.run_pending = Some(p);
            } else {
                // The daemon's end of the socket is gone: drop the route
                // and fall through to the command, the way a dead socket
                // always degraded to the slow path.
                self.socket = None;
                self.sock_pending = None;
                if !p.command.is_empty() {
                    self.start_proc(&p, false).await;
                    self.run_pending = Some(p);
                } else {
                    if self.stale_shown_key.is_empty() || self.stale_shown_key != p.key {
                        self.emit(p.epoch, EMPTY_ROWS.clone(), true).await;
                    }
                    self.run_pending = None;
                }
            }
        } else if !p.command.is_empty() {
            self.start_proc(&p, false).await;
        } else {
            // A socket-only extension whose daemon is not listening answers
            // nothing rather than leaving the spinner up — unless stale
            // rows are already up, which are the better answer.
            if self.stale_shown_key.is_empty() || self.stale_shown_key != p.key {
                self.emit(p.epoch, EMPTY_ROWS.clone(), true).await;
            }
            self.run_pending = None;
        }
    }

    /// The native route: `prov.start`, a progress channel the provider can
    /// stream partial answers on, and the query spawned on its own task.
    async fn start_native(&mut self, p: Pending, n: Arc<Mutex<Box<dyn NativeExt>>>) {
        self.run_via = "native";
        self.plog(
            "prov.start",
            json!({"ep": p.epoch, "via": "native",
                "q": crate::clip(&p.arg, 240)}),
        )
        .await;
        let (ptx, prx) = mpsc::unbounded_channel();
        self.native_partial = Some(prx);
        let ctx = Ctx {
            query: p.query.clone(),
            arg: p.arg.clone(),
            filters: p.filters.clone(),
            // Arc bump — no `Settings` deep clone per run.
            settings: self.shared.settings.read().await.clone(),
            registry: self.shared.registry.read().await.clone(),
            fresh_open: self.fresh_open,
        };
        self.fresh_open = false;
        self.native_run = Some(tokio::spawn(
            async move { n.lock().await.query(ctx, ptx).await },
        ));
        self.native_deadline = Some(Box::pin(tokio::time::sleep_until(
            Instant::now() + Duration::from_millis(self.ext.timeout_ms),
        )));
    }

    /// The command route: `prov.start` and the `search` spawned with the
    /// extension's timeout. `reset_clock` restarts `ms` for a leg that
    /// follows a declined native run.
    async fn start_proc(&mut self, p: &Pending, reset_clock: bool) {
        self.run_via = "proc";
        if reset_clock {
            self.run_start = Instant::now();
        }
        self.plog(
            "prov.start",
            json!({"ep": p.epoch, "via": "proc",
                "cmd": crate::clip(&p.command, 240)}),
        )
        .await;
        let cmd = p.command.clone();
        let tmo = Duration::from_millis(self.ext.timeout_ms);
        self.proc_run = Some(tokio::spawn(async move { process::run(&cmd, tmo).await }));
    }

    /// The socket route: the question goes to the daemon and the answer is
    /// owed by the deadline — the old `killer` timer. `reset_clock` restarts
    /// `ms` for a leg that follows a declined native run. False when the
    /// daemon's end of the socket is gone; the caller drops the route.
    async fn start_socket(&mut self, p: &Pending, reset_clock: bool) -> bool {
        let sent = self
            .socket
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
            self.run_via = "sock";
            if reset_clock {
                self.run_start = Instant::now();
            }
            self.plog(
                "prov.start",
                json!({"ep": p.epoch, "via": "sock",
                    "q": crate::clip(&p.arg, 240)}),
            )
            .await;
            self.sock_pending = Some(p.clone());
            self.sock_waiting = true;
            self.sock_deadline = Some(Box::pin(tokio::time::sleep_until(
                Instant::now() + Duration::from_millis(self.ext.timeout_ms),
            )));
        }
        sent
    }

    /// The native provider declined: the declared socket or command answers
    /// instead — unless `when` said the box lacks what they need.
    pub(super) async fn fallback(&mut self) {
        if let Some(p) = self.run_pending.take() {
            if !self.available {
                if self.run_epoch == self.current_epoch {
                    self.deliver(RunOut::Raw(Vec::new())).await;
                }
            } else if !p.command.is_empty() {
                self.start_proc(&p, true).await;
                self.run_pending = Some(p);
            } else if self.socket.is_some() {
                if self.start_socket(&p, true).await {
                    self.run_pending = Some(p);
                } else {
                    self.socket = None;
                    self.sock_pending = None;
                    if self.run_epoch == self.current_epoch {
                        self.deliver(RunOut::Raw(Vec::new())).await;
                    }
                }
            } else if self.run_epoch == self.current_epoch {
                self.deliver(RunOut::Raw(Vec::new())).await;
            }
        }
    }

    /// The cache reads: a fresh hit answers the ask outright (its own
    /// `prov.done` site); a stale entry shows while the run revalidates.
    /// Returns whether the question was answered.
    pub(super) async fn check_cache(&mut self, p: &Pending) -> bool {
        let mut answered = false;
        // The guards are bound to their own statements so
        // they die before any `.await` below — a MutexGuard
        // is not Send and holding one across the emit would
        // poison the whole task's future.
        let fresh = self
            .shared
            .cache
            .lock()
            .unwrap()
            .get(&self.id, &p.key, self.ext_stamp);
        if let Some(hit) = fresh {
            self.live = Some(Live {
                epoch: p.epoch,
                pending: p.clone(),
            });
            self.plog(
                "prov.done",
                json!({"ep": p.epoch, "via": "cache",
                    "ms": 0u64, "rows": hit.len()}),
            )
            .await;
            self.emit(p.epoch, hit, true).await;
            self.arm_refresh();
            answered = true;
        }
        if !answered {
            let stale =
                self.shared
                    .cache
                    .lock()
                    .unwrap()
                    .get_stale(&self.id, &p.key, self.ext_stamp);
            if let Some(stale) = stale {
                self.stale_shown_key = p.key.clone();
                self.plog(
                    "prov.done",
                    json!({"ep": p.epoch, "via": "stale",
                        "ms": 0u64, "rows": stale.len()}),
                )
                .await;
                self.emit(p.epoch, stale, false).await;
            }
        }
        answered
    }

    /// Connecting is driven by queries, not a retry timer — throttled to one
    /// attempt per three seconds, bounded by two seconds or the timeout.
    pub(super) async fn try_connect(&mut self) {
        if !self.ext.socket.is_empty()
            && self.socket.is_none()
            && self.last_connect.elapsed() > Duration::from_millis(3000)
        {
            self.last_connect = Instant::now();
            if let Ok(chan) = socket::connect(
                &self.ext.socket,
                Duration::from_millis(2000).min(Duration::from_millis(self.ext.timeout_ms.max(1))),
                Some((self.id.clone(), self.tx.clone())),
            )
            .await
            {
                self.socket = Some(chan);
            }
        }
    }

    /// A line the daemon pushed: an answer or a refinement for the live
    /// epoch, a drop log for a foreign one, a hangup on close.
    pub(super) async fn on_push(&mut self, push: Option<SocketPush>) {
        match push {
            // Every line the daemon sends for the live epoch is a
            // complete answer — an answer, or a later refinement of
            // one. `do:` streams its card this way.
            Some(p) if self.opened && p.epoch == self.current_epoch => {
                self.sock_deadline = None;
                self.sock_waiting = false;
                if self.run_pending.is_none() {
                    self.run_pending = self.sock_pending.clone().filter(|q| q.epoch == p.epoch);
                }
                self.run_epoch = p.epoch;
                self.run_key = self
                    .sock_pending
                    .as_ref()
                    .filter(|q| q.epoch == p.epoch)
                    .map(|q| q.key.clone())
                    .unwrap_or_default();
                self.deliver(RunOut::Raw(p.rows)).await;
            }
            Some(p) => {
                self.plog(
                    "prov.drop",
                    json!({"at": "push", "got": p.epoch,
                        "ep": self.current_epoch}),
                )
                .await;
            }
            // The daemon hung up. The deadline, if a question is
            // owed, still resolves it; the next ask reconnects.
            None => self.socket = None,
        }
    }

    /// The native run's deadline — the fuse the proc leg carries inside
    /// `process::run` and the socket leg carries as `sock_deadline`. A
    /// provider that never returns is a decline with prejudice: the
    /// declared script leg gets its own bounded try rather than the
    /// spinner hanging on a wedged native call.
    pub(super) async fn on_native_deadline(&mut self) {
        self.native_deadline = None;
        // Take before abort: a dropped JoinHandle detaches the task, so
        // its late completion lands nowhere instead of overwriting the
        // fallback leg's run.
        if let Some(h) = self.native_run.take() {
            h.abort();
        }
        // A task wedged in a blocking call survives abort until it yields;
        // dropping the receiver keeps its stale partials off the wire.
        self.native_partial = None;
        self.plog(
            "prov.timeout",
            json!({"ep": self.run_epoch,
                "ms": self.ext.timeout_ms, "via": "native",
                "refresh": self.refreshing_run}),
        )
        .await;
        self.fallback().await;
    }

    /// The socket ask's deadline — the old `killer` timer: a daemon that
    /// never speaks resolves as a failed run.
    pub(super) async fn on_sock_deadline(&mut self) {
        self.sock_deadline = None;
        if self.sock_waiting {
            self.sock_waiting = false;
            self.plog(
                "prov.timeout",
                json!({"ep": self.run_epoch,
                    "ms": self.ext.timeout_ms, "via": "sock",
                    "refresh": self.refreshing_run}),
            )
            .await;
            // The old killer: a refresh keeps its rows, stale keeps
            // stale, anything else is an empty answer.
            self.deliver(RunOut::Failed).await;
        }
    }
}
