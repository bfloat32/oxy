//! Activation: Enter on a row, the action panel, pins, previews, sliders.

use std::sync::Arc;
use std::time::Instant;

use serde_json::json;

use super::{Engine, EngineEvent};
use crate::model::row::{Action, Row};
use crate::state;

impl Engine {
    // ------------------------------------------------------------ activating

    pub(super) fn find_row(&self, key: &str) -> Option<Arc<Row>> {
        self.rows.iter().find(|r| r.key == key).cloned()
    }

    pub(super) async fn on_activate(
        &mut self,
        key: &str,
        action_index: Option<usize>,
        shift: bool,
        ctrl: bool,
        depth: u8,
    ) {
        // Shift+Enter is the second action, by keystroke rather than through
        // the panel — "ask ChatGPT" instead of Google.
        let action_index = action_index.or(shift.then_some(1));
        // An empty key is Enter on a list that has not caught up: it resolves
        // to whatever leads once something does.
        let key = if key.is_empty() {
            match self.rows.iter().find(|r| !r.pending) {
                Some(row) => row.key.clone(),
                None => {
                    self.pending_activate = Some((String::new(), action_index, Instant::now()));
                    return;
                }
            }
        } else {
            key.to_string()
        };
        let Some(row) = self.find_row(&key) else {
            // Enter with nothing under it is held, not dropped: the answer may
            // be one debounce away.
            self.pending_activate = Some((key, action_index, Instant::now()));
            return;
        };

        // Enter on a stale row ran the query the row was built from rather
        // than the one on screen. Held instead, and run when the answer lands.
        if let Some((ep, rows)) = self.buckets.get(row.provider_id.as_str())
            && *ep != self.epoch
            && !rows.is_empty()
        {
            self.pending_activate = Some((key, action_index, Instant::now()));
            return;
        }

        // A keyword from `?`, or a query you ran before: the point of picking
        // one is to carry on typing, so nothing runs and nothing closes.
        if !row.fill.is_empty() {
            self.emit(EngineEvent::Type {
                text: row.fill.clone(),
                flow: false,
                poll: false,
            })
            .await;
            return;
        }

        // Ctrl+C copies; Ctrl+Enter asks. Both are frontend keys in the old
        // launcher, surfaced here as modifiers on activate.
        if ctrl {
            let text = row
                .extra
                .get("copyText")
                .and_then(|v| v.as_str())
                .map(String::from)
                .or_else(|| {
                    if row.detail.is_empty() {
                        Some(row.title.clone())
                    } else {
                        Some(row.detail.clone())
                    }
                })
                .unwrap_or_default();
            if !text.is_empty() {
                crate::provider::process::run_detached(&format!(
                    "printf %s {} | wl-copy",
                    crate::support::quote::quote(&text)
                ));
                self.emit(EngineEvent::Notice {
                    text: "Copied".into(),
                })
                .await;
            }
            return;
        }

        let action = action_index
            .and_then(|i| row.actions.as_ref().and_then(|a| a.get(i)).cloned())
            .or_else(|| row.actions.as_ref().and_then(|a| a.first()).cloned());

        if let Some(action) = action {
            return self.run_action(&row, &action, depth).await;
        }

        if row.pending {
            self.pending_activate = Some((key, action_index, Instant::now()));
            return;
        }

        // A normal row: record, preview-commit, run, close.
        self.remember(&row);
        self.remember_query();
        self.remember_mru(&row.extra);
        self.emit_log(
            "act",
            json!({ "ep": self.epoch, "id": row.key, "src": row.provider_id, "t": row.title }),
        )
        .await;
        self.previewed = None;
        self.preview_revert.clear();

        // A row that behaves like a chat: Enter sends, and the box is emptied
        // for the next thing while what was sent stays on the card.
        if row
            .extra
            .get("keepOpen")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            if !row.exec.is_empty() {
                crate::provider::process::run_detached(&row.exec);
            }
            if let Some(clear) = row.extra.get("clearTo").and_then(|v| v.as_str()) {
                self.emit(EngineEvent::Type {
                    text: clear.to_string(),
                    flow: false,
                    poll: false,
                })
                .await;
            }
            return;
        }

