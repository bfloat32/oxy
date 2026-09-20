use super::builtins::{builtin_actions, builtin_extensions, builtin_keywords};
use super::*;
use crate::registry::known_keywords;
use crate::settings::Quicklink;

fn link(keyword: &str) -> Quicklink {
    Quicklink {
        title: format!("link {keyword}"),
        subtitle: String::new(),
        keyword: keyword.into(),
        tags: vec![],
        url: format!("https://{keyword}.example/{{}}"),
        open: String::new(),
        glyph: String::new(),
    }
}

/// N1: a quicklink answers under its own keyword — `later:hi` scopes to
/// `later`, which the quicklinks extension claims through its aliases, so
/// the worker is asked and `arg_for` hands it `hi`.
#[test]
fn quicklink_keyword_routes_to_provider() {
    let links = vec![link("later")];
    let exts = builtin_extensions(&links);
    let ql = exts.iter().find(|e| e.id == "quicklinks").unwrap();
    assert!(
        ql.aliases.iter().any(|a| a == "later"),
        "the link's keyword must ride the extension's aliases"
    );

    let known = known_keywords(&exts, &builtin_keywords());
    let q = Query::parse("later:hi", 1, Some(&known));
    assert_eq!(q.scope, "later");
    assert!(q.routes_to(&ql.keyword, &ql.aliases));
    assert_eq!(q.arg_for(&ql.keyword, &ql.aliases), "hi");
}

/// The engine's half of N1: `claims` consults the same aliases, so the
/// dispatcher and the worker gate can never disagree about a link scope.
#[test]
fn quicklink_alias_survives_reload_and_disable() {
    // Editing quicklinks changes the synthetic extension's aliases, which
    // def_stamp fingerprints — a reload must see it as a redefinition.
    let before = builtin_extensions(&[link("later")]);
    let after = builtin_extensions(&[link("other")]);
    let ql_before = before.iter().find(|e| e.id == "quicklinks").unwrap();
    let ql_after = after.iter().find(|e| e.id == "quicklinks").unwrap();
    assert_ne!(
        crate::registry::def_stamp(ql_before),
        crate::registry::def_stamp(ql_after),
        "a quicklinks edit must read as a changed definition"
    );
}

/// Third-audit nit: `later:`'s chip names the link ("Added Later"), not
/// the transport ("Quicklinks") — the synthesized extension's aliases are
/// link keywords, so only its own keyword may claim the label.
#[test]
fn scope_label_names_the_link_not_quicklinks() {
    let mut engine = bare_engine();
    engine.extensions = Arc::new(builtin_extensions(&[link("later")]));
    engine.settings.quicklinks = vec![link("later")];
    let known = known_keywords(&engine.extensions, &builtin_keywords());
    let q = Query::parse("later:hi", 1, Some(&known));
    assert_eq!(engine.scope_label(&q), "link later");
    let q = Query::parse("quicklinks:x", 1, Some(&known));
    assert_eq!(engine.scope_label(&q), "Quicklinks");
}

/// A minimal engine for the in-process flows — no workers, and state
/// paths under the temp dir so `save_state`'s writes land nowhere real.
fn bare_engine() -> Engine {
    let scratch = std::env::temp_dir().join(format!("oxy-core-test-{}", std::process::id()));
    let (_cmd_tx, cmd_rx) = mpsc::channel(8);
    let (evt_tx, _evt_rx) = mpsc::channel(64);
    let (worker_tx, _worker_rx) = mpsc::channel(64);
    let shared = Arc::new(Shared {
        cache: std::sync::Mutex::new(crate::support::cache::Cache::default()),
        availability: std::sync::Mutex::new(crate::support::availability::Availability::default()),
        settings: RwLock::new(Arc::new(Settings::default())),
        registry: RwLock::new(Arc::new(Vec::new())),
        hello_keywords: std::sync::RwLock::new(Arc::new(Vec::new())),
    });
    let mut engine = Engine {
        cmd_rx,
        evt_tx,
        shared,
        settings: Settings::default(),
        extensions: Arc::new(builtin_extensions(&[])),
        workers: HashMap::new(),
        natives: HashMap::new(),
        native_for: Box::new(|_| None),
        worker_tx,
        state: crate::state::State {
            frecency: HashMap::new(),
            recents: Vec::new(),
            pins: HashMap::new(),
        },
        state_path: scratch.join("oxy-state.json"),
        frecency_path: scratch.join("oxy-frecency.json"),
        epoch: 0,
        raw: String::new(),
        query: Arc::new(Query::parse("", 0, None)),
        opened: true,
        opened_at: None,
        showing: HashSet::new(),
        buckets: HashMap::new(),
        worker_defs: HashMap::new(),
        waiting: HashSet::new(),
        rows: Vec::new(),
        pending_activate: None,
        pending_confirm: None,
        previewed: None,
        preview_revert: String::new(),
        clipboard_url: None,
        ask_task: None,
        ask_pending: None,
        ollama_up: false,
        latency: std::collections::HashMap::new(),
        usage: crate::state::usage::Usage::default(),
        ask_provider: None,
        llm: None,
        ask_probed: true,
        known: HashSet::new(),
    };
    engine.rebuild_known();
    engine
}

