//! The `herdr:` suite: canned `herdr session list` and `api snapshot`
//! answers through the same parse the provider runs — herdr does not exist
//! on every machine the tests do, so the fixtures are the verification.

use serde_json::{Map, Value, json};

use super::rows::{self, Snapshot};

/// The `default` session's snapshot: four agents across the bands — one
/// blocked with a question in `state_labels`, one working, one `done` with
/// a fallback name (`title`, not `name`), one unclassified — over three
/// workspaces, plus a fourth workspace with nothing running in it.
fn default_snap() -> Value {
    json!({
        "agents": [
            { "pane_id": "p1", "workspace_id": "w1", "tab_id": "t1",
              "agent_status": "blocked", "state_change_seq": 3,
              "name": "reviewer", "agent": "claude", "display_agent": "Claude",
              "state_labels": { "blocked": "Approve the plan?" },
              "foreground_cwd": "/home/u/proj", "cwd": "/home/u/proj",
              "terminal_title_stripped": "vim", "focused": false },
            { "pane_id": "p2", "workspace_id": "w1", "tab_id": "t1",
              "agent_status": "working", "state_change_seq": 1,
              "name": "builder", "agent": "codex", "state_labels": {},
              "cwd": "/tmp", "focused": true },
            { "pane_id": "p3", "workspace_id": "w2", "tab_id": "t2",
              "agent_status": "done", "state_change_seq": 2,
              "title": "docs", "agent": "claude",
              "state_labels": { "done": "Finished: write docs" } },
            { "pane_id": "p4", "workspace_id": "w3", "tab_id": "t3",
              "agent_status": "mystery", "state_change_seq": 0,
              "agent": "zsh" }
        ],
        "workspaces": [
            { "workspace_id": "w1", "label": "core", "number": 1, "tab_count": 2, "focused": true },
            { "workspace_id": "w2", "number": 2, "tab_count": 1 },
            { "workspace_id": "w3", "number": 3 },
            { "workspace_id": "w4", "label": "empty proj", "number": 4 }
        ],
        "tabs": [
            { "tab_id": "t1", "label": "main", "number": 1 },
            { "tab_id": "t2", "number": 2 },
            { "tab_id": "t3", "label": "misc" }
        ]
    })
}

/// A second session with one idle agent in its only workspace.
fn other_snap() -> Value {
    json!({
        "agents": [
            { "pane_id": "q1", "workspace_id": "w9", "tab_id": "t9",
              "agent_status": "idle", "name": "linter", "agent": "shell" }
        ],
        "workspaces": [ { "workspace_id": "w9", "number": 1 } ],
        "tabs": []
    })
}

fn sessions() -> Vec<(String, Value)> {
    vec![
        ("default".to_string(), default_snap()),
        ("other".to_string(), other_snap()),
    ]
}

fn empty_seen() -> Map<String, Value> {
    Map::new()
}

#[test]
fn session_list_keeps_only_the_running_names() {
    let list = "name      state     uptime\ndefault   running   2h\nold       stopped   -\nproj      running   5m\n";
    assert_eq!(rows::running_sessions(list), ["default", "proj"]);
}

#[test]
fn session_list_with_no_table_is_no_names() {
    assert!(rows::running_sessions("").is_empty());
    assert!(rows::running_sessions("error: unknown command\n").is_empty());
}

#[test]
fn classify_reads_the_payload_not_the_shape() {
    assert!(matches!(
        rows::classify("{\"result\":{\"snapshot\":{\"agents\":[]}}}"),
        Snapshot::Shot(_)
    ));
    // An error answer is JSON too, and a stale socket answers `null`.
    assert!(matches!(
        rows::classify("{\"error\":\"no server\"}"),
        Snapshot::Skip
    ));
    assert!(matches!(
        rows::classify("{\"result\":{\"snapshot\":null}}"),
        Snapshot::Skip
    ));
    assert!(matches!(rows::classify(""), Snapshot::Skip));
    // Contains the literal but is not JSON, or cannot be indexed — both are
    // the jq death.
    assert!(matches!(
        rows::classify("{\"result\": \"snapshot\""),
        Snapshot::Poison
    ));
    assert!(matches!(rows::classify("[\"snapshot\"]"), Snapshot::Poison));
    assert!(matches!(
        rows::classify("{\"result\":\"snapshot\"}"),
        Snapshot::Poison
    ));
}

