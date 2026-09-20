//! One line of a streamed answer, whatever shape the server sent it in.
//!
//! Every local server speaks "OpenAI-compatible" slightly differently: the
//! `/v1/chat/completions` endpoint of ollama, LM Studio and llama.cpp all use
//! `choices[0].delta.content` over SSE, ollama's own `/api/chat` sends bare
//! NDJSON with `message.content`, and llama.cpp's `/completion` sends
//! `content`. Parsing is deliberately tolerant — an unrecognised line is
//! skipped, never an error — because the alternative is a blank card for a
//! server that answered perfectly well in a shape we did not anticipate.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delta {
    /// A piece of the answer, to be appended to the card.
    Text(String),
    /// The server said the turn is over.
    Done,
    /// The server reported a failure inside the stream.
    Error(String),
}

/// One line of the body → what it means, or nothing at all.
pub fn parse_line(line: &str) -> Option<Delta> {
    let line = line.trim();
    // SSE comments (`: ping`) and blank separators carry nothing.
    let payload = match line.strip_prefix("data:") {
        Some(rest) => rest.trim(),
        None => line,
    };
    if payload.is_empty() || payload.starts_with(':') {
        return None;
    }
    if payload == "[DONE]" {
        return Some(Delta::Done);
    }
    let json: Value = serde_json::from_str(payload).ok()?;

    if let Some(err) = json.get("error") {
        let text = err
            .as_str()
            .map(str::to_string)
            .or_else(|| {
                err.get("message")
                    .and_then(|m| m.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| err.to_string());
        return Some(Delta::Error(text));
    }

    // OpenAI shape: the delta carries the piece, and a finish_reason ends it.
    if let Some(choice) = json.get("choices").and_then(|c| c.get(0)) {
        let text = choice
            .get("delta")
            .and_then(|d| d.get("content"))
            .or_else(|| choice.get("message").and_then(|m| m.get("content")))
            .or_else(|| choice.get("text"))
            .and_then(Value::as_str);
        if let Some(text) = text
            && !text.is_empty()
        {
            return Some(Delta::Text(text.to_string()));
        }
        if choice.get("finish_reason").map(Value::is_null) == Some(false) {
            return Some(Delta::Done);
        }
        return None;
    }

    // ollama's own shape: `message.content`, and `done` as a real boolean.
    if let Some(text) = json
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
    {
        if json.get("done").and_then(Value::as_bool) == Some(true) {
            return if text.is_empty() {
                Some(Delta::Done)
            } else {
                Some(Delta::Text(text.to_string()))
            };
        }
        if !text.is_empty() {
            return Some(Delta::Text(text.to_string()));
        }
        return None;
    }
    if json.get("done").and_then(Value::as_bool) == Some(true) {
        return Some(Delta::Done);
    }

    // llama.cpp's `/completion`, and anything else that just calls it content.
    for key in ["content", "response"] {
        if let Some(text) = json.get(key).and_then(Value::as_str)
            && !text.is_empty()
        {
            return Some(Delta::Text(text.to_string()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_sse_deltas() {
        assert_eq!(
            parse_line(r#"data: {"choices":[{"delta":{"content":"Hel"}}]}"#),
            Some(Delta::Text("Hel".into()))
        );
        assert_eq!(
            parse_line(r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#),
            Some(Delta::Done)
        );
        assert_eq!(parse_line("data: [DONE]"), Some(Delta::Done));
        // A whole message instead of a delta still reads.
        assert_eq!(
            parse_line(r#"data: {"choices":[{"message":{"content":"hi"}}]}"#),
            Some(Delta::Text("hi".into()))
        );
    }

    #[test]
    fn ollama_native_ndjson() {
        assert_eq!(
            parse_line(r#"{"message":{"role":"assistant","content":"a"},"done":false}"#),
            Some(Delta::Text("a".into()))
        );
        assert_eq!(
            parse_line(r#"{"message":{"content":""},"done":true}"#),
            Some(Delta::Done)
        );
        assert_eq!(parse_line(r#"{"done":true}"#), Some(Delta::Done));
        // The generate endpoint's shape, which has no `choices` at all.
        assert_eq!(
            parse_line(r#"{"response":"x","done":false}"#),
            Some(Delta::Text("x".into()))
        );
    }

    #[test]
    fn errors_and_noise() {
        assert_eq!(
            parse_line(r#"data: {"error":{"message":"model not found"}}"#),
            Some(Delta::Error("model not found".into()))
        );
        assert_eq!(
            parse_line(r#"{"error":"out of memory"}"#),
            Some(Delta::Error("out of memory".into()))
        );
        // Blank lines, keep-alive comments, and shapes we do not know are all
        // skipped rather than failing the turn.
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line(": ping"), None);
        assert_eq!(parse_line("data: "), None);
        assert_eq!(parse_line("event: message"), None);
        assert_eq!(
            parse_line(r#"{"id":"x","object":"chat.completion.chunk"}"#),
            None
        );
    }

    #[test]
    fn llama_cpp_content_shape() {
        assert_eq!(
            parse_line(r#"{"content":"piece","stop":false}"#),
            Some(Delta::Text("piece".into()))
        );
    }
}
