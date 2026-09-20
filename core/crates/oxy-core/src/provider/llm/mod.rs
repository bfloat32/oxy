//! Local models, spoken to directly.
//!
//! The launcher has always been able to ask a model through a CLI
//! (`askProviders`, one process per question, stdout line by line). This is
//! the other half: an HTTP client for a model server already running on this
//! machine — ollama, LM Studio, llama.cpp — with no process spawn, no shell
//! and no `stdbuf` in between. It is the first slice of the `ask:` design in
//! `docs/LLM-INTEGRATION.md`: the api-kind transport, local-first.
//!
//! Everything here is loopback-only on purpose. `http://` and nothing else,
//! because the endpoint is the user's configuration and a launcher should not
//! be the thing that quietly sends a question to a remote host in plaintext.

pub mod http;
pub mod models;
pub mod retry;
pub mod stream;
pub mod turn;

pub use http::Url;
pub use stream::Delta;

use serde_json::{Value, json};

use crate::settings::LocalAsk;

/// A configured local endpoint, ready to answer a question.
#[derive(Debug, Clone)]
pub struct Local {
    pub url: Url,
    pub model: String,
    pub system: String,
    pub max_tokens: u64,
    pub temperature: f64,
    /// Resolved at load: `env:NAME` reads the variable, anything else is the
    /// literal. Empty means no header is sent.
    pub key: String,
}

/// `env:NAME` → the variable's value; anything else is the literal.
///
/// An empty variable is an empty key, and an empty key sends no header — the
/// same as not setting one. Saying so is the doctor's job, not this one's.
pub fn resolve_key(raw: &str) -> String {
    match raw.trim().strip_prefix("env:") {
        Some(name) => std::env::var(name.trim()).unwrap_or_default(),
        None => raw.trim().to_string(),
    }
}

impl Local {
    /// Resolve the `ask` block. `None` — the CLI list answers, as before —
    /// when there is no endpoint, or when it is not a plain-HTTP URL.
    pub fn from_settings(ask: &LocalAsk) -> Option<Local> {
        if ask.endpoint.trim().is_empty() {
            return None;
        }
        let url = Url::parse(&ask.endpoint)?;
        Some(Local {
            url,
            model: ask.model.clone(),
            system: ask.system.clone(),
            max_tokens: ask.max_tokens,
            temperature: ask.temperature,
            key: resolve_key(&ask.key),
        })
    }

    /// What the registry chip and the card's header say. The model name is
    /// the useful half; the address is there so two servers can be told
    /// apart.
    pub fn title(&self) -> String {
        if self.model.is_empty() {
            format!("Local · {}", self.url.authority())
        } else {
            format!("Local · {}", self.model)
        }
    }

    /// The request body: the OpenAI chat shape every local server accepts,
    /// with `messages[]` as the context so a later turn can replay the ones
    /// before it. `history` is `(user, assistant)` pairs, oldest first.
    pub fn chat_request(&self, history: &[(String, String)], question: &str) -> String {
        let mut messages: Vec<Value> = Vec::with_capacity(history.len() * 2 + 2);
        if !self.system.is_empty() {
            messages.push(json!({ "role": "system", "content": self.system }));
        }
        for (user, assistant) in history {
            messages.push(json!({ "role": "user", "content": user }));
            messages.push(json!({ "role": "assistant", "content": assistant }));
        }
        messages.push(json!({ "role": "user", "content": question }));

        json!({
            "model": self.model,
            "messages": messages,
            "stream": true,
            "temperature": self.temperature,
            "max_tokens": self.max_tokens,
        })
        .to_string()
    }

    /// Whether anything is listening. A connect, not a request: a model
    /// server that is up but still loading a model should not read as down.
    pub async fn probe(&self) -> bool {
        let timeout = std::time::Duration::from_millis(250);
        matches!(
            tokio::time::timeout(
                timeout,
                tokio::net::TcpStream::connect((self.url.host.as_str(), self.url.port))
            )
            .await,
            Ok(Ok(_))
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(endpoint: &str) -> LocalAsk {
        LocalAsk {
            endpoint: endpoint.to_string(),
            model: "llama3.2".into(),
            system: "Answer briefly.".into(),
            max_tokens: 800,
            temperature: 0.4,
            key: String::new(),
        }
    }

    #[test]
    fn a_key_is_a_literal_or_an_environment_lookup() {
        assert_eq!(resolve_key("sk-abc"), "sk-abc");
        assert_eq!(resolve_key("  spaced  "), "spaced");
        assert_eq!(resolve_key(""), "");
        // `env:NAME` reads the variable; PATH is the one that always exists.
        assert_eq!(
            resolve_key("env:PATH"),
            std::env::var("PATH").unwrap_or_default()
        );
        assert!(!resolve_key("env:PATH").is_empty());
        assert_eq!(resolve_key("env:OXY_SURELY_NOT_SET_XYZ"), "");
    }

    #[test]
    fn no_endpoint_means_the_cli_list_answers() {
        assert!(Local::from_settings(&ask("")).is_none());
        assert!(Local::from_settings(&ask("   ")).is_none());
        // A URL we would have to send plaintext to somewhere else is refused
        // rather than half-supported.
        assert!(Local::from_settings(&ask("https://api.example.com/v1/chat")).is_none());
        assert!(Local::from_settings(&ask("127.0.0.1:11434")).is_none());
    }

    #[test]
    fn a_local_endpoint_resolves_and_names_itself() {
        let local =
            Local::from_settings(&ask("http://127.0.0.1:11434/v1/chat/completions")).unwrap();
        assert_eq!(local.url.port, 11434);
        assert_eq!(local.title(), "Local · llama3.2");

        let mut nameless = ask("http://localhost:1234/v1/chat/completions");
        nameless.model = String::new();
        let local = Local::from_settings(&nameless).unwrap();
        assert_eq!(local.title(), "Local · localhost:1234");
    }

    #[test]
    fn the_request_is_the_openai_chat_shape() {
        let local =
            Local::from_settings(&ask("http://127.0.0.1:11434/v1/chat/completions")).unwrap();
        let body: Value = serde_json::from_str(&local.chat_request(&[], "what is 2+2?")).unwrap();
        assert_eq!(body["model"], "llama3.2");
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_tokens"], 800);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "what is 2+2?");
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn history_replays_before_the_new_question() {
        let local =
            Local::from_settings(&ask("http://127.0.0.1:11434/v1/chat/completions")).unwrap();
        let history = vec![("first".to_string(), "one".to_string())];
        let body: Value = serde_json::from_str(&local.chat_request(&history, "second")).unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[1]["content"], "first");
        assert_eq!(messages[2]["content"], "one");
        assert_eq!(messages[3]["content"], "second");
    }

    #[test]
    fn an_empty_system_prompt_is_left_out() {
        let mut bare = ask("http://127.0.0.1:11434/v1/chat/completions");
        bare.system = String::new();
        let local = Local::from_settings(&bare).unwrap();
        let body: Value = serde_json::from_str(&local.chat_request(&[], "hi")).unwrap();
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    }
}
