//! The built-ins `Settings::default` starts from — the engines and
//! `askProviders` a fresh `oxy.json` behaves as if it listed — plus the two
//! small readers shared by the merge.

use serde_json::Value;

use super::{AskProvider, Engine, LocalAsk};

/// The `ask` block's defaults: no endpoint, so the CLI list answers; and a
/// system prompt written for a launcher card rather than a terminal.
pub(super) fn default_local_ask() -> LocalAsk {
    LocalAsk {
        endpoint: String::new(),
        model: String::new(),
        system: "Answer briefly; this renders in a launcher card, not a terminal.".into(),
        max_tokens: 800,
        temperature: 0.4,
    }
}

pub(super) fn default_ask_providers() -> Vec<AskProvider> {
    [
        (
            "claude",
            "Claude",
            "sonnet",
            "claude -p --model {model} --effort medium {query}",
            "command -v claude",
        ),
        (
            "codex",
            "Codex",
            "",
            "codex exec --skip-git-repo-check {query}",
            "command -v codex",
        ),
        (
            "gemini",
            "Gemini",
            "",
            "gemini -p {query}",
            "command -v gemini",
        ),
        (
            "ollama",
            "Ollama",
            "llama3.2",
            "ollama run {model} {query}",
            "command -v ollama",
        ),
        (
            "aichat",
            "aichat",
            "",
            "aichat {query}",
            "command -v aichat",
        ),
        ("mods", "mods", "", "mods {query}", "command -v mods"),
    ]
    .iter()
    .map(|(id, title, model, command, when)| AskProvider {
        id: id.to_string(),
        title: title.to_string(),
        model: model.to_string(),
        command: command.to_string(),
        when: when.to_string(),
    })
    .collect()
}

pub(super) fn default_engines() -> Vec<Engine> {
    [
        ("google", "Google", "https://www.google.com/search?q={}"),
        ("ddg", "DuckDuckGo", "https://duckduckgo.com/?q={}"),
        ("chatgpt", "Ask ChatGPT", "https://chatgpt.com/?q={}"),
        (
            "perplexity",
            "Ask Perplexity",
            "https://www.perplexity.ai/search?q={}",
        ),
        (
            "youtube",
            "YouTube",
            "https://www.youtube.com/results?search_query={}",
        ),
        ("github", "GitHub", "https://github.com/search?q={}"),
    ]
    .iter()
    .map(|(id, title, url)| Engine {
        id: id.to_string(),
        title: title.to_string(),
        url: url.to_string(),
    })
    .collect()
}

pub(super) fn str_array(v: Option<&Value>) -> Vec<String> {
    v.and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// The percent-encoding a URL argument needs — `encodeURIComponent`.
pub fn url_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_encode_leaves_the_uri_mark_set() {
        assert_eq!(url_encode("a b&c=1"), "a%20b%26c%3D1");
        assert_eq!(url_encode("~-._*'()!"), "~-._*'()!");
    }
}
