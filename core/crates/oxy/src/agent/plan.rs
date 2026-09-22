//! The one certain reading. Some sentences are already a command — "open
//! four terminals here" has exactly one meaning, the desk verbs do it in a
//! fixed order, and handing it to a model turns that certainty into a coin
//! flip. A short list of shapes is matched here and run directly; everything
//! else still goes to the agent.
//!
//! Three rules keep this from becoming a parser (the script's own):
//!
//!   * It matches whole sentences. A pattern has to consume every word.
//!   * Two readings means no match — nothing here asks the reader to guess.
//!   * It only names what it was told to name: no new workspace unless the
//!     sentence says so.
//!
//! Every system sentence ends up calling the same command the keyword for it
//! already calls: `vol:` sets a level with `oxy-volume set`, `bri:` with
//! `omarchy-brightness-display`, `theme:` with `omarchy theme set`.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use crate::agent::{self, desk};

#[derive(Clone)]
pub enum StepKind {
    /// A `desk` verb, argv spelled the way `cmd_desk` takes it.
    Argv(Vec<String>),
    /// A bare command line to run (the system sentences' scripts).
    Run(Vec<String>),
    /// Speaker mute, as a state rather than a toggle.
    Mute(bool),
    /// A tile step: maybe split the biggest pane first, then open `app`.
    Tile { split: bool, app: String },
}

#[derive(Clone)]
pub struct Step {
    pub say: String,
    pub kind: StepKind,
}

pub struct Plan {
    pub steps: Vec<Step>,
    pub done: String,
}

fn numbers(word: &str) -> Option<i64> {
    match word {
        "one" => Some(1),
        "two" => Some(2),
        "three" => Some(3),
        "four" => Some(4),
        "five" => Some(5),
        "six" => Some(6),
        "seven" => Some(7),
        "eight" => Some(8),
        _ => None,
    }
}

const COUNT: &str = r"(?P<n>[1-8]|one|two|three|four|five|six|seven|eight)";
// One word. An application whose name is two ("sublime text") is a sentence
// with a space in it and a guess about where it ends, and a guess is what
// this list exists to avoid.
const APP: &str = r"(?P<app>[a-z0-9][a-z0-9._+-]{0,31})";
// "here" is the only place this understands, and it is also the default.
const HERE: &str = r"(?: here| on this workspace| in this workspace| on this screen)?";
const WS: &str = r"(?P<ws>[1-9]|10)";

/// The sentence shapes, compiled once — `re.fullmatch` gets the module cache,
/// these get a `LazyLock`, and `search` stays cheap per keystroke.
struct Pats {
    open_app: fancy_regex::Regex,
    count_app: fancy_regex::Regex,
    split_count: fancy_regex::Regex,
    new_ws: fancy_regex::Regex,
    ws_tile: fancy_regex::Regex,
    go_ws: fancy_regex::Regex,
    move_ws: fancy_regex::Regex,
    focus_dir: fancy_regex::Regex,
    vol_level: fancy_regex::Regex,
    vol_step: fancy_regex::Regex,
    mute: fancy_regex::Regex,
    bright: fancy_regex::Regex,
    shot: fancy_regex::Regex,
    lock: fancy_regex::Regex,
    theme: fancy_regex::Regex,
    bt: fancy_regex::Regex,
}

fn anchored(pat: &str) -> fancy_regex::Regex {
    // `re.fullmatch`: the pattern has to consume every word.
    fancy_regex::Regex::new(&format!(r"\A(?:{pat})\z")).expect("plan regex compiles")
}

