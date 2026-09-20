//! `pass:` — the vault: names and folders from `pass` or 1Password's `op`,
//! whichever is installed. A port of `bin/oxy-pass`.
//!
//!   pass:            every entry
//!   pass:github      the ones whose name matches
//!
//! What the script proved, kept:
//!   * `pass` wins when both are installed — it answers without a network
//!     and without an unlock prompt, which is the difference between a
//!     keyword that feels instant and one that hangs the launcher.
//!   * A row never carries a secret — not its length, not a field it came
//!     from. Rows are name/folder/store metadata only; copy and clear stay
//!     inside `oxy-pass copy`, whose pipes were written not to echo, and
//!     the exec strings below are that leg's own spelling.
//!   * `folder` and `name` travel separately — the `vault` view draws the
//!     hierarchy — and a top-level entry's folder is `""`, which is not an
//!     entry filed under a folder that shares its name.
//!   * Names come from the tree on disk (`find`), not `pass ls`, whose
//!     output is drawn with box characters meant for a terminal.
//!   * Nothing is cached: the list is re-read each query, because an entry
//!     list is cheap and a stale copy of one is a lie about what you have.
//!   * An empty store is a zero-row answer — the launcher's empty state is
//!     what says so, not a row the script never emitted.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::native::util::{on_path, shq};
use crate::provider::process::run;
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::support::quote::quote;

#[derive(Default)]
pub struct Pass;

/// `CLEAR_SECONDS` — the count `pass -c` also uses, and what the vault
/// view's strip promises before Return is pressed.
const CLEAR_SECONDS: i64 = 45;

/// `find` is a local walk — seconds of headroom it never needs. The two
/// `op` calls share the manifest's 8000ms between them.
const FIND: Duration = Duration::from_secs(2);
const OP: Duration = Duration::from_secs(4);

