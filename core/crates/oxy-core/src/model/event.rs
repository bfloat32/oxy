//! The engine's vocabulary: `EngineCmd` in, `EngineEvent` out.
//!
//! Everything arrives on one channel and everything leaves on another, so the
//! daemon's IPC loop is plumbing rather than logic. `engine` re-exports both
//! so the wire paths (`oxy_core::engine::EngineCmd`) stay stable.

use std::sync::Arc;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::model::row::{Action, Row};
use crate::provider::NativeExt;

/// What the client sends the engine.
pub enum EngineCmd {
    /// The launcher was summoned: the box's text, plus everything a summon
    /// re-sends — the registry for chips and the ask hint.
    Open { text: String },
    /// The text in the box, and whether the launcher is open.
    Query { text: String, opened: bool },
    /// What the clipboard held when the launcher opened (`wl-paste` output).
    Clipboard { url: Option<String> },
    /// The launcher closed: stop refreshes, keep caches.
    Close,
    /// Enter on a row. `action` picks a non-primary action; `shift`/`ctrl`
    /// mirror the frontend's modifier meanings.
    Activate {
        key: String,
        action: Option<usize>,
        shift: bool,
        ctrl: bool,
    },
    /// Ctrl+P on a row.
    Pin { key: String },
    /// The selection landed on a row: run its `previewExec` if it has one.
    Select { key: String },
    /// Leaving the launcher keeps whatever the preview changed.
    CommitPreview,
    /// A slider's `setExec` with the value substituted.
    Set { key: String, value: f64 },
    /// A form row's submission: extension id and its field values.
    SaveSettings {
        id: String,
        values: Map<String, Value>,
    },
    /// Reload extensions and settings from disk.
    Reload,
    /// Register a native provider for the extension id — the daemon injects
    /// what it compiled in.
    RegisterNative {
        ext_id: String,
        native: Box<dyn NativeExt>,
    },
    /// Ctrl+Enter: stream an `askProviders` answer for this question.
    Ask { text: String },
    /// Escape out of the answer panel: kill the stream.
    StopAsk,
    /// An action the row did not declare — a form's `exec` with its `{field}`
    /// tokens substituted, built by the frontend because only it held the
    /// answers. Boxed: it is the fat variant, and this enum crosses a channel
    /// on every client command.
    Act { key: String, action: Box<Action> },
    /// The event log asks for a line of its own (the frontend's own events).
    Log { ev: String, fields: Value },
    /// Liveness.
    Ping,
}

/// What the engine pushes back.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum EngineEvent {
    /// First contact: version, and the keywords the parser validates against.
    Hello {
        version: String,
        keywords: Vec<String>,
    },
    /// The merged list for the current epoch.
    Results {
        epoch: u64,
        /// Shared rows — a publish is refcount bumps, not a deep copy of
        /// every row the merged list carries.
        rows: Vec<Arc<Row>>,
        /// Provider ids still owed an answer.
        waiting: Vec<Arc<str>>,
        /// Provider ids whose visible rows are one or more keystrokes old.
        stale: Vec<Arc<str>>,
        scope: String,
        #[serde(rename = "scopeLabel")]
        scope_label: String,
        #[serde(rename = "helpMode")]
        help_mode: bool,
        #[serde(rename = "recentMode")]
        recent_mode: bool,
        /// The layout the first row asked for.
        view: String,
        /// The armed `confirm` prompt, while one is armed — the empty state
        /// draws it instead of "nothing matches".
        #[serde(default)]
        confirm: String,
    },
    /// Type this into the box (`fill`, follow-up `query`).
    Type {
        text: String,
        /// A step deeper into a flow: the shell pushes where it was onto its
        /// back-trail first. A `fill` is not a step, so it stays false.
        #[serde(default)]
        flow: bool,
        /// What the action set in motion lands later (a player starting, a
        /// daemon answering): the shell re-asks a few times so the list
        /// catches up, the way `followUpTimer` did.
        #[serde(default)]
        poll: bool,
    },
    /// A footer line that is not an answer ("Recent queries cleared").
    Notice {
        text: String,
    },
    /// The extension registry, for help/settings views and source chips —
    /// plus whether Ctrl+Enter has an `askProviders` command behind it.
    Registry {
        extensions: Vec<Value>,
        /// `{"available": bool, "model": title}` — the first provider whose
        /// `when` answered, probed once rather than per question.
        ask: Value,
    },
    /// A log line the daemon appends to the event file.
    Log {
        ev: String,
        fields: Value,
    },
    /// An `ask` run started: the question and which provider answered.
    AnswerStart {
        question: String,
        provider: String,
    },
    /// One line of a streaming `ask` answer.
    Answer {
        line: String,
    },
    /// The stream ended; `error` is why, when it failed before answering.
    AnswerDone {
        error: String,
    },
    Pong,
    /// The daemon should close the launcher after this.
    Close,
}

/// Provider ids whose rows are never frecency-recorded: their keys change
/// per keystroke, so recording them teaches nothing.
pub(crate) const NO_FRECENCY: &[&str] = &["calc", "web"];
