use super::*;
use serde_json::json;

/// The fixture's `hyprctl binds -j`: three good binds, a bind whose key
/// Hyprland dropped, and one with nothing to say.
fn binds_json() -> &'static str {
    r#"[
      {"locked":false,"mouse":false,"release":false,"repeat":false,
       "non_consuming":false,"modmask":64,"submap":"","key":"T",
       "keycode":28,"catch_all":false,"description":"Terminal",
       "dispatcher":"exec","arg":"foot"},
      {"locked":false,"mouse":false,"release":true,"repeat":false,
       "non_consuming":false,"modmask":65,"submap":"","key":"F",
       "keycode":33,"catch_all":false,"description":"Fullscreen",
       "dispatcher":"__lua","arg":"7"},
      {"locked":false,"mouse":false,"release":false,"repeat":false,
       "non_consuming":false,"modmask":4,"submap":"","key":"K",
       "keycode":37,"catch_all":false,"description":"Copy line up",
       "dispatcher":"exec","arg":"copyq"},
      {"modmask":64,"key":"","description":"no key, no row",
       "dispatcher":"__lua","arg":"9"},
      {"modmask":64,"key":"Q","description":"",
       "dispatcher":"__lua","arg":"10"}
    ]"#
}

/// System records from the fixture plus the launcher's own — the same
/// `records=$( … )` the script's jq pass reads.
fn records() -> Vec<Vec<String>> {
    let mut r = parse_binds(binds_json());
    r.extend(OXY_BINDS.iter().map(|(m, k, a)| record("Oxy", m, k, a)));
    r
}

fn titles(rows: &[Value]) -> Vec<&str> {
    rows.iter().map(|r| r["title"].as_str().unwrap()).collect()
}

#[test]
fn the_modmask_table_is_the_scripts() {
    let at = |m: i64| modmask_words(Some(&json!(m)));
    assert_eq!(at(0), "");
    assert_eq!(at(1), "SHIFT");
    assert_eq!(at(4), "CTRL");
    assert_eq!(at(5), "SHIFT CTRL");
    assert_eq!(at(8), "ALT");
    assert_eq!(at(12), "CTRL ALT");
    assert_eq!(at(64), "SUPER");
    assert_eq!(at(65), "SUPER SHIFT");
    assert_eq!(at(69), "SUPER SHIFT CTRL");
    assert_eq!(at(77), "SUPER SHIFT CTRL ALT");
    // A mask the table does not know — CapsLock held, mod2 — is no
    // modifiers, and a missing one reads the same.
    assert_eq!(at(3), "");
    assert_eq!(at(16), "");
    assert_eq!(modmask_words(None), "");
    // jq's `tostring` takes a string mask at face value too.
    assert_eq!(modmask_words(Some(&json!("64"))), "SUPER");
}

#[test]
fn the_binds_leg_drops_what_cannot_be_drawn() {
    let recs = parse_binds(binds_json());
    // Five in, three out: a bind with no key cannot be drawn, and a bind
    // with no description has nothing to say.
    assert_eq!(recs.len(), 3);
    assert_eq!(recs[0], vec!["System", "SUPER", "T", "Terminal"]);
    assert_eq!(recs[1], vec!["System", "SUPER SHIFT", "F", "Fullscreen"]);
    // A non-array answer is jq's `empty`, not a parse error row.
    assert!(parse_binds("{}").is_empty());
    assert!(parse_binds("not json").is_empty());
    assert!(parse_binds("").is_empty());
}

#[test]
fn the_menu_leg_splits_on_the_arrow() {
    let out = "SUPER + K                          → Actions for the selected result\n\
               SHIFT CTRL + W                     → Close window\n\
               PRINT                              → Screenshot\n\
               no arrow here\n\
               SUPER + Z                          → \n\
               SUPER +                            → key fell off\n";
    let recs = parse_menu(out);
    assert_eq!(
        recs,
        vec![
            record("System", "SUPER", "K", "Actions for the selected result"),
            record("System", "SHIFT CTRL", "W", "Close window"),
            record("System", "", "PRINT", "Screenshot"),
            // A combo that is all modifiers keeps the husk as the key —
            // the awk only ever drops an empty key, and "SUPER +" is not.
            record("System", "", "SUPER +", "key fell off"),
        ]
    );
}

