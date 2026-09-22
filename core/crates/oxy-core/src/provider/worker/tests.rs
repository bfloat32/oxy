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
