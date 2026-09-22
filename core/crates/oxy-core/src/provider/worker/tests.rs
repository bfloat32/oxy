//! The native run's deadline: a provider that never answers must resolve
//! the run through the declared command leg — the same fuse the socket leg
//! already carried, so a wedged native call cannot hold a query forever.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{RwLock, mpsc};

use super::{Shared, WorkerCmd, WorkerMsg, run};
use crate::model::query::Query;
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::registry::Extension;
use crate::settings::Settings;

/// A provider whose answer never comes — the future pends forever, which
/// is what a native call wedged in a syscall looks like to the loop.
struct Hang;

impl NativeExt for Hang {
    fn query<'a>(
        &'a mut self,
        _ctx: Ctx,
        _progress: mpsc::UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn std::future::Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(std::future::pending())
    }
}

fn shared() -> Arc<Shared> {
    Arc::new(Shared {
        cache: std::sync::Mutex::new(crate::support::cache::Cache::default()),
        availability: std::sync::Mutex::new(crate::support::availability::Availability::default()),
        settings: RwLock::new(Arc::new(Settings::default())),
        registry: RwLock::new(Arc::new(Vec::new())),
        hello_keywords: std::sync::RwLock::new(Arc::new(Vec::new())),
    })
}

fn ext() -> Extension {
    Extension {
        id: "hang".into(),
        keyword: "zzhang".into(),
        search: "echo done".into(),
        timeout_ms: 150,
        always: true,
        min_chars: 1,
        native: "hang".into(),
        ..Default::default()
    }
}

/// The hung native run hits its deadline and the worker falls through to
/// the script leg: `prov.timeout via=native`, then `prov.start via=proc`.
/// Asserting the log sequence rather than the rows keeps the test honest
/// on a box without `bash` — the routing is the contract under test.
#[tokio::test]
async fn a_hung_native_run_falls_back_at_the_deadline() {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (msg_tx, mut msg_rx) = mpsc::channel(64);
    let (self_tx, _self_rx) = mpsc::unbounded_channel();
    tokio::spawn(run(
        ext(),
        Some(Box::new(Hang)),
        self_tx,
        cmd_rx,
        msg_tx,
        shared(),
    ));

    cmd_tx
        .send(WorkerCmd::Ask(Arc::new(Query::parse("anything", 1, None))))
        .unwrap();

    let mut saw_native_timeout = false;
    let mut saw_proc_start = false;
    let collect = async {
        while let Some(msg) = msg_rx.recv().await {
            if let WorkerMsg::Log { ev, fields, .. } = msg {
                let via = fields.get("via").and_then(|v| v.as_str()).unwrap_or("");
                if ev == "prov.timeout" && via == "native" {
                    saw_native_timeout = true;
                }
                if ev == "prov.start" && via == "proc" {
                    saw_proc_start = true;
                    break;
                }
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(10), collect).await;

    assert!(saw_native_timeout, "the hung run never hit its deadline");
    assert!(
        saw_proc_start,
        "the deadline did not route to the script leg"
    );
}

/// A native's partial carrying `pending: true` reaches the wire with the
/// flag on — the calc placeholder is a hand-built row the way the QML one
/// was, and `to_row` drops `pending` only for untrusted script JSON.
struct Placeholder;

impl NativeExt for Placeholder {
    fn query<'a>(
        &'a mut self,
        _ctx: Ctx,
        progress: mpsc::UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn std::future::Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move {
            let _ = progress.send(vec![serde_json::json!({
                "id": "x",
                "title": "calculating",
                "pending": true,
            })]);
            NativeOutcome::Rows(vec![serde_json::json!({
                "id": "x",
                "title": "done",
            })])
        })
    }
}

#[tokio::test]
async fn a_native_placeholder_keeps_its_pending_flag() {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (msg_tx, mut msg_rx) = mpsc::channel(64);
    let (self_tx, _self_rx) = mpsc::unbounded_channel();
    let mut ext = ext();
    ext.max_rows = 50;
    tokio::spawn(run(
        ext,
        Some(Box::new(Placeholder)),
        self_tx,
        cmd_rx,
        msg_tx,
        shared(),
    ));

    cmd_tx
        .send(WorkerCmd::Ask(Arc::new(Query::parse("anything", 1, None))))
        .unwrap();

    let mut saw_pending = false;
    let mut settled = false;
    let collect = async {
        while let Some(msg) = msg_rx.recv().await {
            if let WorkerMsg::Rows { rows, last, .. } = msg {
                if !last {
                    saw_pending |= rows.iter().any(|r| r.pending);
                }
                if last {
                    settled = true;
                    break;
                }
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(10), collect).await;

    assert!(saw_pending, "the placeholder's pending flag was dropped");
    assert!(settled, "the real answer still landed last");
}

/// An answered empty over stale rows keeps the stale set and sends `Done` —
/// the seeded wait clears without a `last` rows emit to carry it.
struct Empty;

impl NativeExt for Empty {
    fn query<'a>(
        &'a mut self,
        _ctx: Ctx,
        _progress: mpsc::UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn std::future::Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move { NativeOutcome::Empty })
    }
}

#[tokio::test]
async fn a_kept_stale_answer_clears_the_wait() {
    let mut ext = ext();
    ext.cache_ms = 1;
    ext.search = String::new();
    ext.max_rows = 50;
    let sh = shared();
    let q = Query::parse("anything", 1, None);
    let arg = q.arg_for(&ext.keyword, &ext.aliases);
    let filters = q.extras(&ext.keyword, &ext.aliases);
    let key = crate::registry::cache_key(&ext, "", &arg, &filters);
    let stamp = crate::support::cache::ext_stamp(&ext);
    let stale: crate::model::row::SharedRows =
        Arc::new(vec![Arc::new(crate::model::row::Row::new("k", "hang"))]);
    sh.cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .put("hang", &key, stale, 1, stamp);
    // Past the one-millisecond ttl the entry is stale, not fresh.
    tokio::time::sleep(Duration::from_millis(5)).await;

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (msg_tx, mut msg_rx) = mpsc::channel(64);
    let (self_tx, _self_rx) = mpsc::unbounded_channel();
    tokio::spawn(run(ext, Some(Box::new(Empty)), self_tx, cmd_rx, msg_tx, sh));

    cmd_tx.send(WorkerCmd::Ask(Arc::new(q))).unwrap();

    let mut saw_stale = false;
    let mut saw_done = false;
    let mut saw_waiting = false;
    let mut saw_last_rows = false;
    let collect = async {
        while let Some(msg) = msg_rx.recv().await {
            match msg {
                WorkerMsg::Rows { rows, last, .. } => {
                    saw_stale |= !last && !rows.is_empty();
                    saw_last_rows |= last;
                }
                WorkerMsg::Waiting { .. } => saw_waiting = true,
                // The run's own Done — after the native Empty resolves.
                WorkerMsg::Done { .. } if saw_stale => {
                    saw_done = true;
                    break;
                }
                WorkerMsg::Done { .. } => {}
                _ => {}
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(10), collect).await;

    assert!(saw_stale, "the stale answer never drew");
    assert!(saw_done, "the kept stale answer never retired the wait");
    assert!(
        !saw_waiting,
        "stale rows already up are not the spinner's business"
    );
    assert!(!saw_last_rows, "nothing last-emitted over the kept rows");
}