#[test]
fn modifier_order_is_canonical() {
    // "CTRL SHIFT" and "SHIFT CTRL" are one family, not two.
    let recs = vec![
        record("System", "CTRL SHIFT", "P", "one way"),
        record("System", "SHIFT CTRL", "P", "the other"),
    ];
    let rows = rows(&recs, "");
    // modweight orders SHIFT before CTRL, whatever the source printed.
    assert_eq!(rows[0]["group"], json!("Shift Ctrl"));
    assert_eq!(rows[0]["keys"], json!(["Shift", "Ctrl", "P"]));
    assert_eq!(rows[1]["group"], json!("Shift Ctrl"));
    assert_eq!(rows[0]["combo"], json!("Shift + Ctrl + P"));
    // Every piece gets its own plus in the hyprland.lua spelling.
    assert_eq!(rows[0]["config"], json!("SHIFT + CTRL + P"));
}

#[test]
fn key_names_draw_the_way_the_keycap_does() {
    assert_eq!(keydisplay("Return"), "Return");
    assert_eq!(keydisplay("RETURN"), "Return");
    assert_eq!(keydisplay("Escape"), "Esc");
    assert_eq!(keydisplay("k"), "K");
    assert_eq!(keydisplay("Left"), "←");
    assert_eq!(keydisplay("Prior"), "Page Up");
    assert_eq!(keydisplay("comma"), ",");
    assert_eq!(keydisplay("XF86AudioRaiseVolume"), "Audio Raise Volume");
    assert_eq!(keydisplay("XF86MonBrightnessUp"), "Mon Brightness Up");
    assert_eq!(keydisplay("mouse_down"), "Wheel ↓");
    // A multi-character name the table does not know is left alone.
    assert_eq!(keydisplay("1 – 9"), "1 – 9");
}

#[test]
fn aliases_are_the_names_people_type() {
    assert_eq!(keyaliases("Return"), "enter");
    assert_eq!(keyaliases("Escape"), "esc");
    assert_eq!(keyaliases("XF86AudioMute"), "media fn");
    assert_eq!(keyaliases("XF86Tools"), "fn");
    assert_eq!(keyaliases("K"), "");
}

