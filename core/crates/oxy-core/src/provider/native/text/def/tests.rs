use super::*;
use serde_json::json;

#[test]
fn term_cleanup_matches_the_script() {
    use super::word::clean_term;
    assert_eq!(clean_term("ephemeral"), Some("ephemeral".into()));
    assert_eq!(clean_term("Ephemeral"), Some("Ephemeral".into()));
    assert_eq!(clean_term("  ephemeral.  "), Some("ephemeral".into()));
    assert_eq!(clean_term("ephemeral?"), Some("ephemeral".into()));
    assert_eq!(clean_term("\"café\""), Some("café".into()));
    assert_eq!(clean_term("ice   cream"), Some("ice cream".into()));
    assert_eq!(
        clean_term("kick the bucket"),
        Some("kick the bucket".into())
    );
    // The gates.
    assert_eq!(clean_term(""), None);
    assert_eq!(clean_term("e"), None);
    assert_eq!(clean_term("the quick brown fox jumps"), None);
    assert_eq!(clean_term("../../etc/passwd"), None);
    assert_eq!(clean_term("a?b=c"), None);
    assert_eq!(clean_term("12345"), None);
    // A leading apostrophe is not a letter.
    assert_eq!(clean_term("'twas"), None);
}

#[test]
fn stems_are_the_scripts_order() {
    use super::word::stems;
    assert_eq!(stems("cats"), vec!["cat"]);
    assert_eq!(stems("berries"), vec!["berry"]);
    assert_eq!(stems("batches"), vec!["batch"]);
    assert_eq!(stems("glass"), Vec::<String>::new());
    assert_eq!(stems("happiest"), vec!["happy"]);
    assert_eq!(stems("biggest"), vec!["bigg", "bigge"]);
    assert_eq!(stems("happier"), vec!["happy"]);
    assert_eq!(stems("bigger"), vec!["bigg", "bigge"]);
    assert_eq!(stems("running"), vec!["runn", "runne", "run"]);
    assert_eq!(stems("making"), vec!["mak", "make"]);
    assert_eq!(stems("occured"), vec!["occur", "occure"]);
    assert_eq!(stems("stopped"), vec!["stopp", "stoppe", "stop"]);
    // *ied under five characters offers nothing.
    assert_eq!(stems("died"), Vec::<String>::new());
    assert_eq!(stems("tried"), vec!["try"]);
    assert_eq!(stems("quickly"), vec!["quick"]);
    assert_eq!(stems("happily"), vec!["happy"]);
    // `'s` first, then the plural case still fires.
    assert_eq!(stems("it's"), vec!["it", "it'"]);
    // Short words return early instead of stemming to nothing.
    assert_eq!(stems("red"), Vec::<String>::new());
    assert_eq!(stems("ring"), Vec::<String>::new());
}

#[test]
fn near_is_a_typo_away() {
    use super::word::near;
    assert!(near("recieve", "receive"));
    assert!(near("definately", "definitely"));
    // A keyboard mash is not a misspelling of anything.
    assert!(!near("asdfghjkl", "oesophageal"));
    // First letters must match and the length gap is two at most.
    assert!(!near("xylophone", "telephone"));
    assert!(!near("cat", "category"));
}

#[test]
fn entry_validation_is_the_jq_gate() {
    assert!(is_entry("[{\"word\":\"x\"}]"));
    assert!(!is_entry("[]"));
    // The API's "no definitions found" is an object, not an array.
    assert!(!is_entry("{\"title\":\"No Definitions Found\"}"));
    assert!(!is_entry(""));
    assert!(!is_entry("not json"));
}

/// A trimmed-down api.dictionaryapi.dev answer: two meanings, an example,
/// sense-level and meaning-level synonyms to merge.
fn fixture_body() -> Value {
    json!([{
        "word": "happy",
        "phonetic": "/ˈhæpi/",
        "phonetics": [{ "text": "/ˈhæpi/" }],
        "meanings": [
            {
                "partOfSpeech": "adjective",
                "definitions": [
                    { "definition": "Feeling joy.",
                      "example": "She was happy.",
                      "synonyms": ["glad"],
                      "antonyms": ["sad"] },
                    { "definition": "Content or satisfied.",
                      "synonyms": ["content"] }
                ],
                "synonyms": ["joyful", "glad"],
                "antonyms": ["unhappy"]
            },
            {
                "partOfSpeech": "verb",
                "definitions": [{ "definition": "To make happy." }]
            }
        ]
    }])
}