        // Close first: launching while an exclusive-focus layer surface is
        // still mapped puts the new window behind it, and Omarchy's launch
        // OSD would render underneath this overlay. The socket write is
        // ordered, so the unmap starts at worst one frame before the spawn.
        self.emit(EngineEvent::Close).await;
        if !row.exec.is_empty() {
            crate::provider::process::run_detached(&row.exec);
        }
    }

    /// An action on a row: the panel's choice, or Enter on a row that carries
    /// one.
    pub(super) async fn run_action(&mut self, row: &Row, action: &Action, depth: u8) {
        // A confirmation asks in the box: a dialog would take the keyboard
        // from an overlay that holds it exclusively.
        if !action.confirm.is_empty() && self.pending_confirm.as_deref() != Some(action.id.as_str())
        {
            self.pending_confirm = Some(action.id.clone());
            let text = format!("/{} ", action.id);
            self.emit(EngineEvent::Type {
                text: text.clone(),
                flow: false,
                poll: false,
            })
            .await;
            // Re-ask the text the box will hold, not the text that fired the
            // action: on_query's walking-away rule clears an arm whose text
            // differs, and a client that ignores `type` still sees the prompt.
            self.on_query(&text).await;
            return;
        }
        self.pending_confirm = None;
        self.emit_log("action", json!({ "id": action.id, "ep": self.epoch }))
            .await;

        // Effects the engine performs itself; anything with `exec` instead is
        // a shell command, which is how an extension adds one.
        match action.effect.as_str() {
            "clear.recents" => {
                self.state.recents.clear();
                self.save_state();
                self.emit(EngineEvent::Notice {
                    text: "Recent queries cleared".into(),
                })
                .await;
                self.on_query(&self.raw.clone()).await;
                return;
            }
            "clear.pins" => {
                self.state.pins.clear();
                self.save_state();
                self.emit(EngineEvent::Notice {
                    text: "Pins cleared".into(),
                })
                .await;
                self.on_query(&self.raw.clone()).await;
                return;
            }
            "clear.all" => {
                self.state.recents.clear();
                self.state.pins.clear();
                self.save_state();
                self.emit(EngineEvent::Notice {
                    text: "History and pins cleared".into(),
                })
                .await;
                self.on_query(&self.raw.clone()).await;
                return;
            }
            "reload.extensions" => {
                self.on_reload().await;
                self.emit(EngineEvent::Notice {
                    text: "Extensions reloaded".into(),
                })
                .await;
                return;
            }
            "open.settings" => {
                self.emit(EngineEvent::Type {
                    text: "settings:".into(),
                    flow: false,
                    poll: false,
                })
                .await;
                return;
            }
            "stats" => {
                let cached = self
                    .shared
                    .cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .entries();
                let checks = self
                    .shared
                    .availability
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .stats();
                let usage = self.usage.summary();
                let mut text = format!(
                    "{} extensions · {} answers cached · {} checks, {} ok",
                    self.extensions.len(),
                    cached,
                    checks.0,
                    checks.1
                );
                // Which model answers, and how often — absent until a turn
                // has finished, rather than a zero that means nothing.
                if !usage.is_empty() {
                    text.push_str(&format!(" · ask: {usage}"));
                }
                // The worst three, from the answers that have run: a provider
                // that takes a second is the usual reason a box feels slow.
                let slow = super::slowest(&self.latency, 3);
                if !slow.is_empty() {
                    let parts: Vec<String> = slow
                        .iter()
                        .map(|(id, l)| format!("{id} {}ms", l.max_ms))
                        .collect();
                    text.push_str(&format!(" · slowest: {}", parts.join(", ")));
                }
                self.emit(EngineEvent::Notice { text }).await;
                return;
            }
            _ => {}
        }

        let follow_up = action
            .extra
            .get("query")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_default();
        let stay_open = action.keep_open;

        self.remember(row);
        self.remember_query();
        self.remember_mru(&action.extra);

        if let Some(row_key) = action.extra.get("row").and_then(|v| v.as_str()) {
            // A self-reference: activate the row it names — bounded, because
            // two actions naming each other would recurse until the stack
            // ran out.
            if depth >= 8 {
                self.emit_log(
                    "act.chain",
                    json!({ "row": row_key, "note": "self-reference chain exceeded 8 hops" }),
                )
                .await;
                return;
            }
            let key = row_key.to_string();
            return Box::pin(self.on_activate(&key, None, false, false, depth + 1)).await;
        }

        if !action.exec.is_empty() {
            let exec = action.exec.clone();
            if follow_up.is_empty() && !stay_open {
                self.emit(EngineEvent::Close).await;
            }
            crate::provider::process::run_detached(&exec);
        }

        if !follow_up.is_empty() {
            // An exec behind the follow-up lands whenever it lands — a player
            // starting, a daemon answering — so the box is typed now and the
            // query re-asked a few times while it catches up. A follow-up with
            // nothing running is just somewhere to go.
            self.emit(EngineEvent::Type {
                text: follow_up,
                flow: true,
                poll: !action.exec.is_empty(),
            })
            .await;
        } else if stay_open {
            // The action changed something the current answer shows: re-ask
            // on the poll cadence rather than once, so a slow effect still
            // arrives.
            self.emit(EngineEvent::Type {
                text: self.raw.clone(),
                flow: false,
                poll: true,
            })
            .await;
        }
        // An action carrying nothing else does nothing; the launcher stays put.
    }

    // ---------------------------------------------------------------- pins

    pub(super) async fn on_pin(&mut self, key: &str) {
        let Some(row) = self.find_row(key) else {
            return;
        };
        state::pin_toggle(&mut self.state.pins, &row.key);
        self.save_state();
        self.emit(EngineEvent::Notice {
            text: if state::pin_has(&self.state.pins, &row.key) {
                "Pinned".into()
            } else {
                "Unpinned".into()
            },
        })
        .await;
        let epoch = self.epoch;
        let query = self.query.clone();
        self.publish(epoch, &query).await;
    }

    // ------------------------------------------------------------- preview

    pub(super) fn on_select(&mut self, key: &str) {
        let Some(row) = self.find_row(key) else {
            return;
        };
        if row.preview_exec.is_empty() {
            return;
        }
        // Landing on the same row again does not run it twice. Moving to a
        // different preview does not undo the last one — a preview chain is
        // undone once, from the state you arrived in.
        if self.previewed.as_deref() == Some(row.key.as_str()) {
            return;
        }
        if self.preview_revert.is_empty() && !row.revert_exec.is_empty() {
            self.preview_revert = row.revert_exec.clone();
        }
        crate::provider::process::run_detached(&row.preview_exec);
        self.previewed = Some(row.key.clone());
    }

    pub(super) fn unpreview(&mut self) {
        self.previewed = None;
        if !self.preview_revert.is_empty() {
            let revert = std::mem::take(&mut self.preview_revert);
            crate::provider::process::run_detached(&revert);
        }
    }

    // ----------------------------------------------------------------- set

    pub(super) async fn on_set(&mut self, key: &str, value: f64) {
        let Some(row) = self.find_row(key) else {
            return;
        };
        if row.set_exec.is_empty() {
            return;
        }
        let rendered = row.set_exec.replace("{value}", &format!("{value}"));
        crate::provider::process::run_detached(&rendered);
        // Sliders keep the launcher open; the next refresh draws the truth.
    }
}