#[test]
fn a_row_is_the_scripts_shape() {
    let recs = vec![record("System", "SUPER", "K", "Do a thing")];
    let rows = rows(&recs, "");
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!(r["id"], json!("System|SUPER + K|Do a thing"));
    assert_eq!(r["title"], json!("Do a thing"));
    assert_eq!(r["subtitle"], json!("Super + K"));
    assert_eq!(r["group"], json!("Super"));
    assert_eq!(r["keys"], json!(["Super", "K"]));
    assert_eq!(r["combo"], json!("Super + K"));
    assert_eq!(r["config"], json!("SUPER + K"));
    assert_eq!(r["scope"], json!("System"));
    // A system row wears no chip — "System" on every row is furniture.
    assert_eq!(r["accessory"], json!(""));
    assert_eq!(r["view"], json!("shortcuts"));
    assert_eq!(r["exec"], json!("printf %s SUPER\\ +\\ K | wl-copy"));
    assert_eq!(r["score"], json!(99999));
    let actions = r["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 3);
    assert_eq!(actions[0]["title"], json!("Copy Combination"));
    assert_eq!(actions[0]["shortcut"], json!("↵"));
    assert_eq!(actions[0]["exec"], r["exec"]);
    assert_eq!(actions[1]["title"], json!("Copy Action"));
    assert_eq!(
        actions[1]["exec"],
        json!("printf %s Do\\ a\\ thing | wl-copy")
    );
    assert_eq!(actions[2]["title"], json!("Copy Line"));
    assert_eq!(
        actions[2]["exec"],
        json!("printf %s Super\\ +\\ K\\ \\ →\\ \\ Do\\ a\\ thing | wl-copy")
    );
}

#[test]
fn super_leads_and_the_bare_keys_close() {
    let rows = rows(&records(), "");
    // Three system binds and seventeen launcher keys, Super family first,
    // Unmodified last.
    assert_eq!(rows.len(), 20);
    assert_eq!(rows[0]["title"], json!("Terminal"));
    assert_eq!(rows[0]["group"], json!("Super"));
    assert_eq!(rows[1]["title"], json!("Fullscreen"));
    assert_eq!(rows[1]["group"], json!("Super Shift"));
    // Shift Return leads the non-Super families, and the system's Ctrl K
    // ties its Oxy namesake on rank and key — the action's name breaks it.
    assert_eq!(rows[2]["group"], json!("Shift"));
    assert_eq!(
        rows[2]["title"],
        json!("Second action on the selected result")
    );
    assert_eq!(rows[4]["title"], json!("Actions for the selected result"));
    assert_eq!(rows[5]["title"], json!("Copy line up"));
    assert_eq!(rows[5]["scope"], json!("System"));
    // The unmodified family ends the keymap, single-char keys first.
    assert_eq!(rows[19]["group"], json!("Unmodified"));
    assert_eq!(rows[19]["title"], json!("Next result"));
    assert_eq!(rows[19]["keys"], json!(["Tab"]));
    assert_eq!(rows[18]["title"], json!("Run the selected result"));
    assert_eq!(rows[18]["keys"], json!(["Return"]));
}

#[test]
fn the_launcher_keys_say_when_the_system_owns_them() {
    let rows = rows(&records(), "");
    let oxy_k = rows
        .iter()
        .find(|r| r["scope"] == "Oxy" && r["keys"] == json!(["Ctrl", "K"]));
    // The fixture binds Ctrl K, so the launcher's own Ctrl K row says the
    // key may never arrive.
    assert_eq!(oxy_k.unwrap()["accessory"], json!("Oxy · also bound"));
    let oxy_p = rows
        .iter()
        .find(|r| r["scope"] == "Oxy" && r["keys"] == json!(["Ctrl", "P"]));
    assert_eq!(oxy_p.unwrap()["accessory"], json!("Oxy"));
}

#[test]
fn a_term_is_a_word_prefix_or_a_long_enough_substring() {
    // "super" is a word in the Super families' combos, "f" the F key's own
    // word — one row.
    let out = rows(&records(), "super f");
    assert_eq!(titles(&out), ["Fullscreen"]);

    // "win" is the alias SUPER carries, so it finds the Super family.
    let out = rows(&records(), "win");
    assert_eq!(titles(&out), ["Terminal", "Fullscreen"]);

    // "win k" needs a k-word too: none of the fixture's Super binds has
    // one, and the Oxy rows carry no "win" — silence.
    assert!(rows(&records(), "win k").is_empty());

    // One character can only ever lead a word: "k" finds the K rows, never
    // "workspace".
    let out = rows(&records(), "k");
    assert_eq!(
        titles(&out),
        ["Actions for the selected result", "Copy line up"]
    );

    // Three characters can sit anywhere in the hay.
    assert!(rows(&records(), "shot").is_empty());
    let out = rows(&records(), "term");
    assert_eq!(titles(&out), ["Terminal"]);

    // The keyalias path: "enter" is a name no source prints.
    let out = rows(&records(), "enter");
    assert_eq!(
        titles(&out),
        [
            "Second action on the selected result",
            "Ask the agent this question",
            "Run the selected result"
        ]
    );

    // The plus is punctuation, not a word: "ctrl+n" is "ctrl" and "n" —
    // and "n" leads "ninth" as well as the N key.
    let out = rows(&records(), "ctrl+n");
    assert_eq!(
        titles(&out),
        ["Next result", "Run the first to ninth result"]
    );
}

#[test]
fn the_launcher_keys_are_listed_whether_or_not_the_system_answers() {
    let oxy_only: Vec<Vec<String>> = OXY_BINDS
        .iter()
        .map(|(m, k, a)| record("Oxy", m, k, a))
        .collect();
    let rows = rows(&oxy_only, "");
    assert_eq!(rows.len(), 17);
    assert_eq!(rows[0]["group"], json!("Shift"));
    assert_eq!(rows[0]["accessory"], json!("Oxy"));
}