/// N13: the first Enter on `/clear-all` arms the confirm, and the engine's
/// own re-ask of `/clear-all ` must not clear it — that re-ask used to
/// run the old text through the walking-away rule and the arm died one
/// line after it was set.
#[tokio::test]
async fn confirmed_action_arms_and_confirms() {
    let mut engine = bare_engine();
    let action = builtin_actions()
        .into_iter()
        .find(|a| a.id == "clear-all")
        .unwrap();
    let row = Row::new("action:clear-all", "actions");

    engine.on_query("/clear").await;
    engine.run_action(&row, &action).await;
    assert_eq!(
        engine.pending_confirm.as_deref(),
        Some("clear-all"),
        "the arm must survive the engine re-asking the confirm text"
    );
    assert_eq!(engine.raw, "/clear-all ");

    // The second Enter is the answer: the action runs and disarms.
    engine.run_action(&row, &action).await;
    assert!(engine.pending_confirm.is_none());
}

/// N16: a preview chain reverts to the state you arrived in — the first
/// row's `revertExec`, not the last previewed row's. Moving between
/// previews runs nothing but the next preview.
#[tokio::test]
async fn preview_revert_is_the_first_rows() {
    let mut engine = bare_engine();
    let mut a = Row::new("a", "probe");
    a.preview_exec = "test-nop preview-a".into();
    a.revert_exec = "test-nop revert-a".into();
    let mut b = Row::new("b", "probe");
    b.preview_exec = "test-nop preview-b".into();
    b.revert_exec = "test-nop revert-b".into();
    engine.rows = vec![Arc::new(a), Arc::new(b)];

    engine.on_select("a");
    assert_eq!(engine.preview_revert, "test-nop revert-a");
    engine.on_select("b");
    assert_eq!(
        engine.preview_revert, "test-nop revert-a",
        "the revert to run is the first previewed row's — the state you \
         arrived in, not the one you last previewed"
    );
    engine.unpreview();
    assert!(engine.preview_revert.is_empty());
    assert!(engine.previewed.is_none());
}

/// N2's fingerprint: any field a gate or a row build reads flips it —
/// spot-check the ones the audit measured (search, when, keyword).
#[test]
fn def_stamp_covers_gate_fields() {
    let exts = builtin_extensions(&[]);
    let base = exts.iter().find(|e| e.id == "commands").unwrap();
    let mut changed = base.clone();
    changed.search = "echo different".into();
    assert_ne!(
        crate::registry::def_stamp(base),
        crate::registry::def_stamp(&changed)
    );
    let mut changed = base.clone();
    changed.when = "command -v definitely-absent-thing".into();
    assert_ne!(
        crate::registry::def_stamp(base),
        crate::registry::def_stamp(&changed)
    );
    let mut changed = base.clone();
    changed.debounce_ms += 1;
    assert_ne!(
        crate::registry::def_stamp(base),
        crate::registry::def_stamp(&changed)
    );
}

#[test]
fn the_slowest_providers_are_ranked_by_their_worst_answer() {
    use std::collections::HashMap;
    let mut latency: HashMap<String, crate::engine::Latency> = HashMap::new();
    latency.insert(
        "repo".into(),
        crate::engine::Latency {
            count: 2,
            total_ms: 1_600,
            max_ms: 900,
        },
    );
    latency.insert(
        "git".into(),
        crate::engine::Latency {
            count: 10,
            total_ms: 1_400,
            max_ms: 140,
        },
    );
    latency.insert(
        "emoji".into(),
        crate::engine::Latency {
            count: 0,
            total_ms: 0,
            max_ms: 0,
        },
    );

    let top = crate::engine::slowest(&latency, 3);
    assert_eq!(
        top.len(),
        2,
        "a provider that never ran is not slow, it is absent"
    );
    assert_eq!(top[0].0, "repo");
    assert_eq!(top[0].1.max_ms, 900);
    assert_eq!(top[1].0, "git");
    assert_eq!(top[1].1.mean_ms(), 140);

    // The cap is a cap.
    assert_eq!(crate::engine::slowest(&latency, 1).len(), 1);
}