#[test]
fn rows_sort_by_band_then_shrink_to_24() {
    let (state, out) = rows::build(&sessions(), &empty_seen(), 1000, "/home/u", "").unwrap();
    // blocked, done, working, idle, unclassified, then the workspace with
    // nothing in it.
    let ids: Vec<&str> = out.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        [
            "herdr:default:p1",
            "herdr:default:p3",
            "herdr:default:p2",
            "herdr:other:q1",
            "herdr:default:p4",
            "herdr:default:w4",
        ]
    );
    let bands: Vec<i64> = out.iter().map(|r| r["band"].as_i64().unwrap()).collect();
    assert_eq!(bands, [0, 1, 2, 3, 4, 5]);
    // Every agent is a first sighting with an empty seen map.
    assert!(out.iter().all(|r| r["since"] == json!(-1)));
    assert_eq!(state.len(), 5);
}

#[test]
fn the_first_row_carries_the_counts() {
    let (_, out) = rows::build(&sessions(), &empty_seen(), 1000, "/home/u", "").unwrap();
    assert_eq!(
        out[0]["counts"],
        json!({
            "blocked": 1, "done": 1, "working": 1, "idle": 1, "unknown": 1,
            "agents": 5, "spaces": 6, "sessions": 2, "matched": 6,
        })
    );
    assert_eq!(out[1]["counts"], Value::Null);
    // `sessions` rides every row.
    assert!(out.iter().all(|r| r["sessions"] == json!(2)));
}

#[test]
fn a_row_is_the_scripts_emit_field_for_field() {
    let (_, out) = rows::build(&sessions(), &empty_seen(), 1000, "/home/u", "").unwrap();
    let r = &out[0];
    assert_eq!(r["view"], "herdr");
    assert_eq!(r["title"], "reviewer");
    assert_eq!(r["subtitle"], "blocked");
    assert_eq!(r["status"], "blocked");
    assert_eq!(r["kind"], "Claude");
    assert_eq!(r["note"], "Approve the plan?");
    assert_eq!(r["what"], "vim");
    assert_eq!(r["path"], "~/proj");
    assert_eq!(r["detail"], "~/proj");
    assert_eq!(r["paneId"], "p1");
    assert_eq!(r["wsLabel"], "core");
    assert_eq!(r["tabLabel"], "main");
    assert_eq!(r["tabCount"], 2);
    assert_eq!(r["here"], false);
    assert_eq!(r["score"], 90000);
    assert_eq!(r["exec"], "oxy-herdr --focus 'default' agent 'p1'");
    let actions = r["actions"].as_array().unwrap();
    assert_eq!(actions[0]["title"], "Go To Agent");
    assert_eq!(actions[0]["shortcut"], "↵");
    assert_eq!(
        actions[1]["exec"], "printf %s 'p1' | wl-copy",
        "Copy Pane ID"
    );
}

#[test]
fn scores_step_down_a_hundred_a_row() {
    let (_, out) = rows::build(&sessions(), &empty_seen(), 1000, "/home/u", "").unwrap();
    let scores: Vec<i64> = out.iter().map(|r| r["score"].as_i64().unwrap()).collect();
    assert_eq!(scores, [90000, 89900, 89800, 89700, 89600, 89500]);
}