/// `STORE="${PASSWORD_STORE_DIR:-$HOME/.password-store}"` — an empty
/// variable reads as unset, the way `:-` treats it.
fn store_dir() -> PathBuf {
    std::env::var_os("PASSWORD_STORE_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::settings::paths::home().join(".password-store"))
}

/// The script's own needle check — `*"$needle"*` over the lowered name and
/// folder is a substring match against either.
fn wanted(name: &str, folder: &str, needle: &str) -> bool {
    needle.is_empty()
        || name.to_lowercase().contains(needle)
        || folder.to_lowercase().contains(needle)
}

/// `have_op`'s second half: `[[ -n $(op account list 2>/dev/null) ]]` —
/// command substitution strips the trailing newlines, and what is left is
/// the sign-in check.
fn signed_in(stdout: &str) -> bool {
    !stdout.trim_end_matches('\n').is_empty()
}

/// The script's `row()`: one jq object per entry — metadata only, with
/// `exec` pointing back at `oxy-pass copy`, the leg that reads the secret
/// and pipes it to `wl-copy` without ever printing it. `copyname` is what
/// "Copy Entry Name" puts on the clipboard (the jq `@sh`, which is the
/// single-quote spelling `quote` already is).
fn entry_row(
    tool: &str,
    id: &str,
    name: &str,
    folder: &str,
    copyname: &str,
    store: &str,
    score: i64,
) -> Value {
    let exec = format!("oxy-pass copy {} {} {}", shq(tool), shq(id), shq(name));
    json!({
        "id": id,
        "title": name,
        // A folderless entry still needs something in the subtitle for any
        // view that only knows how to draw one, and the store it came from
        // is the honest answer.
        "subtitle": if folder.is_empty() { store } else { folder },
        "glyph": "󰌾",
        "exec": exec,
        "score": score,
        "folder": folder,
        "name": name,
        "tool": tool,
        "store": store,
        "clearSeconds": CLEAR_SECONDS,
        "actions": [
            { "title": "Copy Password", "shortcut": "↵", "exec": exec },
            { "title": "Copy Entry Name", "exec": format!("printf %s {} | wl-copy", quote(copyname)) },
        ],
    })
}

/// `find`'s stdout → `(entry, name, folder)` — the script's parameter
/// expansions one for one: the `$STORE/` prefix and the `.gpg` suffix
/// stripped, the folder everything before the last slash or nothing at
/// all, the name everything after it.
fn pass_entries(store: &str, listing: &str) -> Vec<(String, String, String)> {
    let prefix = format!("{store}/");
    listing
        .lines()
        .filter_map(|file| {
            let entry = file.strip_prefix(&prefix).unwrap_or(file);
            let entry = entry.strip_suffix(".gpg").unwrap_or(entry);
            if entry.is_empty() {
                return None;
            }
            let (folder, name) = match entry.rfind('/') {
                Some(i) => (&entry[..i], &entry[i + 1..]),
                None => ("", entry),
            };
            Some((entry.to_string(), name.to_string(), folder.to_string()))
        })
        .collect()
}

/// `.x // ""` as the script's jq read it — null and false both fall to
/// empty — then `@tsv`'s render of what survived: strings as themselves,
/// everything else as JSON.
fn jq_str(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | Some(Value::Bool(false)) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// `op item list --format json` → `(vault, title)` pairs — the array the
/// script piped through `jq -r '.[]? | [.vault.name // "", .title // ""] |
/// @tsv'`, parsed directly instead of going through the tab escape. A null
/// item is the empty line the `read` loop skipped; a non-object one (or a
/// scalar `vault`, which `.name` cannot index) is where jq would have
/// died, keeping what was already printed.
fn op_items(listing: &str) -> Vec<(String, String)> {
    let Ok(v) = serde_json::from_str::<Value>(listing) else {
        return Vec::new();
    };
    // `.[]?` iterates arrays and an object's values alike; on a scalar the
    // error is suppressed and nothing prints at all.
    let items: Vec<&Value> = match &v {
        Value::Array(a) => a.iter().collect(),
        Value::Object(o) => o.values().collect(),
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for item in items {
        if item.is_null() {
            continue;
        }
        if !item.is_object() {
            break;
        }
        let vault = match item.get("vault") {
            None | Some(Value::Null) => String::new(),
            Some(Value::Object(v)) => jq_str(v.get("name")),
            Some(_) => break,
        };
        let title = jq_str(item.get("title"));
        if title.is_empty() {
            continue;
        }
        out.push((vault, title));
    }
    out
}

/// The Password Store leg: the sorted `.gpg` tree under `$STORE`, one row
/// per entry that survived the needle. `row pass "$entry" "${entry##*/}"
/// "$folder" "$entry"` — the full path is both the reference and the name
/// "Copy Entry Name" puts on the clipboard.
fn pass_rows(store: &str, listing: &str, needle: &str) -> Vec<Value> {
    let mut rows = Vec::new();
    let mut score = 90000i64;
    for (entry, name, folder) in pass_entries(store, listing) {
        if !wanted(&name, &folder, needle) {
            continue;
        }
        rows.push(entry_row(
            "pass",
            &entry,
            &name,
            &folder,
            &entry,
            "Password Store",
            score,
        ));
        score -= 100;
    }
    rows
}

/// The 1Password leg: `row op "$vault/$title/password" "$title" "$vault"
/// "$title"` — `op read op://vault/item/password` is the one call that
/// never needs the item id resolved a second time, so the row carries
/// vault/item and the copy path appends the field.
fn op_rows(listing: &str, needle: &str) -> Vec<Value> {
    let mut rows = Vec::new();
    let mut score = 90000i64;
    for (vault, title) in op_items(listing) {
        if !wanted(&title, &vault, needle) {
            continue;
        }
        let id = format!("{vault}/{title}/password");
        rows.push(entry_row(
            "op",
            &id,
            &title,
            &vault,
            &title,
            "1Password",
            score,
        ));
        score -= 100;
    }
    rows
}

impl NativeExt for Pass {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        // `needle="${query,,}"` — the argument lowercased, untrimmed, as
        // the script took it.
        let needle = ctx.arg.to_lowercase();
        Box::pin(async move {
            let store = store_dir();
            let pass_on_path = on_path("pass");

            // `have_pass()`: the binary and the store dir both — `pass`
            // installed but never `pass init`'d counts as absent, and `op`
            // gets its turn below.
            if pass_on_path && store.is_dir() {
                let listing = run(
                    &format!(
                        "find {} -type f -name '*.gpg' 2>/dev/null | sort",
                        quote(&store.to_string_lossy())
                    ),
                    FIND,
                )
                .await
                .map(|f| f.stdout)
                .unwrap_or_default();
                return NativeOutcome::Rows(pass_rows(&store.to_string_lossy(), &listing, &needle));
            }

            // `have_op()`: the binary and a signed-in account — `op account
            // list` prints nothing at all until one is added, which is the
            // whole second half of the manifest's `when`.
            let op_signed_in = on_path("op")
                && run("op account list 2>/dev/null", OP)
                    .await
                    .map(|f| signed_in(&f.stdout))
                    .unwrap_or(false);
            if op_signed_in {
                let listing = run("op item list --format json 2>/dev/null", OP)
                    .await
                    .map(|f| f.stdout)
                    .unwrap_or_default();
                return NativeOutcome::Rows(op_rows(&listing, &needle));
            }

            if pass_on_path {
                // `command -v pass` alone satisfied the manifest's `when`;
                // the script then found no store to read and printed
                // nothing — an empty answer is the answer, not a decline.
                return NativeOutcome::Rows(Vec::new());
            }
            // Neither half of the `when` holds — `pass` absent and `op`
            // missing or signed out — so the script leg gets its say.
            NativeOutcome::Fallback
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `find $STORE -type f -name '*.gpg' | sort`, as it really prints:
    /// sorted full paths under the store root.
    const STORE: &str = "/home/u/.password-store";
    const FIND_OUT: &str = "/home/u/.password-store/email/fastmail.gpg\n/home/u/.password-store/email/work.gpg\n/home/u/.password-store/github.gpg\n/home/u/.password-store/ssh/deploy-key.gpg\n";

    /// `op item list --format json`, the fields the script reads — plus the
    /// ones a real item also carries and a row must never repeat.
    const OP_LIST: &str = r#"[
      {"id":"a1","title":"Netflix","vault":{"id":"v1","name":"Personal"},
       "category":"LOGIN","content_checksum":"x","password":"hunter2"},
      {"id":"a2","title":"Deploy Key","vault":{"id":"v2","name":"Work"},
       "fields":[{"id":"password","value":"s3cret"}]},
      {"id":"a3","title":"Top"},
      {"id":"a4","title":""},
      null
    ]"#;

    #[test]
    fn the_gate_reading_is_command_substitutions() {
        // `[[ -n $(...) ]]` — trailing newlines stripped, nothing else.
        assert!(signed_in("acct@example.com\n"));
        assert!(signed_in("  \n")); // whitespace is still something
        assert!(!signed_in("\n\n\n"));
        assert!(!signed_in(""));
    }

    #[test]
    fn pass_entries_strip_prefix_suffix_and_split() {
        let entries = pass_entries(STORE, FIND_OUT);
        assert_eq!(
            entries,
            vec![
                (
                    "email/fastmail".to_string(),
                    "fastmail".to_string(),
                    "email".to_string()
                ),
                (
                    "email/work".to_string(),
                    "work".to_string(),
                    "email".to_string()
                ),
                // A top-level entry's folder is empty — not itself.
                ("github".to_string(), "github".to_string(), "".to_string()),
                (
                    "ssh/deploy-key".to_string(),
                    "deploy-key".to_string(),
                    "ssh".to_string()
                ),
            ]
        );
    }

    #[test]
    fn pass_entries_deep_folders_and_skips() {
        // `${entry%/*}` keeps the whole path above the name; a bare `.gpg`
        // and a blank line are the `[[ -n $entry ]]` skips.
        let listing = "/s/a/b/c.gpg\n/s/.gpg\n\n/s/odd\n";
        let entries = pass_entries("/s", listing);
        assert_eq!(
            entries,
            vec![
                ("a/b/c".to_string(), "c".to_string(), "a/b".to_string()),
                // No `.gpg` suffix, so nothing is stripped — `find -name
                // '*.gpg'` would not print it, but a stray line survives.
                ("odd".to_string(), "odd".to_string(), "".to_string()),
            ]
        );
    }

    #[test]
    fn pass_entries_a_path_outside_the_store_stands_whole() {
        // `${file#"$STORE"/}` only strips a real prefix — a line that does
        // not start with it is kept entire, split at its own last slash.
        let entries = pass_entries("/s", "/other/place/thing.gpg\n");
        assert_eq!(
            entries,
            vec![(
                "/other/place/thing".to_string(),
                "thing".to_string(),
                "/other/place".to_string()
            )]
        );
    }

    #[test]
    fn op_items_read_vault_and_title_only() {
        let items = op_items(OP_LIST);
        assert_eq!(
            items,
            vec![
                ("Personal".to_string(), "Netflix".to_string()),
                ("Work".to_string(), "Deploy Key".to_string()),
                // `.vault.name // ""` — no vault is an empty folder.
                ("".to_string(), "Top".to_string()),
            ]
        );
    }

    #[test]
    fn op_items_the_shapes_jq_survives() {
        assert!(op_items("not json").is_empty());
        assert!(op_items("{}").is_empty()); // an empty object iterates to nothing
        assert!(op_items("[]").is_empty());
        assert!(op_items("5").is_empty()); // `.[]?` on a scalar is suppressed
        // A non-object item ends the program where jq's `.vault` would have;
        // what printed before it stands.
        assert_eq!(
            op_items(r#"[{"title":"A"},{"title":"B"},"oops"]"#),
            vec![
                ("".to_string(), "A".to_string()),
                ("".to_string(), "B".to_string())
            ]
        );
        // A scalar `vault` cannot be indexed by `.name` — the same stop.
        assert_eq!(
            op_items(r#"[{"vault":"x","title":"A"},{"title":"B"}]"#),
            Vec::<(String, String)>::new()
        );
    }

    #[test]
    fn the_row_is_the_scripts_object() {
        let row = entry_row(
            "pass",
            "email/work",
            "work",
            "email",
            "email/work",
            "Password Store",
            90000,
        );
        assert_eq!(row["id"], json!("email/work"));
        assert_eq!(row["title"], json!("work"));
        assert_eq!(row["subtitle"], json!("email"));
        assert_eq!(row["glyph"], json!("󰌾"));
        assert_eq!(row["exec"], json!("oxy-pass copy pass email/work work"));
        assert_eq!(row["score"], json!(90000));
        assert_eq!(row["folder"], json!("email"));
        assert_eq!(row["name"], json!("work"));
        assert_eq!(row["tool"], json!("pass"));
        assert_eq!(row["store"], json!("Password Store"));
        assert_eq!(row["clearSeconds"], json!(45));
        let actions = row["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0]["title"], json!("Copy Password"));
        assert_eq!(actions[0]["shortcut"], json!("↵"));
        assert_eq!(
            actions[0]["exec"],
            json!("oxy-pass copy pass email/work work")
        );
        assert_eq!(actions[1]["title"], json!("Copy Entry Name"));
        assert_eq!(
            actions[1]["exec"],
            json!("printf %s 'email/work' | wl-copy")
        );
    }

    #[test]
    fn a_folderless_row_subtitles_the_store() {
        let row = entry_row(
            "pass",
            "github",
            "github",
            "",
            "github",
            "Password Store",
            90000,
        );
        assert_eq!(row["subtitle"], json!("Password Store"));
        assert_eq!(row["folder"], json!(""));
        let op = entry_row(
            "op",
            "Personal/Netflix/password",
            "Netflix",
            "Personal",
            "Netflix",
            "1Password",
            89900,
        );
        assert_eq!(op["subtitle"], json!("Personal"));
        assert_eq!(op["id"], json!("Personal/Netflix/password"));
        assert_eq!(op["store"], json!("1Password"));
        assert_eq!(op["tool"], json!("op"));
        assert_eq!(
            op["exec"],
            json!("oxy-pass copy op Personal/Netflix/password Netflix")
        );
    }

    #[test]
    fn the_exec_quotes_the_way_printf_q_did() {
        // `%q`: plain when it can be, backslashes when it must.
        let row = entry_row(
            "op",
            "Work/it's deploy/password",
            "it's deploy",
            "Work",
            "it's deploy",
            "1Password",
            90000,
        );
        assert_eq!(
            row["exec"],
            json!("oxy-pass copy op Work/it\\'s\\ deploy/password it\\'s\\ deploy")
        );
        // And the copy-name action's jq `@sh` is the single-quote spelling.
        assert_eq!(
            row["actions"][1]["exec"],
            json!("printf %s 'it'\\''s deploy' | wl-copy")
        );
    }

    #[test]
    fn no_field_is_the_secret() {
        // The fixture items carry passwords, checksums and field values;
        // none of it may reach a row — not in a field, not in an exec.
        for row in op_rows(OP_LIST, "") {
            let text = row.to_string();
            assert!(!text.contains("hunter2"), "{text}");
            assert!(!text.contains("s3cret"), "{text}");
            assert!(!text.contains("checksum"), "{text}");
            assert!(row.get("password").is_none());
            assert!(row.get("fields").is_none());
            // And the exec never embeds anything but the reference:
            // tool, vault/item/password id, and the display name.
            let exec = row["exec"].as_str().unwrap();
            assert!(exec.starts_with("oxy-pass copy op "), "{exec}");
        }
    }

    #[test]
    fn the_needle_matches_name_or_folder() {
        assert!(wanted("netflix", "", "flix"));
        assert!(wanted("netflix", "Personal", "pers"));
        assert!(!wanted("netflix", "personal", "work"));
        assert!(wanted("a", "", "")); // empty needle emits everything
    }

    #[test]
    fn pass_rows_filter_and_score_like_the_script() {
        let rows = pass_rows(STORE, FIND_OUT, "email");
        // Folder and name both match — the two email entries, in find's
        // sorted order, scores decremented per emitted row only.
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["id"], json!("email/fastmail"));
        assert_eq!(rows[0]["score"], json!(90000));
        assert_eq!(rows[1]["id"], json!("email/work"));
        assert_eq!(rows[1]["score"], json!(89900));
        // A needle that matches nothing is the script's silent loop.
        assert!(pass_rows(STORE, FIND_OUT, "zzz").is_empty());
        // An empty store emits nothing at all.
        assert!(pass_rows(STORE, "", "").is_empty());
    }

    #[test]
    fn op_rows_carry_the_vault_path_reference() {
        let rows = op_rows(OP_LIST, "");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["id"], json!("Personal/Netflix/password"));
        assert_eq!(rows[0]["title"], json!("Netflix"));
        assert_eq!(rows[0]["folder"], json!("Personal"));
        assert_eq!(rows[0]["store"], json!("1Password"));
        assert_eq!(rows[0]["tool"], json!("op"));
        assert_eq!(rows[0]["clearSeconds"], json!(45));
        assert_eq!(
            rows[0]["exec"],
            json!("oxy-pass copy op Personal/Netflix/password Netflix")
        );
        // `deploy` is in the name; `work` is in the folder — both match.
        assert_eq!(op_rows(OP_LIST, "work").len(), 1);
        assert_eq!(
            op_rows(OP_LIST, "deploy")[0]["id"],
            json!("Work/Deploy Key/password")
        );
    }
}
