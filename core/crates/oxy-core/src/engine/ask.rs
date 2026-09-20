//! The `ask` stream: Ctrl+Enter streams a question to the first
//! `askProviders` command whose `when` answers.

use serde_json::json;

use super::{Engine, EngineEvent};

impl Engine {
    // ----------------------------------------------------------------- ask

    /// Ctrl+Enter: the first `askProviders` entry whose `when` answers is run
    /// with the question, and its stdout streams back as `Answer` events.
    pub(super) async fn on_ask(&mut self, question: &str) {
        self.stop_ask();
        if question.trim().is_empty() {
            return;
        }

        // The probe ran at registry emit, once per settings load — a wrong
        // guess costs more than a hundred milliseconds, but probing on every
        // question costs it every time.
        if !self.ask_probed {
            self.probe_ask().await;
        }
        let spec = self.ask_provider.clone();

        self.emit(EngineEvent::AnswerStart {
            question: question.to_string(),
            provider: spec.as_ref().map(|p| p.title.clone()).unwrap_or_default(),
        })
        .await;

        let Some(spec) = spec else {
            self.emit(EngineEvent::AnswerDone {
                error: "No ask provider is installed".into(),
            })
            .await;
            return;
        };

        let command = spec
            .command
            .replace("{model}", &spec.model)
            .replace("{query}", &crate::support::quote::quote(question));
        // stdbuf so a line-buffered model actually streams; stderr folded in
        // so a real failure is visible rather than silent.
        let body = format!("stdbuf -oL {command} < /dev/null 2>&1");
        let Some(mut child) = crate::provider::process::spawn_stream(&body) else {
            self.emit(EngineEvent::AnswerDone {
                error: "Could not start the ask command".into(),
            })
            .await;
            return;
        };

        let evt = self.evt_tx.clone();
        self.ask_task = Some(tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt;
            let mut answered = false;
            if let Some(stdout) = child.stdout.take() {
                let mut lines = tokio::io::BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    answered = true;
                    if evt.send(EngineEvent::Answer { line }).await.is_err() {
                        return;
                    }
                }
            }
            let code = child.wait().await.ok().and_then(|s| s.code()).unwrap_or(-1);
            // An exit code only becomes the error when nothing else did —
            // a model that answered and then exited 1 still answered.
            let error = if code != 0 && !answered {
                format!("That command exited {code}. Check askProviders in oxy.json.")
            } else {
                String::new()
            };
            let _ = evt.send(EngineEvent::AnswerDone { error }).await;
        }));
    }

    /// Kill the stream, if one is running. Idempotent.
    pub(super) fn stop_ask(&mut self) {
        if let Some(task) = self.ask_task.take() {
            task.abort();
        }
    }

    /// The first `askProviders` entry whose `when` answers, in list order —
    /// `probeNextProvider`'s port. Runs once per settings load; the probes are
    /// serial because a wrong guess costs more than the wait does.
    async fn probe_ask(&mut self) {
        self.ask_probed = true;
        self.ask_provider = None;
        for provider in self.settings.ask_ordered() {
            if provider.when.is_empty() || crate::provider::process::check(&provider.when).await {
                self.ask_provider = Some(provider.clone());
                break;
            }
        }
    }

    /// The registry event, sent on every open transition and every reload:
    /// the frontend's source chips, keyword help and ask hint all read it.
    pub(super) async fn emit_registry(&mut self) {
        if !self.ask_probed {
            self.probe_ask().await;
        }
        let ask = match &self.ask_provider {
            Some(p) => json!({ "available": true, "model": p.title }),
            None => json!({ "available": false, "model": "" }),
        };
        self.emit(EngineEvent::Registry {
            extensions: self
                .extensions
                .iter()
                .map(|e| {
                    json!({
                        "id": e.id, "title": e.title, "keyword": e.keyword,
                        "aliases": e.aliases, "glyph": e.glyph, "accent": e.accent,
                        "view": e.view,
                    })
                })
                .collect(),
            ask,
        })
        .await;
    }
}