#[test]
fn a_workspace_with_no_agent_is_a_band_five_row() {
    let (_, out) = rows::build(&sessions(), &empty_seen(), 1000, "/home/u", "").unwrap();
    let r = out.last().unwrap();
    assert_eq!(r["id"], "herdr:default:w4");
    assert_eq!(r["band"], 5);
    assert_eq!(r["status"], "");
    assert_eq!(r["name"], "empty proj");
    // An empty status reads as "workspace" in the subtitle.
    assert_eq!(r["subtitle"], "workspace");
    assert_eq!(r["wsLabel"], "empty proj");
    assert_eq!(r["tabLabel"], "");
    assert_eq!(r["here"], false);
    assert_eq!(
        r["exec"], "oxy-herdr --focus 'default' workspace 'w4'",
        "the workspace kind drives herdr workspace focus"
    );
    // No pane to copy: the only action is the go-to.
    let actions = r["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0]["title"], "Go To Workspace");
}

#[test]
fn the_fallbacks_keep_a_row_nameless_never() {
    let (_, out) = rows::build(&sessions(), &empty_seen(), 1000, "/home/u", "").unwrap();
    // p3 has `title` but no `name`: the label falls back one field.
    let p3 = out.iter().find(|r| r["id"] == "herdr:default:p3").unwrap();
    assert_eq!(p3["name"], "docs");
    assert_eq!(p3["title"], "docs");
    assert_eq!(p3["note"], "Finished: write docs");
    // p4 has only `agent`: the name is the kind it runs, and `mystery` is
    // the unclassified band.
    let p4 = out.iter().find(|r| r["id"] == "herdr:default:p4").unwrap();
    assert_eq!(p4["name"], "zsh");
    assert_eq!(p4["kind"], "zsh");
    assert_eq!(p4["note"], "");
    // Unlabeled workspaces and tabs read as their numbers.
    assert_eq!(p3["wsLabel"], "w2");
    assert_eq!(p3["tabLabel"], "2");
}

#[test]
fn seen_gives_a_wait_its_duration() {
    let mut seen = Map::new();
    seen.insert(
        "default|p1".to_string(),
        json!({"st": "blocked", "sq": 3, "at": 900, "known": true}),
    );
    let (state, out) = rows::build(&sessions(), &seen, 1000, "/home/u", "").unwrap();
    assert_eq!(out[0]["since"], 100);
    // The same status and counter keeps the old stamp — and the old entry.
    assert_eq!(state["default|p1"]["at"], 900);
}

#[test]
fn a_changed_status_restarts_the_clock() {
    let mut seen = Map::new();
    seen.insert(
        "default|p1".to_string(),
        json!({"st": "working", "sq": 3, "at": 900, "known": true}),
    );
    let (state, out) = rows::build(&sessions(), &seen, 1000, "/home/u", "").unwrap();
    // Known before, changed now: the wait started this run — 0, not 100.
    assert_eq!(out[0]["since"], 0);
    assert_eq!(
        state["default|p1"],
        json!({"st": "blocked", "sq": 3, "at": 1000, "known": true})
    );
}

#[test]
fn the_longest_wait_leads_its_band() {
    let snap = json!({
        "agents": [
            { "pane_id": "new", "workspace_id": "w", "tab_id": "t",
              "agent_status": "blocked", "state_change_seq": 1, "name": "new q" },
            { "pane_id": "old", "workspace_id": "w", "tab_id": "t",
              "agent_status": "blocked", "state_change_seq": 1, "name": "old q" }
        ],
        "workspaces": [], "tabs": []
    });
    let mut seen = Map::new();
    seen.insert(
        "s|new".to_string(),
        json!({"st": "blocked", "sq": 1, "at": 900, "known": true}),
    );
    seen.insert(
        "s|old".to_string(),
        json!({"st": "blocked", "sq": 1, "at": 500, "known": true}),
    );
    let sessions = vec![("s".to_string(), snap)];
    let (_, out) = rows::build(&sessions, &seen, 1000, "", "").unwrap();
    assert_eq!(out[0]["id"], "herdr:s:old");
    assert_eq!(out[0]["since"], 500);
    assert_eq!(out[1]["id"], "herdr:s:new");
    assert_eq!(out[1]["since"], 100);
}

#[test]
fn the_needle_filters_across_the_row_fields() {
    let (_, out) = rows::build(&sessions(), &empty_seen(), 1000, "/home/u", "linter").unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["id"], "herdr:other:q1");
    assert_eq!(out[0]["counts"]["matched"], 1);

    // Session name is in the haystack too.
    let (_, out) = rows::build(&sessions(), &empty_seen(), 1000, "/home/u", "other").unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["id"], "herdr:other:q1");

    // A workspace label reaches its agents.
    let (_, out) = rows::build(&sessions(), &empty_seen(), 1000, "/home/u", "core").unwrap();
    assert!(out.iter().all(|r| r["wsLabel"] == "core"));
    assert_eq!(out.len(), 2);
}

#[test]
fn a_query_that_matches_nothing_still_returns_state() {
    // "A run that matched nothing still writes the state": build answers
    // the new seen map with an empty row set — the caller writes it.
    let (state, out) = rows::build(&sessions(), &empty_seen(), 1000, "/home/u", "zzzz").unwrap();
    assert!(out.is_empty());
    assert_eq!(state.len(), 5);
}

#[test]
fn what_jq_could_not_compute_is_silence() {
    // An agent that is not an object dies on `. + { session }`.
    let bad = vec![("s".to_string(), json!({"agents": [5]}))];
    assert!(rows::build(&bad, &empty_seen(), 1000, "", "").is_none());

    // A scalar where `state_labels` is indexed.
    let bad = vec![(
        "s".to_string(),
        json!({"agents": [{"pane_id": "p", "workspace_id": "w", "tab_id": "t",
                          "agent_status": "idle", "state_labels": 5}]}),
    )];
    assert!(rows::build(&bad, &empty_seen(), 1000, "", "").is_none());

    // A poisoned seen entry — `$prev.st` on a number.
    let good = vec![(
        "s".to_string(),
        json!({"agents": [{"pane_id": "p", "workspace_id": "w", "tab_id": "t",
                          "agent_status": "idle"}]}),
    )];
    let mut seen = Map::new();
    seen.insert("s|p".to_string(), json!(7));
    assert!(rows::build(&good, &seen, 1000, "", "").is_none());
    // …and the same build over the retry's `{}` answers fine.
    assert!(rows::build(&good, &empty_seen(), 1000, "", "").is_some());
}
