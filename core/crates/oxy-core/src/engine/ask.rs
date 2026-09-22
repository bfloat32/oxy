//! The `ask` stream: Ctrl+Enter streams a question to a local model when one
//! is configured, and to the first `askProviders` command whose `when`
//! answers when one is not.

use serde_json::json;

use super::{Engine, EngineEvent};
use crate::provider::llm::Local;
use crate::provider::worker::WorkerMsg;

impl Engine {
    // ----------------------------------------------------------------- ask

    /// Ctrl+Enter: a configured local model answers in-process, streaming its
    /// deltas back as `Answer` events; otherwise the first `askProviders`
    /// entry whose `when` answers is run with the question, and its stdout
    /// streams back the same way.
    pub(super) async fn on_ask(&mut self, question: &str) {
        if question.trim().is_empty() {
            return;
        }
        // A turn is already streaming: keep this question for the moment it
        // ends rather than cancelling it — the answer that was already
        // arriving is the thing worth keeping. Escape, and `stopask`, still
        // cancel on purpose.
        if self.ask_task.is_some() {
            self.ask_pending = Some(question.to_string());
            return;
        }
        self.stop_ask();

        // The probe ran at registry emit, once per settings load — a wrong
        // guess costs more than a hundred milliseconds, but probing on every
        // question costs it every time.
        if !self.ask_probed {
            self.probe_ask().await;
        }
        // A local endpoint that is not listening at all falls back to the CLI
        // list; one that answers badly does not — its error belongs in the
        // card, not hidden behind a second provider's answer. The probe is
        // bounded (250ms) and only runs when an endpoint is configured.
        let mut fell_back = None;
        if let Some(llm) = self.llm.clone() {
            if llm.probe().await || !self.probe_cli_ask().await {
                self.ask_local(llm, question).await;
                return;
            }
            fell_back = Some(format!("nothing is listening on {}", llm.url.authority()));
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
        // After the start, which clears the card: the line has to survive the
        // reset to be worth saying.
        if let Some(why) = fell_back {
            self.emit(EngineEvent::Answer {
                line: format!("· {why} — {} answers instead", spec.title),
            })
            .await;
        }

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
        let worker_tx = self.worker_tx.clone();
        let title = spec.title.clone();
        self.ask_task = Some(tokio::spawn(async move {
            let mut answered = false;
            if let Some(stdout) = child.stdout.take() {
                // The same bounded reader the sockets use: a provider that
                // streams one giant line cannot grow the buffer without end.
                let mut reader = tokio::io::BufReader::new(stdout);
                let mut buf = Vec::with_capacity(4096);
                while let Ok(Some(line)) = crate::support::lines::next(
                    &mut reader,
                    &mut buf,
                    crate::support::lines::MAX_LINE,
                )
                .await
                {
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
            // The engine counts the turn and starts whatever was typed while
            // this one was talking.
            let _ = worker_tx.send(WorkerMsg::AskDone { model: title }).await;
        }));
    }

    /// Kill the stream, if one is running — and the question queued behind
    /// it. Escape means stop asking, not "stop this one and start the next
    /// one anyway": a queued question the user dismissed must not fire when
    /// a later turn ends. Idempotent.
    pub(super) fn stop_ask(&mut self) {
        if let Some(task) = self.ask_task.take() {
            task.abort();
        }
        self.ask_pending = None;
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

        let title = llm.title();
        let mut pieces = crate::provider::llm::turn::spawn(llm, Vec::new(), question.to_string());
        let evt = self.evt_tx.clone();
        let worker_tx = self.worker_tx.clone();
        self.ask_task = Some(tokio::spawn(async move {
            // Deltas are buffered into whole lines because the wire's `answer`
            // event is one line per event — the card appends a newline
            // between them, so a token per event would render one word per
            // line. Token-level framing is the design's next step, and it
            // needs the frontend to change with it.
            let mut pending = String::new();
            let mut error = String::new();
            while let Some(piece) = pieces.recv().await {
                match piece {
                    crate::provider::llm::turn::Piece::Text(text) => {
                        pending.push_str(&text);
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
                    }
                    crate::provider::llm::turn::Piece::Notice(line) => {
                        if evt.send(EngineEvent::Answer { line }).await.is_err() {
                            return;
                        }
                    }
                    crate::provider::llm::turn::Piece::Error(why) => {
                        error = why;
                        break;
                    }
                }
            }
            // Whatever is left of the last line is a line too.
            if !pending.is_empty()
                && evt
                    .send(EngineEvent::Answer {
                        line: std::mem::take(&mut pending),
                    })
                    .await
                    .is_err()
            {
                return;
            }
            // An error after some text is reported, not thrown away: the card
            // keeps what arrived and shows why it stopped.
            let _ = evt.send(EngineEvent::AnswerDone { error }).await;
            let _ = worker_tx.send(WorkerMsg::AskDone { model: title }).await;
        }));
    }

    /// The CLI list, probed lazily — for a fallback, not at load. A machine
    /// with an endpoint configured pays nothing until the endpoint is found
    /// missing, and then pays it once.
    async fn probe_cli_ask(&mut self) -> bool {
        if self.ask_provider.is_some() {
            return true;
        }
        self.ask_provider = self.first_available_provider().await;
        self.ask_provider.is_some()
    }

    /// The first `askProviders` entry whose `when` answers, in list order.
    /// The probes run together — each `when` is a bounded subprocess and a
    /// row of absent CLIs costs the slowest one instead of the sum, which
    /// the engine loop would otherwise sit through on first open.
    async fn first_available_provider(&self) -> Option<crate::settings::AskProvider> {
        let providers: Vec<crate::settings::AskProvider> =
            self.settings.ask_ordered().into_iter().cloned().collect();
        let mut probes = tokio::task::JoinSet::new();
        for (i, p) in providers.iter().enumerate() {
            let when = p.when.clone();
            probes.spawn(async move {
                (
                    i,
                    when.is_empty() || crate::provider::process::check(&when).await,
                )
            });
        }
        let mut ok = vec![false; providers.len()];
        while let Some(res) = probes.join_next().await {
            if let Ok((i, pass)) = res {
                ok[i] = pass;
            }
        }
        providers
            .into_iter()
            .zip(ok)
            .find(|(_, pass)| *pass)
            .map(|(p, _)| p)
    }

    /// The first `askProviders` entry whose `when` answers, in list order —
    /// `probeNextProvider`'s port. Runs once per settings load.
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
        self.ask_provider = self.first_available_provider().await;
    }

    /// The registry event, sent on every open transition and every reload:
    /// the frontend's source chips, keyword help and ask hint all read it.
    pub(super) async fn emit_registry(&mut self) {
        if !self.ask_probed {
            self.probe_ask().await;
        }
        // `hint` is what the footer says when nothing is configured: the
        // absence of a provider is a fact, and the useful half of it is what
        // to do about it.
        let ask = match (&self.llm, &self.ask_provider) {
            (Some(local), _) => json!({ "available": true, "model": local.title(), "hint": "" }),
            (None, Some(p)) => json!({ "available": true, "model": p.title, "hint": "" }),
            (None, None) => json!({
                "available": false,
                "model": "",
                "hint": "set ask.endpoint in oxy.json, or install claude, codex, gemini or ollama",
            }),
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
