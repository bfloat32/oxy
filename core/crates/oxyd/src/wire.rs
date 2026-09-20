use oxy_core::engine::EngineCmd;
use oxy_core::provider::worker::Shared;
use serde_json::{Value, json};

/// The hello each new client is owed — built per connect, not once at
/// boot: a reload changes the keyword set, and a client joining after one
/// must hear the set as it stands. Version is the plugin's, because the
/// log's `sess` line is what a bug report is read from.
pub(crate) fn hello_line(src: &Shared) -> String {
    let known = src
        .hello_keywords
        .read()
        .map(|k| (**k).clone())
        .unwrap_or_default();
    json!({
        "op": "hello",
        "version": oxy_core::PLUGIN_VERSION,
        "keywords": known,
    })
    .to_string()
        + "\n"
}

/// Parse a command line into an `EngineCmd`. Unknown ops are ignored rather
/// than fatal: a newer frontend talking to an older daemon degrades, not
/// dies. Takes the already-parsed `Value` — the caller looked at `op` first,
/// and a query line is parsed exactly once per keystroke.
pub(crate) fn parse_cmd(v: Value) -> Option<EngineCmd> {
    let op = v.get("op")?.as_str()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("");
    Some(match op {
        "query" => EngineCmd::Query {
            text: s("text").to_string(),
            opened: v.get("opened").and_then(|x| x.as_bool()).unwrap_or(true),
        },
        "open" => EngineCmd::Open {
            text: s("text").to_string(),
        },
        "clipboard" => EngineCmd::Clipboard {
            url: v.get("url").and_then(|x| x.as_str()).map(String::from),
        },
        "close" => EngineCmd::Close,
        "activate" => EngineCmd::Activate {
            key: s("key").to_string(),
            action: v.get("action").and_then(|x| x.as_u64()).map(|n| n as usize),
            shift: v.get("shift").and_then(|x| x.as_bool()).unwrap_or(false),
            ctrl: v.get("ctrl").and_then(|x| x.as_bool()).unwrap_or(false),
        },
        "act" => EngineCmd::Act {
            key: s("key").to_string(),
            // A synthesized action (a form's exec with {field} filled in):
            // not one of the row's declared actions, so it arrives whole.
            action: Box::new(
                serde_json::from_value(v.get("action").cloned().unwrap_or(Value::Null)).ok()?,
            ),
        },
        "pin" => EngineCmd::Pin {
            key: s("key").to_string(),
        },
        "select" => EngineCmd::Select {
            key: s("key").to_string(),
        },
        "commitpreview" => EngineCmd::CommitPreview,
        "set" => EngineCmd::Set {
            key: s("key").to_string(),
            value: v.get("value").and_then(|x| x.as_f64()).unwrap_or(0.0),
        },
        "savesettings" => EngineCmd::SaveSettings {
            id: s("id").to_string(),
            values: v
                .get("values")
                .and_then(|x| x.as_object().cloned())
                .unwrap_or_default(),
        },
        "ask" => EngineCmd::Ask {
            text: s("text").to_string(),
        },
        "stopask" => EngineCmd::StopAsk,
        "reload" => EngineCmd::Reload,
        "log" => EngineCmd::Log {
            ev: s("ev").to_string(),
            fields: v.get("fields").cloned().unwrap_or(Value::Null),
        },
        "ping" => EngineCmd::Ping,
        _ => return None,
    })
}
