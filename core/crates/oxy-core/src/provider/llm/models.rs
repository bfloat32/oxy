//! The model list a local server publishes, read from either shape.
//!
//! `/v1/models` answers the OpenAI shape (`{"data": [{"id": …}]}`) on ollama,
//! LM Studio and llama.cpp alike; ollama's own `/api/tags` answers
//! `{"models": [{"name": …}]}`. Both are read here, tolerantly, because the
//! point of the doctor is to tell a user what their server has — a parse that
//! only understands one shape would report "no models" for the other and send
//! them looking in the wrong place.

use serde_json::Value;

/// Every model id the body names, in the order it names them, without
/// duplicates. An unparseable body is an empty list — the caller reports the
/// checkpoint as failed either way.
pub fn parse_models(text: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    let mut push = |name: &str| {
        let name = name.trim();
        if !name.is_empty() && !out.iter().any(|m| m == name) {
            out.push(name.to_string());
        }
    };
    if let Some(list) = value.get("data").and_then(Value::as_array) {
        for entry in list {
            if let Some(id) = entry.get("id").and_then(Value::as_str) {
                push(id);
            }
        }
    }
    if let Some(list) = value.get("models").and_then(Value::as_array) {
        for entry in list {
            if let Some(name) = entry.get("name").and_then(Value::as_str) {
                push(name);
            }
        }
    }
    out
}

/// Does this list contain the configured model?
///
/// Exact match first; then the tag-insensitive one ollama needs, because
/// `ollama run llama3.2` and `/api/tags`' `llama3.2:latest` are the same
/// model and a doctor that called that a miss would be wrong.
pub fn has_model(models: &[String], wanted: &str) -> bool {
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return false;
    }
    if models.iter().any(|m| m == wanted) {
        return true;
    }
    let base = wanted.split(':').next().unwrap_or(wanted);
    models
        .iter()
        .any(|m| m.split(':').next().unwrap_or(m) == base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_openai_shape() {
        let body = r#"{"object":"list","data":[{"id":"llama3.2"},{"id":"qwen2.5:7b"}]}"#;
        assert_eq!(parse_models(body), vec!["llama3.2", "qwen2.5:7b"]);
    }

    #[test]
    fn the_ollama_shape() {
        let body = r#"{"models":[{"name":"llama3.2:latest","size":123},{"name":"nomic-embed"}]}"#;
        assert_eq!(parse_models(body), vec!["llama3.2:latest", "nomic-embed"]);
    }

    #[test]
    fn junk_is_an_empty_list_not_a_panic() {
        assert!(parse_models("not json").is_empty());
        assert!(parse_models("{}").is_empty());
        assert!(parse_models(r#"{"data":[]}"#).is_empty());
        assert!(parse_models(r#"{"data":[{"id":42},{"id":""}]}"#).is_empty());
    }

    #[test]
    fn a_model_is_found_with_or_without_its_tag() {
        let models = vec!["llama3.2:latest".to_string(), "qwen2.5:7b".to_string()];
        assert!(has_model(&models, "llama3.2"));
        assert!(has_model(&models, "llama3.2:latest"));
        assert!(has_model(&models, "qwen2.5"));
        assert!(!has_model(&models, "mistral"));
        assert!(!has_model(&models, ""));
    }
}
