//! What an extension becomes when it answers: `build_command` renders the
//! `search` template into a shell line, `cache_key` names the slot an answer
//! is kept under, and `known_keywords` is the set the query parser validates
//! against.

use std::collections::HashSet;

use serde_json::Value;

use super::def::Extension;
use crate::support::quote as shellquote;

/// `{query}` and `{any-filter}` are replaced by shell-quoted values. Anything
/// unmatched becomes an empty quoted string rather than a literal brace.
pub fn build_command(
    ext: &Extension,
    arg_text: &str,
    filters: &std::collections::BTreeMap<String, String>,
    settings: Option<&serde_json::Map<String, Value>>,
) -> String {
    let re = placeholder_re();
    let command = re
        .replace_all(&ext.search, |caps: &fancy_regex::Captures| {
            let key = caps
                .get(1)
                .map(|m| m.as_str().to_lowercase())
                .unwrap_or_default();
            if key == "query" {
                shellquote::quote(&msys_path(arg_text))
            } else if let Some(v) = filters.get(&key) {
                shellquote::quote(&msys_path(v))
            } else {
                shellquote::quote("")
            }
        })
        .into_owned();

    settings_prefix(settings) + &command
}

/// Git Bash rewrites an argument that begins with `/` into a Windows path
/// when it hands the line to a native child — `oxy-agent search /policy`
/// arrives as `C:/Program Files/Git/policy`. Doubling the leading slash is
/// MSYS's own escape: `//policy` converts to `/policy`, so literals and POSIX
/// paths both survive. Anywhere else the query passes through untouched.
fn msys_path(arg: &str) -> std::borrow::Cow<'_, str> {
    if cfg!(windows) && arg.starts_with('/') {
        format!("/{arg}").into()
    } else {
        arg.into()
    }
}

fn placeholder_re() -> &'static fancy_regex::Regex {
    static RE: std::sync::OnceLock<fancy_regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        // `i`, as the script's `gi` carried: `{Query}` substitutes the same
        // as `{query}` — the capture is lowercased before lookup.
        fancy_regex::Regex::new(r"(?i)\{([a-z0-9_-]+)\}").expect("placeholder regex compiles")
    })
}

/// What `settings:` collected, handed to the script as environment.
/// Environment rather than arguments: an argument is visible in every process
/// listing on the machine, and a token is the first thing anybody puts here.
fn settings_prefix(settings: Option<&serde_json::Map<String, Value>>) -> String {
    let mut out = String::new();
    let Some(settings) = settings else { return out };
    for (key, value) in settings {
        // Only what could be an environment name, so a hand-edited config
        // cannot inject a second command through a key.
        let valid = key.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid || value.is_null() {
            continue;
        }
        let rendered = match value {
            Value::String(x) => x.clone(),
            other => other.to_string(),
        };
        out.push_str(&format!(
            "OXY_{}={} ",
            key.to_uppercase(),
            shellquote::quote(&rendered)
        ));
    }
    out
}

/// The key an answer is cached and refreshed under: the command itself for a
/// command extension, the spelled-out question for a socket or native one.
pub fn cache_key(
    ext: &Extension,
    command: &str,
    arg_text: &str,
    filters: &std::collections::BTreeMap<String, String>,
) -> String {
    if !command.is_empty() {
        return command.to_string();
    }
    let filters = serde_json::to_string(filters).unwrap_or_default();
    format!("ask\0{}\0{}\0{}", ext.socket, arg_text, filters)
}

/// The keyword set the parser validates against: every keyword, alias and
/// declared filter of every extension, plus the built-in scopes.
pub fn known_keywords(extensions: &[Extension], extra: &HashSet<String>) -> HashSet<String> {
    let mut out = extra.clone();
    for ext in extensions {
        out.extend(ext.keywords());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn build_command_substitutes() {
        let ext = Extension::normalize(
            &json!({"id":"file","search":"find {query} {format}","filters":["format"]}),
            PathBuf::new(),
        )
        .unwrap();
        let mut filters = std::collections::BTreeMap::new();
        filters.insert("format".to_string(), "pdf".to_string());
        let cmd = build_command(&ext, "report's", &filters, None);
        assert_eq!(cmd, "find 'report'\\''s' 'pdf'");
    }
}