static PATS: LazyLock<Pats> = LazyLock::new(|| Pats {
    open_app: anchored(&format!(r"(?:open|launch|start) (?:a |an |the )?{APP}")),
    count_app: anchored(&format!(r"(?:open |launch |start )?{COUNT} {APP}s?{HERE}")),
    split_count: anchored(&format!(
        r"split (?:this|the|my) (?:screen|workspace|window) (?:in|into) {COUNT} {APP}s?"
    )),
    new_ws: anchored(
        r"(?:open|make|start|go to|give me) (?:a |an )?(?:new |fresh |empty )+workspace",
    ),
    ws_tile: anchored(&format!(
        r"(?:open|make|start) (?:a |an )?(?:new |fresh |empty )+workspace and (?:split it into|open|put) {COUNT} {APP}s?(?: in it)?"
    )),
    go_ws: anchored(&format!(r"(?:go to|switch to|show me|open) workspace {WS}")),
    move_ws: anchored(&format!(
        r"(?:move|send) (?:this|the current|the focused|the active) window to workspace {WS}"
    )),
    focus_dir: anchored(
        r"(?:focus|go)(?: to)? (?:the )?(?:window (?:to the |on the |)?)?(?P<dir>left|right|up|down|above|below)",
    ),
    vol_level: anchored(
        r"(?:set |turn |put )?(?:the )?(?P<who>mic|microphone|input|volume|sound) ?(?:volume )?(?:to |at |on )?(?P<n>\d{1,3})%?",
    ),
    vol_step: anchored(
        r"(?:turn )?(?:the )?(?:volume|sound) (?P<way>up|down)|(?P<w2>louder|quieter)",
    ),
    mute: anchored(r"(?P<un>un)?mute(?: the (?:sound|volume|speakers|audio))?"),
    bright: anchored(
        r"(?:set |turn |put )?(?:the )?(?:screen |display )?brightness (?:to |at |on )?(?P<n>\d{1,3})%?",
    ),
    shot: anchored(
        r"(?:take |grab |capture )?a? ?screenshot(?: of (?:the )?(?P<what>whole screen|screen|everything|this window|the active window))?",
    ),
    lock: anchored(r"lock (?:the |my )?(?:screen|computer|session)"),
    theme: anchored(
        r"(?:switch|change|set) (?:the )?theme to (?:the )?(?P<name>.{1,40}?)(?: theme)?|(?:switch|change) to (?:the )?(?P<n2>.{1,40}?) theme",
    ),
    bt: anchored(r"turn (?:bluetooth (?P<a>on|off)|(?P<b>on|off) bluetooth)"),
});

fn fullmatch<'t>(re: &fancy_regex::Regex, text: &'t str) -> Option<fancy_regex::Captures<'t>> {
    re.captures(text).ok().flatten()
}

fn cap<'t>(caps: &fancy_regex::Captures<'t>, name: &str) -> &'t str {
    caps.name(name).map(|m| m.as_str()).unwrap_or("")
}

fn dir_of(word: &str) -> Option<&'static str> {
    match word {
        "left" => Some("l"),
        "right" => Some("r"),
        "up" | "above" => Some("u"),
        "down" | "below" => Some("d"),
        _ => None,
    }
}

/// Whether this word names something that starts. The whole guard on the
/// fast path for applications: if the name does not resolve to a command
/// here, the sentence is not a certainty and the agent gets it. Memoised
/// because the daemon answers this for every keystroke.
fn app_known(name: &str) -> bool {
    static APPS: LazyLock<Mutex<HashMap<String, bool>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let key = name.split_whitespace().collect::<Vec<_>>().join(" ");
    *APPS
        .lock()
        .unwrap()
        .entry(key.clone())
        .or_insert_with(|| desk::app_command(&key).is_some())
}

/// `terminals` is `terminal` when there are four of them. Only ever tried
/// when the plural does not resolve on its own, so an application whose own
/// name ends in s keeps it.
fn app_word(word: &str, count: i64) -> Option<String> {
    let word = word.split_whitespace().collect::<Vec<_>>().join(" ");
    if app_known(&word) {
        return Some(word);
    }
    if count > 1 && word.ends_with('s') && app_known(&word[..word.len() - 1]) {
        return Some(word[..word.len() - 1].to_string());
    }
    None
}

fn count_of(text: &str) -> i64 {
    let text = text.to_lowercase();
    numbers(&text).unwrap_or_else(|| text.parse::<i64>().unwrap_or(0))
}

/// One space between words, no trailing punctuation, no `please`. Anything
/// more forgiving than this is the beginning of a parser.
fn normal(sentence: &str) -> String {
    let mut text = sentence
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    while matches!(text.chars().last(), Some('.') | Some('!') | Some('?')) {
        text.pop();
    }
    if let Some(rest) = text.strip_prefix("please")
        && rest.starts_with(|c: char| c.is_whitespace())
    {
        text = rest.trim_start().to_string();
    }
    if let Some(rest) = text.strip_suffix("please")
        && rest.ends_with(|c: char| c.is_whitespace())
    {
        text = rest.trim_end().to_string();
    }
    text.trim().to_string()
}

/// Opening n of something is n steps, not one. The card then names the
/// window it is on rather than sitting on `Open 4 terminals` for eight
/// seconds, and a run that dies halfway says how far it got.
fn tile_steps(count: i64, app: &str) -> Vec<Step> {
    (0..count)
        .map(|i| Step {
            say: format!(
                "Open {app}{}",
                if count == 1 {
                    String::new()
                } else {
                    format!(" {} of {count}", i + 1)
                }
            ),
            kind: StepKind::Tile {
                split: i > 0,
                app: app.to_string(),
            },
        })
        .collect()
}

