//! The rows `note:` draws — the script's `row` emit and its two action
//! lists, kept field for field. The `exec` strings still call back into
//! `oxy-note`, `wl-copy` and `nautilus` verbatim: the write legs were never
//! ported, so a row built here runs the same script a script row would.

use serde_json::{Value, json};

use crate::support::quote::quote;

/// One editor offer: the program `command -v` is asked for, the label the
/// action wears, and the command `{}` in which the quoted path lands.
pub type Editor = (String, String, String);

/// The script's `row()`: `jq -cn` over named arguments, `preview`
/// duplicating `excerpt` — the same object for every kind of row.
#[allow(clippy::too_many_arguments)]
pub fn row(
    id: &str,
    title: &str,
    subtitle: &str,
    detail: &str,
    excerpt: &str,
    words: usize,
    kind: &str,
    exec: &str,
    score: i64,
    actions: Vec<Value>,
    fresh: bool,
) -> Value {
    json!({
        "id": id,
        "title": title,
        "subtitle": subtitle,
        "detail": detail,
        "excerpt": excerpt,
        "preview": excerpt,
        "words": words,
        "kind": kind,
        "exec": exec,
        "score": score,
        "view": "notes",
        "fresh": fresh,
        "actions": actions,
    })
}

/// The first action's `exec` — the script's `jq -r '.[0].exec // ""'` —
/// which is what Enter on the row runs. With no editor installed that is
/// "Copy Contents", the same fallback the script produces.
pub fn first_exec(actions: &[Value]) -> String {
    actions
        .first()
        .and_then(|a| a.get("exec"))
        .and_then(|e| e.as_str())
        .unwrap_or("")
        .to_string()
}

/// `note_actions` — every way out of a note that exists: each installed
/// editor (the first wearing ↵), then what you do with a note without
/// opening it. The `{}` in an editor's command is the path, quoted.
pub fn note_actions(path: &str, editors: &[Editor]) -> Vec<Value> {
    let q = quote(path);
    let mut acts: Vec<Value> = editors
        .iter()
        .map(|(_, label, cmd)| {
            json!({
                "title": format!("Open in {label}"),
                "exec": cmd.replace("{}", &q),
            })
        })
        .collect();
    if let Some(first) = acts.first_mut()
        && let Some(obj) = first.as_object_mut()
    {
        obj.insert("shortcut".to_string(), json!("↵"));
    }
    acts.extend([
        json!({ "title": "Copy Contents", "exec": format!("wl-copy < {q}") }),
        json!({ "title": "Copy Path", "exec": format!("printf %s {q} | wl-copy") }),
        json!({ "title": "Reveal in Files", "exec": format!("nautilus --select {q}") }),
        json!({ "title": "Move to Wastebasket", "exec": format!("oxy-note --trash {q}") }),
    ]);
    acts
}

