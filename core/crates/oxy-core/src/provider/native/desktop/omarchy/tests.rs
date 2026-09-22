use super::*;
use serde_json::json;

/// The fixture tree the tests share, over the panels `items_from` always
/// seeds: a root with an action, a bare submenu and a `run`-carrying link
/// under it, a `when`-hidden node, and one more root.
fn fixture_tree() -> Map<String, Value> {
    let mut t = items_from(&[]);
    for (k, v) in [
        ("oxyfix", json!({"icon": "F", "label": "Oxyfix"})),
        (
            "oxyfix.theme",
            json!({"icon": "T", "label": "Fixture Theme",
                   "aliases": ["fixtheme"], "action": "echo theme"}),
        ),
        (
            "oxyfix.font",
            json!({"icon": "N", "label": "Fixture Font",
                   "description": "Pick a font", "action": "echo font"}),
        ),
        ("oxyfix.deep", json!({"label": "Deep"})),
        (
            "oxyfix.deep.leaf",
            json!({"label": "Leaf", "target": "https://example.com",
                   "run": "xdg-open https://example.com"}),
        ),
        (
            "oxyfix.hidden",
            json!({"label": "Oxyfix Concealed", "action": "echo h", "when": "false"}),
        ),
        ("zzzlast", json!({"label": "Zzzlast", "action": "echo z"})),
    ] {
        t.insert(k.to_string(), v);
    }
    t
}

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("oxy-omarchy-test-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn strip_drops_comment_lines_and_trailing_commas() {
    let raw =
        "{\n  // gone\n     // indented, gone too\n  \"a\": 1,\n  \"b\": [\n    1,\n  ],\n}\n";
    let stripped = strip_jsonc(raw);
    assert!(serde_json::from_str::<Value>(&stripped).is_ok());
    // `//` mid-line stays — a URL is not a comment.
    assert!(strip_jsonc("{\"u\": \"http://x\"}\n").contains("http://x"));
    // The comma goes, the newline before the closer stays.
    assert_eq!(strip_jsonc("[1,\n]"), "[1\n]");
    // And the same blind rule applies inside a string — the menu's
    // leniency, not correctness.
    assert_eq!(strip_jsonc("{\"a\": \"x,}\"}"), "{\"a\": \"x}\"}");
}

#[test]
fn title_case_is_pythons() {
    assert_eq!(title_case("panel"), "Panel");
    assert_eq!(title_case("speed test"), "Speed Test");
    assert_eq!(title_case("a1b"), "A1B");
    assert_eq!(title_case("foo_bar"), "Foo_Bar");
    assert_eq!(title_case("it's"), "It'S");
}

#[test]
fn label_falls_back_to_the_last_segment() {
    assert_eq!(label_of(None, "panel"), "Panel");
    assert_eq!(
        label_of(None, "trigger.tmux-keybindings"),
        "Tmux Keybindings"
    );
    let e = json!({"label": ""});
    assert_eq!(label_of(Some(&e), "a.b"), "B");
    let e = json!({"label": "Theme"});
    assert_eq!(label_of(Some(&e), "style.theme"), "Theme");
}

