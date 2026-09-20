//! Worker reconciliation: spawn the workers the registry wants, retire the
//! ones whose extension changed or left.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::mpsc;

use super::Engine;
use crate::provider::worker::{self, WorkerCmd};
use crate::registry::Extension;

impl Engine {
    /// One worker per extension that can answer, and no worker for one that
    /// changed or left. A worker answers with the `Extension` it was built
    /// with, so a reload that only *added* workers would leave an edited
    /// extension answering with its old definition forever — the QML's
    /// Instantiator rebuilt every provider on registry change.
    pub(super) async fn spawn_workers(&mut self, extensions: &Arc<Vec<Extension>>) {
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
}