/// `new_actions` — the same list for text that is not a note yet: save,
/// then save-and-open in each editor, then copy. The `query: "note:"` on
/// Save is the follow-up the engine re-asks, so the written note is on top
/// saying "Saved just now" by the time the launcher looks again.
pub fn new_actions(query: &str, editors: &[Editor]) -> Vec<Value> {
    let q = quote(query);
    let mut acts = vec![json!({
        "title": "Save Note",
        "shortcut": "↵",
        "exec": format!("oxy-note --save {q}"),
        "query": "note:",
    })];
    acts.extend(editors.iter().map(|(key, label, _)| {
        json!({
            "title": format!("Save and Open in {label}"),
            "exec": format!("oxy-note --open {key} {q}"),
        })
    }));
    acts.push(json!({ "title": "Copy Text", "exec": format!("printf %s {q} | wl-copy") }));
    acts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editors() -> Vec<Editor> {
        vec![
            (
                "omawrite".to_string(),
                "Omawrite".to_string(),
                "omawrite {}".to_string(),
            ),
            (
                "nvim".to_string(),
                "Neovim (Omarchy default)".to_string(),
                "omarchy-launch-tui --app-id=org.omarchy.note nvim {}".to_string(),
            ),
        ]
    }

    #[test]
    fn the_row_shape_is_the_scripts() {
        let r = row(
            "/n/x.md",
            "T",
            "just now",
            "5 Aug 2027",
            "body",
            4,
            "note",
            "open it",
            87999,
            vec![json!({"title": "A", "exec": "a"})],
            true,
        );
        assert_eq!(r["id"], json!("/n/x.md"));
        assert_eq!(r["title"], json!("T"));
        assert_eq!(r["subtitle"], json!("just now"));
        assert_eq!(r["detail"], json!("5 Aug 2027"));
        assert_eq!(r["excerpt"], json!("body"));
        // `preview` is the excerpt again — Ctrl+Enter shows the same text.
        assert_eq!(r["preview"], json!("body"));
        assert_eq!(r["words"], json!(4));
        assert_eq!(r["kind"], json!("note"));
        assert_eq!(r["exec"], json!("open it"));
        assert_eq!(r["score"], json!(87999));
        assert_eq!(r["view"], json!("notes"));
        assert_eq!(r["fresh"], json!(true));
        assert_eq!(r["actions"][0]["title"], json!("A"));
    }

    #[test]
    fn note_actions_offer_every_editor_then_the_rest() {
        let acts = note_actions("/home/u/Notes/a b.md", &editors());
        // Vim order: each editor, then the four fixed ways out.
        assert_eq!(acts.len(), 2 + 4);
        assert_eq!(acts[0]["title"], json!("Open in Omawrite"));
        assert_eq!(acts[0]["exec"], json!("omawrite '/home/u/Notes/a b.md'"));
        // Only the first editor carries the Enter shortcut.
        assert_eq!(acts[0]["shortcut"], json!("↵"));
        assert!(acts[1].get("shortcut").is_none());
        assert_eq!(
            acts[1]["exec"],
            json!("omarchy-launch-tui --app-id=org.omarchy.note nvim '/home/u/Notes/a b.md'")
        );
        assert_eq!(acts[2]["title"], json!("Copy Contents"));
        assert_eq!(acts[2]["exec"], json!("wl-copy < '/home/u/Notes/a b.md'"));
        assert_eq!(
            acts[3]["exec"],
            json!("printf %s '/home/u/Notes/a b.md' | wl-copy")
        );
        assert_eq!(
            acts[4]["exec"],
            json!("nautilus --select '/home/u/Notes/a b.md'")
        );
        assert_eq!(
            acts[5]["exec"],
            json!("oxy-note --trash '/home/u/Notes/a b.md'")
        );
        // The row's own exec is the first action's.
        assert_eq!(first_exec(&acts), "omawrite '/home/u/Notes/a b.md'");
    }

    #[test]
    fn note_actions_with_no_editors_still_copy_and_trash() {
        let acts = note_actions("/n/x.md", &[]);
        assert_eq!(acts.len(), 4);
        assert_eq!(acts[0]["title"], json!("Copy Contents"));
        // `.[0].exec` is then the copy — what the script's jq would answer.
        assert_eq!(first_exec(&acts), "wl-copy < '/n/x.md'");
    }

    #[test]
    fn new_actions_save_then_save_and_open_then_copy() {
        let acts = new_actions("a b", &editors());
        assert_eq!(acts.len(), 1 + 2 + 1);
        assert_eq!(acts[0]["title"], json!("Save Note"));
        assert_eq!(acts[0]["shortcut"], json!("↵"));
        assert_eq!(acts[0]["exec"], json!("oxy-note --save 'a b'"));
        // The follow-up re-enters the mode, so the save shows itself.
        assert_eq!(acts[0]["query"], json!("note:"));
        // The editor entries use the key, not the label, in the exec.
        assert_eq!(acts[1]["exec"], json!("oxy-note --open omawrite 'a b'"));
        assert_eq!(acts[1]["title"], json!("Save and Open in Omawrite"));
        assert_eq!(acts[2]["exec"], json!("oxy-note --open nvim 'a b'"));
        assert_eq!(acts[3]["exec"], json!("printf %s 'a b' | wl-copy"));
    }
}