#[test]
fn load_reads_items_wrapper_or_the_bare_map() {
    let dir = tmp("load");
    let bare = dir.join("bare.jsonc");
    std::fs::write(&bare, "{\"a\": {\"label\": \"A\"}, \"s\": \"str\"}").unwrap();
    let got = load(&bare);
    assert!(got.contains_key("a") && got.contains_key("s"));
    let wrapped = dir.join("wrapped.jsonc");
    std::fs::write(&wrapped, "{\"items\": {\"a\": {}}}").unwrap();
    let got = load(&wrapped);
    assert_eq!(got.len(), 1);
    assert!(got.contains_key("a"));
    for name in ["missing", "junk", "list", "blank"] {
        let p = dir.join(name);
        match name {
            "junk" => std::fs::write(&p, "{not json").unwrap(),
            "list" => std::fs::write(&p, "[1,2]").unwrap(),
            "blank" => std::fs::write(&p, "// nothing else\n").unwrap(),
            _ => {}
        }
        assert!(load(&p).is_empty(), "{name}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn items_merge_is_panels_then_files_and_a_redeclared_id_keeps_its_place() {
    let dir = tmp("merge");
    let d = dir.join("d.jsonc");
    let u = dir.join("u.jsonc");
    std::fs::write(
        &d,
        "{\"one\": {}, \"two\": {\"label\": \"Old\"}, \"gone\": 5}",
    )
    .unwrap();
    std::fs::write(&u, "{\"two\": {\"label\": \"New\"}, \"three\": {}}").unwrap();
    let t = items_from(&[d, u]);
    let keys: Vec<&str> = t.keys().map(String::as_str).collect();
    // Panels first, file order after; `gone` was not an object, and `two`
    // was redeclared — it keeps its slot and takes the new body.
    assert_eq!(keys[0], "panel.network");
    assert_eq!(keys[4], "panel.battery");
    assert_eq!(keys[5..], ["one", "two", "three"]);
    assert!(!t.contains_key("gone"));
    assert_eq!(t["two"]["label"], json!("New"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn browsing_is_the_root_in_file_order() {
    let tree = fixture_tree();
    let rows = collect(&tree, "");
    let ids: Vec<&str> = rows.iter().map(|r| r.node_id.as_str()).collect();
    // The panels' dotted ids keep them out of the root; so does every
    // second-level route.
    assert_eq!(ids, ["oxyfix", "zzzlast"]);
    // Every descendant at any depth counts — deep.leaf included.
    assert_eq!(rows[0].children, 5);
}

#[test]
fn search_ranks_where_the_words_landed() {
    let tree = fixture_tree();
    // Both words land on the theme row only.
    let rows = collect(&tree, "oxyfix theme");
    assert_eq!(rows[0].node_id, "oxyfix.theme");
    assert_eq!(rows[0].missed, 0);
    // The subtree answer: "oxyfix" lands everywhere under it, "deep"
    // narrows to the branch — and the shorter path wins the tie.
    let rows = collect(&tree, "oxyfix deep");
    assert_eq!(rows[0].node_id, "oxyfix.deep");
    assert_eq!(rows[0].kind, "menu");
    assert_eq!(rows[0].children, 1);
    assert_eq!(rows[1].node_id, "oxyfix.deep.leaf");
    // A word that lands nowhere drops the row entirely.
    assert!(collect(&tree, "definitelyabsent").is_empty());
    // A half-asked question still answers, ranked behind: only "fixture"
    // landed, so everything carries a miss.
    let rows = collect(&tree, "fixture absentword");
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.missed == 1));
    // Alphabetical breaks the last tie: Font before Theme.
    assert_eq!(rows[0].node_id, "oxyfix.font");
    assert_eq!(rows[1].node_id, "oxyfix.theme");
}

#[test]
fn each_rank_beats_the_next() {
    // One node per rank the script knows: exact label, label prefix,
    // alias, substring, all-words, and "the id was all that landed".
    let mut tree = items_from(&[]);
    for (k, v) in [
        ("a.exact", json!({"label": "Match This", "action": "x"})),
        ("a.prefix", json!({"label": "Match Thisly", "action": "x"})),
        (
            "a.alias",
            json!({"label": "Other", "aliases": ["match this"], "action": "x"}),
        ),
        (
            "a.substr",
            json!({"label": "A Match This One", "action": "x"}),
        ),
        ("a.words", json!({"label": "Match X This", "action": "x"})),
        ("a.matchrest", json!({"label": "Zzz", "action": "x"})),
    ] {
        tree.insert(k.to_string(), v);
    }
    // "match" reaches a.matchrest only through its node id — "this" never
    // lands there, so it trails the whole set on the miss.
    let rows = collect(&tree, "match this");
    let ids: Vec<&str> = rows.iter().map(|r| r.node_id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "a.exact",
            "a.prefix",
            "a.alias",
            "a.substr",
            "a.words",
            "a.matchrest"
        ]
    );
}

#[test]
fn row_json_carries_every_field_the_view_draws() {
    let tree = fixture_tree();
    let rows = collect(&tree, "oxyfix leaf");
    let leaf = rows
        .iter()
        .find(|r| r.node_id == "oxyfix.deep.leaf")
        .unwrap();
    let r = row_json(leaf, 1, false);
    assert_eq!(r["id"], json!("omarchy-oxyfix.deep.leaf"));
    assert_eq!(r["title"], json!("Leaf"));
    assert_eq!(r["subtitle"], json!("Oxyfix  ·  Deep"));
    assert_eq!(r["accessory"], json!("oxyfix.deep.leaf"));
    assert_eq!(r["exec"], json!("xdg-open https://example.com"));
    assert_eq!(r["score"], json!(94900));
    assert_eq!(r["view"], json!("menutree"));
    assert_eq!(r["mode"], json!("search"));
    assert_eq!(r["trail"], json!(["Oxyfix", "Deep"]));
    assert_eq!(r["kind"], json!("link"));
    assert_eq!(r["node"], json!("oxyfix.deep.leaf"));
    assert_eq!(r["depth"], json!(3));
    assert_eq!(r["children"], json!(0));
    assert_eq!(
        r["actions"][0],
        json!({"title": "Open", "shortcut": "↵", "exec": "xdg-open https://example.com"})
    );
    assert_eq!(
        r["actions"][1],
        json!({"title": "Copy the Route",
               "exec": "printf %s \"oxyfix.deep.leaf\" | wl-copy"})
    );
}

#[test]
fn menu_rows_say_they_open_and_summon() {
    let tree = fixture_tree();
    let rows = collect(&tree, "oxyfix deep");
    let deep = rows.iter().find(|r| r.node_id == "oxyfix.deep").unwrap();
    let r = row_json(deep, 2, false);
    assert_eq!(r["subtitle"], json!("Oxyfix  ·  opens a submenu"));
    assert_eq!(r["exec"], json!("omarchy menu summon oxyfix.deep"));
    assert_eq!(r["score"], json!(94800));
    // A root row with no trail calls itself Omarchy.
    let root = collect(&tree, "")
        .into_iter()
        .find(|r| r.node_id == "oxyfix")
        .unwrap();
    let r = row_json(&root, 1, true);
    assert_eq!(r["subtitle"], json!("Omarchy  ·  opens a submenu"));
    assert_eq!(r["mode"], json!("browse"));
    assert_eq!(r["trail"], json!([]));
}

#[test]
fn py_dumps_is_the_json_dumps_the_script_embeds() {
    assert_eq!(py_dumps_str("style.theme"), "\"style.theme\"");
    assert_eq!(py_dumps_str("café"), "\"caf\\u00e9\"");
    assert_eq!(py_dumps_str("a\nb"), "\"a\\nb\"");
    assert_eq!(py_dumps_str("\u{7f}"), "\"\\u007f\"");
    assert_eq!(py_dumps_str("x\u{1f600}"), "\"x\\ud83d\\ude00\"");
}

#[tokio::test]
async fn when_gates_what_the_menu_would_hide() {
    let tree = fixture_tree();
    let candidates = collect(&tree, "oxyfix");
    // Empty and falsy `when`s pass without spawning; a truthy non-string
    // is the script's TypeError, which its `except` reads as hidden; and
    // `false` fails however it runs — a `bash` exit 1 here, a failed
    // spawn where no bash lives.
    assert!(passes(&Value::Null).await);
    assert!(passes(&json!("")).await);
    assert!(!passes(&json!(5)).await);
    let hidden = candidates
        .iter()
        .find(|c| c.node_id == "oxyfix.hidden")
        .unwrap();
    assert!(!passes(&hidden.when).await);
}

#[test]
fn panels_carry_their_own_summon() {
    let tree = fixture_tree();
    let rows = collect(&tree, "panel.network");
    assert_eq!(rows.len(), 1);
    let p = &rows[0];
    assert_eq!(p.kind, "action");
    assert_eq!(p.trail, vec!["Panel".to_string()]);
    assert_eq!(p.exec, json!("omarchy-shell shell summon omarchy.network"));
}