/// The steps this sentence certainly means, or None. Pure apart from the
/// application/theme lists: `search` can call it per keystroke and the
/// answer is the same every time it is asked.
pub async fn plan_for(sentence: &str) -> Option<Plan> {
    let text = normal(sentence);
    if text.is_empty() {
        return None;
    }

    // open one application
    if let Some(m) = fullmatch(&PATS.open_app, &text)
        && let Some(app) = app_word(cap(&m, "app"), 1)
    {
        return Some(Plan {
            steps: vec![Step {
                say: format!("Open {app}"),
                kind: StepKind::Argv(vec!["open".into(), app.clone()]),
            }],
            done: format!("{app} on workspace {{ws}}"),
        });
    }

    // n of something, here, which is where you already are
    let m = fullmatch(&PATS.count_app, &text).or_else(|| {
        // The application is named on purpose: "split this screen into four"
        // could be four panes of anything, and a default is a guess.
        fullmatch(&PATS.split_count, &text)
    });
    if let Some(m) = m {
        let count = count_of(cap(&m, "n"));
        if let Some(app) = app_word(cap(&m, "app"), count)
            && (1..=8).contains(&count)
        {
            return Some(Plan {
                steps: tile_steps(count, &app),
                done: format!(
                    "{count} {app}{} on workspace {{ws}}",
                    if count == 1 { "" } else { "s" }
                ),
            });
        }
    }

    // a workspace with nothing on it, because that is what was asked for
    if fullmatch(&PATS.new_ws, &text).is_some() {
        return Some(Plan {
            steps: vec![Step {
                say: "Go to an empty workspace".into(),
                kind: StepKind::Argv(vec!["empty".into()]),
            }],
            done: "workspace {ws}, with nothing on it".into(),
        });
    }

    // the example on the empty card: a new workspace, then fill it
    if let Some(m) = fullmatch(&PATS.ws_tile, &text) {
        let count = count_of(cap(&m, "n"));
        if let Some(app) = app_word(cap(&m, "app"), count)
            && (1..=8).contains(&count)
        {
            let mut steps = vec![Step {
                say: "Go to an empty workspace".into(),
                kind: StepKind::Argv(vec!["empty".into()]),
            }];
            steps.extend(tile_steps(count, &app));
            return Some(Plan {
                steps,
                done: format!(
                    "{count} {app}{} on workspace {{ws}}",
                    if count == 1 { "" } else { "s" }
                ),
            });
        }
    }

    // go to a numbered one
    if let Some(m) = fullmatch(&PATS.go_ws, &text) {
        let ws = cap(&m, "ws");
        return Some(Plan {
            steps: vec![Step {
                say: format!("Go to workspace {ws}"),
                kind: StepKind::Argv(vec!["workspace".into(), ws.into()]),
            }],
            done: "workspace {ws}".into(),
        });
    }

    // send this window somewhere
    if let Some(m) = fullmatch(&PATS.move_ws, &text) {
        let ws = cap(&m, "ws");
        return Some(Plan {
            steps: vec![Step {
                say: format!("Move this window to workspace {ws}"),
                kind: StepKind::Argv(vec!["move".into(), ws.into()]),
            }],
            done: format!("moved to workspace {ws}"),
        });
    }

    // focus the neighbour
    if let Some(m) = fullmatch(&PATS.focus_dir, &text)
        && let Some(d) = dir_of(cap(&m, "dir"))
    {
        let where_ = cap(&m, "dir");
        return Some(Plan {
            steps: vec![Step {
                say: format!("Focus the window {where_}"),
                kind: StepKind::Argv(vec!["focus".into(), d.into()]),
            }],
            done: format!("focus moved {where_}"),
        });
    }

    system_plan(&text).await
}

/// A percentage, or None. Anything outside 0 to 100 is not a level, it is a
/// sentence this list does not understand.
fn level(text: &str) -> Option<i64> {
    let n: i64 = text.parse().ok()?;
    (0..=100).contains(&n).then_some(n)
}

