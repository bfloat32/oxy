//! The `ask` stream: Ctrl+Enter streams a question to a local model when one
//! is configured, and to the first `askProviders` command whose `when`
//! answers when one is not.

use serde_json::json;

use super::{Engine, EngineEvent};
use crate::provider::llm::{Delta, Local};

impl Engine {
    // ----------------------------------------------------------------- ask

    /// Ctrl+Enter: a configured local model answers in-process, streaming its
    /// deltas back as `Answer` events; otherwise the first `askProviders`
    /// entry whose `when` answers is run with the question, and its stdout
    /// streams back the same way.
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
        if let Some(llm) = self.llm.clone() {
            self.ask_local(llm, question).await;
            return;
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

    /// The native path: one POST to the configured endpoint, deltas streamed
    /// back as they arrive. Deltas are buffered into whole lines because the
    /// wire's `answer` event is one line per event — the card appends a
    /// newline between them, so a token per event would render one word per
    /// line. Token-level framing is the design's next step, and it needs the
    /// frontend to change with it.
    async fn ask_local(&mut self, llm: Local, question: &str) {
        self.emit(EngineEvent::AnswerStart {
            question: question.to_string(),
            provider: llm.title(),
        })
        .await;

        let body = llm.chat_request(&[], question);
        let evt = self.evt_tx.clone();
        self.ask_task = Some(tokio::spawn(async move {
            let mut error = String::new();
            let mut pending = String::new();

            macro_rules! flush {
                ($force:expr) => {
                    while let Some(i) = pending.find('\n') {
                        let line: String = pending.drain(..=i).collect();
                        if evt
                            .send(EngineEvent::Answer {
                                line: line.trim_end_matches(['\n', '\r']).to_string(),
                            })
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    if $force && !pending.is_empty() {
                        if evt
                            .send(EngineEvent::Answer {
                                line: std::mem::take(&mut pending),
                            })
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                };
            }

            match crate::provider::llm::http::post_json(&llm.url, &body).await {
                Ok(mut response) if response.status == 200 => loop {
                    match response.next_line().await {
                        Ok(Some(line)) => match crate::provider::llm::stream::parse_line(&line) {
                            Some(Delta::Text(text)) => {
                                pending.push_str(&text);
                                flush!(false);
                            }
                            Some(Delta::Error(why)) => error = why,
                            Some(Delta::Done) => {
                                flush!(true);
                                break;
                            }
                            None => {}
                        },
                        Ok(None) => {
                            flush!(true);
                            break;
                        }
                        Err(e) => {
                            error = format!("The stream broke: {e}");
                            break;
                        }
                    }
                },
                Ok(response) => {
                    error = format!("{} answered {}.", llm.url.authority(), response.status);
                }
                Err(e) => {
                    error = format!(
                        "Could not reach {} ({e}). Is the model server running?",
                        llm.url.authority()
                    );
                }
            }
            // An error after some text is reported, not thrown away: the card
            // keeps what arrived and shows why it stopped.
            let _ = evt.send(EngineEvent::AnswerDone { error }).await;
        }));
    }

    /// The first `askProviders` entry whose `when` answers, in list order —
    /// `probeNextProvider`'s port. Runs once per settings load; the probes are
    /// serial because a wrong guess costs more than the wait does.
    ///
    /// A configured local model is resolved here too, and it short-circuits
    /// the list: the user named an endpoint, so probing four CLIs to ignore
    /// their answer would be four processes spent on nothing.
    async fn probe_ask(&mut self) {
        self.ask_probed = true;
        self.ask_provider = None;
        self.llm = Local::from_settings(&self.settings.ask);
        if self.llm.is_some() {
            return;
        }
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
        let ask = match (&self.llm, &self.ask_provider) {
            (Some(local), _) => json!({ "available": true, "model": local.title() }),
            (None, Some(p)) => json!({ "available": true, "model": p.title }),
            (None, None) => json!({ "available": false, "model": "" }),
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