#[test]
fn senses_flatten_entry_meaning_definition() {
    let s = super::rows::define_rows(&fixture_body(), "").unwrap();
    assert_eq!(s.len(), 4); // three senses + the thesaurus row
    assert_eq!(s[0]["id"], "def-adjective-0");
    assert_eq!(s[0]["title"], "happy  ·  adjective");
    assert_eq!(s[0]["subtitle"], "Feeling joy.");
    // def-level + meaning-level synonyms, sorted and deduplicated.
    assert_eq!(s[0]["detail"], "syn. glad, joyful");
    assert_eq!(s[0]["score"], 95000);
    assert_eq!(s[0]["view"], "split");
    assert_eq!(s[0]["exec"], "printf %s 'Feeling joy.' | wl-copy");
    assert_eq!(
        s[0]["preview"],
        "happy   /ˈhæpi/   adjective\n\nFeeling joy.\n\n“She was happy.”\n\nSynonyms: glad, joyful\nAntonyms: sad, unhappy"
    );
    // Second sense of a meaning carries its number in the title, and the
    // meaning-level synonym list merges into every definition under it.
    assert_eq!(s[1]["title"], "happy  ·  adjective  2");
    assert_eq!(s[1]["detail"], "syn. content, glad, joyful");
    // No synonyms: detail is the part of speech, no fourth action.
    assert_eq!(s[2]["detail"], "verb");
    assert_eq!(s[2]["actions"].as_array().unwrap().len(), 3);
}

#[test]
fn the_row_actions_are_the_scripts() {
    let s = super::rows::define_rows(&fixture_body(), "").unwrap();
    let actions = s[0]["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 4);
    assert_eq!(actions[0]["title"], "Copy Definition");
    assert_eq!(actions[0]["shortcut"], "↵");
    assert_eq!(actions[0]["exec"], "printf %s 'Feeling joy.' | wl-copy");
    assert_eq!(actions[1]["title"], "Copy Word");
    assert_eq!(actions[1]["exec"], "printf %s 'happy' | wl-copy");
    assert_eq!(
        actions[2]["exec"],
        "printf %s 'happy (adjective): Feeling joy.' | wl-copy"
    );
    assert_eq!(actions[3]["exec"], "printf %s 'glad, joyful' | wl-copy");
}

#[test]
fn the_note_replaces_the_detail() {
    let s = super::rows::define_rows(&fixture_body(), "No entry for “happier” — showing “happy”")
        .unwrap();
    assert_eq!(s[0]["detail"], "No entry for “happier” — showing “happy”");
    assert!(
        s[0]["preview"]
            .as_str()
            .unwrap()
            .starts_with("No entry for “happier” — showing “happy”\n\nhappy   /ˈhæpi/   adjective")
    );
}

#[test]
fn the_thesaurus_row_comes_last() {
    let rows = super::rows::define_rows(&fixture_body(), "").unwrap();
    let last = rows.last().unwrap();
    assert_eq!(last["id"], "def-synonyms");
    assert_eq!(last["title"], "happy  ·  synonyms");
    assert_eq!(last["subtitle"], "content, glad, joyful");
    assert_eq!(last["detail"], "Thesaurus");
    assert_eq!(last["score"], 80000);
    assert_eq!(
        last["preview"],
        "Synonyms for happy\n\ncontent\nglad\njoyful"
    );
    // No synonyms anywhere: no thesaurus row.
    let bare = json!([{ "word": "x", "meanings": [{ "partOfSpeech": "noun",
        "definitions": [{ "definition": "a thing" }] }] }]);
    assert_eq!(super::rows::define_rows(&bare, "").unwrap().len(), 1);
    // Definitions without text never become rows.
    let empty = json!([{ "word": "x", "meanings": [{ "partOfSpeech": "noun",
        "definitions": [{ "example": "no text here" }] }] }]);
    assert_eq!(super::rows::define_rows(&empty, "").unwrap().len(), 0);
}

#[test]
fn malformed_shapes_are_silence_like_jq_dying() {
    // A numeric word, a string where the meanings list belongs — each is
    // where the jq program dies and the script prints nothing.
    assert!(super::rows::define_rows(&json!([{ "word": 5 }]), "").is_none());
    assert!(super::rows::define_rows(&json!([{ "meanings": "noun" }]), "").is_none());
    assert!(super::rows::define_rows(&json!({ "word": "x" }), "").is_none());
}

#[test]
fn the_two_no_answer_rows_say_what_the_script_says() {
    let off = super::rows::offline_row("kubernetes");
    assert_eq!(off["id"], "def-offline");
    assert_eq!(off["title"], "Dictionary unreachable");
    assert_eq!(off["subtitle"], "Could not look up “kubernetes”");
    assert_eq!(
        off["detail"],
        "No network, or api.dictionaryapi.dev is down"
    );
    assert_eq!(
        off["preview"],
        "Could not reach api.dictionaryapi.dev to look up “kubernetes”.\n\nWords looked up before are still answered from the cache."
    );
    assert_eq!(off["score"], 90000);
    assert_eq!(off["view"], "split");
    assert!(off["actions"].as_array().unwrap().is_empty());

    let none = super::rows::no_def_row("kick the bucket", "kick the bucket");
    assert_eq!(none["id"], "def-none");
    assert_eq!(none["title"], "No definition for \"kick the bucket\"");
    assert_eq!(none["subtitle"], "Dictionary");
    assert_eq!(none["detail"], "Enter searches the web instead");
    assert_eq!(
        none["preview"],
        "No dictionary entry for \"kick the bucket\".\n\nEnter searches the web for it."
    );
    assert_eq!(
        none["exec"],
        "xdg-open https://www.google.com/search\\?q=define+kick+the+bucket"
    );
    assert_eq!(none["actions"][0]["exec"], none["exec"]);
    assert_eq!(none["actions"][0]["shortcut"], "↵");
}
