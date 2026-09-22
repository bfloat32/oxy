//! Persistence: settings writes, reload, state writes, and the event sink
//! every other module emits through.

use std::sync::Arc;

use serde_json::{Map, Value, json};

use super::builtins::{builtin_extensions, now_ms};
use super::{Engine, EngineEvent};
use crate::model::event::NO_FRECENCY;
use crate::model::row::Row;
use crate::settings::Settings;
use crate::state;

impl Engine {
    // ------------------------------------------------------------- settings

    /// The form's answers, written through the file's own text so nothing the
    /// user wrote is lost and nothing we defaulted to is recorded as chosen.
    pub(super) async fn on_save_settings(&mut self, id: &str, values: Map<String, Value>) {
        let path = crate::settings::paths::settings_file();
        // A missing file is a first save, not a reason to drop the form; a
        // file that does not parse is reported rather than half-rewritten;
        // and valid JSON that is not an object cannot hold settings at all —
        // `as_object_mut().unwrap()` on it used to take the daemon down.
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let trailing_newline = text.is_empty() || text.ends_with('\n');
        let mut raw = match serde_json::from_str::<Value>(&text) {
            Ok(v) if v.is_object() => v,
            Ok(_) => {
                self.emit(EngineEvent::Notice {
                    text: "oxy.json is not an object — the save was not written".into(),
                })
                .await;
                return;
            }
            Err(_) if text.trim().is_empty() => json!({}),
            Err(_) => {
                self.emit(EngineEvent::Notice {
                    text: "oxy.json does not parse — the save was not written".into(),
                })
                .await;
                return;
            }
        };
        let Some(obj) = raw.as_object_mut() else {
            return;
        };
        let entry = obj
            .entry("extensionSettings".to_string())
            .or_insert_with(|| json!({}));
        if !entry.is_object() {
            *entry = json!({});
        }
        entry
            .as_object_mut()
            .unwrap()
            .insert(id.to_string(), Value::Object(values));
        let mut text = serde_json::to_string_pretty(&raw).unwrap_or_default();
        // Keep the file's ending: a trailing newline stays, its absence
        // stays absent — the write changes one subtree, not the file shape.
        if trailing_newline && !text.ends_with('\n') {
            text.push('\n');
        }
        // Atomic, the way every file this launcher owns is written — and a
        // per-writer tmp name, since a second engine (`oxy test --local`)
        // can write the same file.
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        if std::fs::write(&tmp, &text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
        self.settings = Settings::load(&path);
        *self.shared.settings.write().await = Arc::new(self.settings.clone());
        // Back to the list, on the poll cadence: the write above re-reads
        // the file here, but the providers reading it next need the passes.
        self.emit(EngineEvent::Type {
            text: "settings:".into(),
            flow: true,
            poll: true,
        })
        .await;
    }

    // -------------------------------------------------------------- reload

    pub(super) async fn on_reload(&mut self) {
        self.settings = Settings::load(&crate::settings::paths::settings_file());
        for path in self.settings.recovered.clone() {
            self.emit_log("settings.recovered", json!({ "f": path.to_string_lossy() }))
                .await;
        }
        *self.shared.settings.write().await = Arc::new(self.settings.clone());
        let load_t0 = std::time::Instant::now();
        let report = crate::registry::load_dir(
            &crate::settings::paths::extensions_dir(),
            &self.settings.extensions,
        );
        self.emit_log(
            "ext.load",
            json!({
                "n": report.extensions.len(),
                "ms": load_t0.elapsed().as_millis() as u64,
            }),
        )
        .await;
        for (path, why) in &report.bad {
            self.emit_log(
                "ext.bad",
                json!({ "f": path.to_string_lossy(), "why": why }),
            )
            .await;
        }
        let mut extensions = report.extensions;
        extensions.extend(builtin_extensions(&self.settings.quicklinks));
        self.extensions = Arc::new(extensions);
        *self.shared.registry.write().await = self.extensions.clone();
        self.rebuild_known();
        // Reconcile workers with the new definitions: arrivals spawn,
        // departures and redefinitions are shut down and respawned — the
        // cache survives in `Shared`, keyed by command, so an unchanged
        // answer still hits.
        self.spawn_workers(&self.extensions.clone()).await;
        // The ask probe's answer may have changed with the settings.
        self.ask_probed = false;
        self.emit_registry().await;
        self.on_query(&self.raw.clone()).await;
    }

    // --------------------------------------------------------------- state

    /// Rows whose key changes on every keystroke teach nothing: a calculator
    /// answer is keyed by its expression and a web search by its terms.
    pub(super) fn remember(&mut self, row: &Row) {
        if !self.settings.frecency || row.key.is_empty() {
            return;
        }
        if NO_FRECENCY.contains(&row.provider_id.as_str()) {
            return;
        }
        state::frecency_record(&mut self.state.frecency, &row.key, now_ms(), &self.raw);
        self.save_state();
    }

    /// A provider-declared recency write: `"remember": {"file", "value"}`
    /// on the row or the action. Runs before the exec it rides with, the way
    /// `oxy-emoji --used` ran before the copy.
    pub(super) fn remember_mru(&self, extra: &serde_json::Map<String, Value>) {
        let Some(r) = extra.get("remember").and_then(|v| v.as_object()) else {
            return;
        };
        let (Some(file), Some(value)) = (
            r.get("file").and_then(|v| v.as_str()),
            r.get("value").and_then(|v| v.as_str()),
        ) else {
            return;
        };
        let keep = r.get("keep").and_then(|v| v.as_u64()).unwrap_or(24) as usize;
        let dir = self
            .state_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| crate::settings::paths::state_home().join("omarchy"));
        state::mru_record(&dir, file, value, keep);
    }

    /// The question, not the answer.
    pub(super) fn remember_query(&mut self) {
        if !self.settings.recents {
            return;
        }
        // A `/` action is an instruction, not a question.
        if self.raw.trim_start().starts_with('/') {
            return;
        }
        state::recents_record(&mut self.state.recents, &self.raw);
        self.save_state();
    }

    pub(super) fn save_state(&mut self) {
        // What decayed to nothing goes now, on the same cadence the QML used:
        // the file holds only keys that still mean a launch.
        crate::state::frecency_prune(&mut self.state.frecency, now_ms());
        let _ = self.state.save(&self.frecency_path, &self.state_path);
    }

    // --------------------------------------------------------------- emit

    pub(super) async fn emit(&self, event: EngineEvent) {
        let _ = self.evt_tx.send(event).await;
    }

    pub(super) async fn emit_log(&self, ev: impl Into<String>, fields: Value) {
        if self.settings.log {
            let _ = self
                .evt_tx
                .send(EngineEvent::Log {
                    ev: ev.into(),
                    fields,
                })
                .await;
        }
    }
}
