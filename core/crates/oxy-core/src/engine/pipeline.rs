//! The query pipeline: parse, dispatch to workers, merge, publish.

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{Value, json};

use super::builtins::{builtin_help, now_ms};
use super::{Engine, EngineEvent};
use crate::model::query::Query;
use crate::model::row::Row;
use crate::provider::worker::{WorkerCmd, WorkerMsg};
use crate::registry::Extension;
use crate::state;
use crate::support::rank;

impl Engine {
    // -------------------------------------------------------------- queries

    pub(super) async fn on_query(&mut self, text: &str) {
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

    pub(super) async fn on_close(&mut self) {
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

    // ------------------------------------------------------------- merging

    /// An inline provider answered: write its bucket and rebuild.
    pub(super) fn put_inline(&mut self, provider: &'static str, epoch: u64, rows: Vec<Row>) {
        self.buckets.insert(
            Arc::from(provider),
            (epoch, Arc::new(rows.into_iter().map(Arc::new).collect())),
        );
    }

    /// A worker answered: write its bucket, clear its wait. Returns whether a
    /// publish is owed — `run` batches a keystroke's burst and publishes once.
    pub(super) async fn handle_worker(&mut self, msg: WorkerMsg) -> bool {
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
            WorkerMsg::AskDone { model } => {
                // The task is over, so the handle it left behind is stale —
                // clearing it here is what lets the *next* question start
                // instead of queueing behind a stream that already ended.
                self.ask_task = None;
                // One completed turn on this model. The ledger counts turns,
                // the way jcode's does — never "was it useful", which we
                // cannot know from here.
                self.usage.record(&model, now_ms());
                state::usage::save(&crate::settings::paths::usage_file(), &self.usage);
                // A question typed while this one was being answered starts
                // now: the point of queueing it rather than cancelling.
                if let Some(next) = self.ask_pending.take() {
                    self.on_ask(&next).await;
                }
                false
            }
        }
    }

    /// Merge every bucket, apply frecency and pins, publish the result — and
    /// fire the Enter a placeholder was holding. `query` is the already-parsed
    /// question — publish runs once per provider answer, so re-parsing the
    /// same text each time would be per-keystroke waste.
    pub(super) async fn publish(&mut self, epoch: u64, query: &Query) {
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
    pub(super) fn scope_label(&self, query: &Query) -> String {
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
}