fn one(say: String, run: Vec<String>, done: String) -> Plan {
    Plan {
        steps: vec![Step {
            say,
            kind: StepKind::Run(run),
        }],
        done,
    }
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// The installed themes, from the command that installs and sets them.
/// Re-read once a minute, because a theme installed while the daemon is up
/// should not need the daemon restarted to be typeable.
async fn themes() -> Vec<String> {
    static THEMES: LazyLock<Mutex<(f64, Vec<String>)>> =
        LazyLock::new(|| Mutex::new((0.0, Vec::new())));
    {
        let cache = THEMES.lock().unwrap();
        if agent::now() - cache.0 <= 60.0 {
            return cache.1.clone();
        }
    }
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(4),
        tokio::process::Command::new("omarchy")
            .args(["theme", "list"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output(),
    )
    .await;
    let list = match out {
        Ok(Ok(o)) => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
        _ => Vec::new(),
    };
    let mut cache = THEMES.lock().unwrap();
    cache.0 = agent::now();
    cache.1 = list;
    cache.1.clone()
}

async fn theme_named(name: &str) -> Option<String> {
    let name = name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    themes()
        .await
        .into_iter()
        .find(|t| t.to_lowercase() == name)
}

/// The sentences that are not about windows: sound, screen, theme, lock.
/// Each one is a call to the script that keyword already uses.
async fn system_plan(text: &str) -> Option<Plan> {
    // sound, as a level
    if let Some(m) = fullmatch(&PATS.vol_level, text)
        && agent::on_path("oxy-volume")
        && let Some(n) = level(cap(&m, "n"))
    {
        let who = cap(&m, "who");
        let input = matches!(who, "mic" | "microphone" | "input");
        let label = if input { "microphone" } else { "speaker" };
        let dir = if input { "input" } else { "output" };
        return Some(one(
            format!("Set the {label} volume to {n}%"),
            argv(&["oxy-volume", "set", dir, &n.to_string()]),
            format!("{label} volume at {n}%"),
        ));
    }

    // sound, as a step in the direction the volume keys go
    if let Some(m) = fullmatch(&PATS.vol_step, text)
        && agent::on_path("omarchy-audio-output-volume")
    {
        let way = if !cap(&m, "way").is_empty() {
            cap(&m, "way").to_string()
        } else if cap(&m, "w2") == "louder" {
            "up".to_string()
        } else {
            "down".to_string()
        };
        return Some(one(
            format!("Turn the volume {way}"),
            argv(&[
                "omarchy-audio-output-volume",
                if way == "up" { "raise" } else { "lower" },
            ]),
            format!("volume {way}"),
        ));
    }

    // mute is a state, not a toggle: "mute" said twice is still muted
    if let Some(m) = fullmatch(&PATS.mute, text)
        && agent::on_path("omarchy-audio-output-volume")
    {
        let want = cap(&m, "un").is_empty();
        return Some(Plan {
            steps: vec![Step {
                say: format!("{} the speakers", if want { "Mute" } else { "Unmute" }),
                kind: StepKind::Mute(want),
            }],
            done: format!("speakers {}", if want { "muted" } else { "unmuted" }),
        });
    }

    // screen brightness
    if let Some(m) = fullmatch(&PATS.bright, text)
        && agent::on_path("omarchy-brightness-display")
        && let Some(n) = level(cap(&m, "n"))
    {
        return Some(one(
            format!("Set the brightness to {n}%"),
            argv(&["omarchy-brightness-display", &format!("{n}%")]),
            format!("brightness at {n}%"),
        ));
    }

    // a screenshot, in the three shapes omarchy already takes one in
    if let Some(m) = fullmatch(&PATS.shot, text)
        && agent::on_path("omarchy-capture-screenshot")
    {
        let what = cap(&m, "what");
        // No argument is what the Print Screen binding on this machine runs,
        // so a bare "take a screenshot" does the same thing the key does.
        let how = if matches!(what, "whole screen" | "screen" | "everything") {
            "fullscreen"
        } else if what.contains("window") {
            "windows"
        } else {
            ""
        };
        return Some(one(
            format!(
                "Take a screenshot{}",
                if how.is_empty() {
                    String::new()
                } else {
                    format!(" ({how})")
                }
            ),
            if how.is_empty() {
                argv(&["omarchy-capture-screenshot"])
            } else {
                argv(&["omarchy-capture-screenshot", how])
            },
            "screenshot taken".into(),
        ));
    }

    // lock, spelled out in full: `lock` on its own is a word with several jobs
    if fullmatch(&PATS.lock, text).is_some() && agent::on_path("omarchy-system-lock") {
        return Some(one(
            "Lock the screen".into(),
            argv(&["omarchy-system-lock"]),
            "locked".into(),
        ));
    }

    // a theme, by a name this machine actually has
    if let Some(m) = fullmatch(&PATS.theme, text)
        && agent::on_path("omarchy")
    {
        let wanted = {
            let name = cap(&m, "name");
            if name.is_empty() { cap(&m, "n2") } else { name }
        };
        if let Some(theme) = theme_named(wanted).await {
            return Some(one(
                format!("Switch to the {theme} theme"),
                argv(&["omarchy", "theme", "set", &theme]),
                format!("{theme} theme"),
            ));
        }
    }

    // bluetooth, on or off, which is a state and not a toggle
    if let Some(m) = fullmatch(&PATS.bt, text)
        && agent::on_path("omarchy-bluetooth-power")
    {
        let way = {
            let a = cap(&m, "a");
            if a.is_empty() { cap(&m, "b") } else { a }
        };
        return Some(one(
            format!("Turn bluetooth {way}"),
            argv(&["omarchy-bluetooth-power", way]),
            format!("bluetooth {way}"),
        ));
    }

    None
}

pub fn plan_labels(plan: Option<&Plan>) -> Vec<String> {
    plan.map(|p| p.steps.iter().map(|s| s.say.clone()).collect())
        .unwrap_or_default()
}
