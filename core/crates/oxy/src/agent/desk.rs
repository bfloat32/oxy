//! The desktop helper — `oxy-agent desk <verb>`. Not a sandbox, and not the
//! reason the agent cannot run hyprctl: it can. This is here because Hyprland
//! on this machine is configured in Lua, `hyprctl dispatch workspace 9` is a
//! syntax error under it, and the string forms that do parse can return `ok`
//! and do nothing. Every verb is the dispatcher spelled the way this
//! compositor actually takes it, and `open`/`tile` wait for windows to map.

use std::time::Duration;

use serde_json::Value;

use crate::agent;

const DESK_HELP: &str = "oxy-agent desk <verb> [argument]

  list                  every window, workspace and monitor, as JSON
  empty                 go to the lowest workspace with nothing on it
  workspace <name>      go to one: a number, a name, e+1, e-1, previous
  move <name>           move the focused window to one
  open <app>            launch an application and wait for its window
  tile <n> [app]        open n of them, splitting the largest pane each time
  focus <l|r|u|d>       move focus
  swap <l|r|u|d>        swap the focused window with its neighbour
  preselect <l|r|u|d>   put the next window on that side of this one
  split                 flip the current split between rows and columns
  float                 float or tile the focused window
  fullscreen            fullscreen the focused window
  close                 close the focused window

Set OXY_DESK_DRY=1 to print what each would dispatch and change nothing.";

/// Where a verb's stdout/stderr go. `cmd_desk` prints them; `run_step`
/// captures them so the only thing on a plan run's stdout is the event
/// stream the card reads.
#[derive(Default)]
pub struct Io {
    pub out: String,
    pub err: String,
}

impl Io {
    pub fn say(&mut self, line: &str) {
        self.out.push_str(line);
        self.out.push('\n');
    }

    pub fn complain(&mut self, line: &str) {
        self.err.push_str(line);
        self.err.push('\n');
    }
}

fn dry() -> bool {
    std::env::var("OXY_DESK_DRY").as_deref() == Ok("1")
}

pub(crate) fn dry_run() -> bool {
    dry()
}

/// `json.dumps(obj, indent=1)` — serde's pretty printer with a one-space
/// indent, which is the same layout.
fn json_indent1(v: &Value) -> String {
    let mut buf = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
    let _ = serde::Serialize::serialize(v, &mut ser);
    String::from_utf8_lossy(&buf).into_owned()
}

async fn run_capture(argv: &[String], timeout: Duration) -> Option<std::process::Output> {
    tokio::time::timeout(
        timeout,
        tokio::process::Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    .ok()
    .and_then(|r| r.ok())
}

/// One Lua dispatcher. The whole point of this helper: `hyprctl dispatch`
/// wraps its argument as `return hl.dispatch(<argument>)`, so the argument
/// is Lua and the pre-Lua string forms are gone.
async fn dispatch(expr: &str, io: &mut Io) -> i32 {
    if dry() {
        io.say(&format!("would dispatch: {expr}"));
        return 0;
    }
    let Some(p) = run_capture(
        &[
            "hyprctl".to_string(),
            "dispatch".to_string(),
            expr.to_string(),
        ],
        Duration::from_secs(30),
    )
    .await
    else {
        io.complain("dispatch failed");
        return 1;
    };
    let out = String::from_utf8_lossy(&p.stdout).trim().to_string();
    let err = String::from_utf8_lossy(&p.stderr).trim().to_string();
    // hyprctl says `error: ...` on stdout and exits 0 for a dispatcher it
    // could not run, so the exit status alone reports success for something
    // that did nothing. A `warning:` is not the same thing and stays a
    // success with its text kept, because the agent needs to read it.
    if !p.status.success() || out.starts_with("error") {
        io.complain(if !out.is_empty() {
            &out
        } else if !err.is_empty() {
            &err
        } else {
            "dispatch failed"
        });
        return 1;
    }
    io.say(if out.is_empty() { "ok" } else { &out });
    0
}

async fn hypr_json(what: &str) -> Option<Value> {
    let p = run_capture(
        &["hyprctl".to_string(), "-j".to_string(), what.to_string()],
        Duration::from_secs(4),
    )
    .await?;
    serde_json::from_str(&String::from_utf8_lossy(&p.stdout)).ok()
}

pub(crate) async fn active_workspace() -> Option<i64> {
    active_workspace_id().await
}

async fn active_workspace_id() -> Option<i64> {
    hypr_json("activeworkspace")
        .await
        .and_then(|ws| ws.get("id").and_then(|id| id.as_i64()))
}

async fn clients_on(workspace_id: Option<i64>) -> Vec<Value> {
    hypr_json("clients")
        .await
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|c| {
            c.get("mapped").and_then(|m| m.as_bool()).unwrap_or(false)
                && c.get("workspace")
                    .and_then(|w| w.get("id"))
                    .and_then(|id| id.as_i64())
                    == workspace_id
        })
        .collect()
}

/// A named but empty workspace is somebody's plan for later, not scratch
/// space, so `empty` walks past it. Reading the same file the bar reads.
fn named_workspace(wid: i64) -> bool {
    let path = dirs_home().join(".config/omarchy/shell.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(data) = serde_json::from_str::<Value>(&text) else {
        return false;
    };

    fn walk(node: &Value, wid: i64) -> bool {
        match node {
            Value::Object(map) => {
                if let Some(Value::Object(names)) = map.get("names") {
                    match names.get(&wid.to_string()) {
                        Some(Value::Object(entry)) if entry.get("label").is_some() => {
                            return true;
                        }
                        Some(Value::String(s)) if !s.is_empty() => return true,
                        _ => {}
                    }
                }
                map.values().any(|v| walk(v, wid))
            }
            Value::Array(list) => list.iter().any(|v| walk(v, wid)),
            _ => false,
        }
    }
    walk(&data, wid)
}

fn dirs_home() -> std::path::PathBuf {
    oxy_core::settings::paths::home()
}

async fn desk_empty(io: &mut Io) -> i32 {
    let mut busy = std::collections::HashSet::new();
    for c in hypr_json("clients")
        .await
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
    {
        if let Some(wid) = c
            .get("workspace")
            .and_then(|w| w.get("id"))
            .and_then(|id| id.as_i64())
        {
            busy.insert(wid);
        }
    }
    for wid in 1..=10i64 {
        if busy.contains(&wid) || named_workspace(wid) {
            continue;
        }
        let rc = dispatch(&format!("hl.dsp.focus({{ workspace = {wid} }})"), io).await;
        if rc == 0 && !dry() {
            io.say(&format!("workspace {wid}"));
        }
        return rc;
    }
    io.complain("every workspace from 1 to 10 is occupied or named");
    1
}

const TERMINALS: &[&str] = &["alacritty", "ghostty", "foot", "kitty", "wezterm"];

/// An application name, resolved to something that starts it. `omarchy
/// launch terminal` is preferred for a terminal because it opens in the
/// active terminal's directory, which is what a person means by "another
/// one".
pub fn app_command(name: &str) -> Option<Vec<String>> {
    let name = name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if name.is_empty() {
        return None;
    }
    if matches!(name.as_str(), "terminal" | "term" | "console" | "shell") {
        if agent::on_path("omarchy") {
            return Some(vec!["omarchy".into(), "launch".into(), "terminal".into()]);
        }
        for cand in TERMINALS {
            if agent::on_path(cand) {
                return Some(vec![cand.to_string()]);
            }
        }
        return None;
    }
    if !valid_app_name(&name) {
        return None;
    }
    if let Some(entry) = desktop_entry(&name) {
        if agent::on_path("gtk-launch") {
            let base = entry
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default();
            return Some(vec!["gtk-launch".into(), base]);
        }
        if agent::on_path("gio") {
            return Some(vec![
                "gio".into(),
                "launch".into(),
                entry.to_string_lossy().into_owned(),
            ]);
        }
    }
    if agent::on_path(&name) {
        return Some(vec![name]);
    }
    None
}

fn valid_app_name(name: &str) -> bool {
    static RE: std::sync::LazyLock<fancy_regex::Regex> = std::sync::LazyLock::new(|| {
        fancy_regex::Regex::new(r"\A[A-Za-z0-9._+ -]{1,64}\z").expect("app name regex")
    });
    RE.is_match(name).unwrap_or(false)
}

fn desktop_entry(name: &str) -> Option<std::path::PathBuf> {
    let mut roots = vec![dirs_home().join(".local/share/applications")];
    let dirs =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    for d in dirs.split(':') {
        roots.push(std::path::PathBuf::from(d).join("applications"));
    }
    for root in &roots {
        let path = root.join(format!("{name}.desktop"));
        if path.is_file() {
            return Some(path);
        }
    }
    for root in &roots {
        let Ok(read) = std::fs::read_dir(root) else {
            continue;
        };
        let mut names: Vec<String> = read
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect();
        names.sort();
        for f in names {
            if f.ends_with(".desktop") && f.to_lowercase().contains(name) {
                return Some(root.join(f));
            }
        }
    }
    None
}

/// Launch, and wait for the window. Returning before the window exists is
/// what turns a sequence of these into a race: the next split happens
/// against a pane that has not appeared yet.
async fn desk_open(name: &str, quiet: bool, io: &mut Io) -> i32 {
    let Some(cmd) = app_command(name) else {
        io.complain(&format!("no application called {}", agent::py_repr(name)));
        return 2;
    };
    if dry() {
        io.say(&format!("would launch: {}", cmd.join(" ")));
        return 0;
    }

    let wid = active_workspace_id().await;
    let before: std::collections::HashSet<String> = clients_on(wid)
        .await
        .iter()
        .filter_map(|c| c.get("address").and_then(|a| a.as_str()).map(String::from))
        .collect();
    let mut spawn = tokio::process::Command::new(&cmd[0]);
    spawn
        .args(&cmd[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Its own session, so a launched app does not die with this process.
    #[cfg(unix)]
    spawn.process_group(0);
    let spawned = spawn.spawn();
    if let Err(e) = spawned {
        io.complain(&e.to_string());
        return 1;
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let now_list = clients_on(wid).await;
        let fresh = now_list.iter().any(|c| {
            c.get("address")
                .and_then(|a| a.as_str())
                .is_some_and(|a| !before.contains(a))
        });
        if fresh {
            if !quiet {
                io.say(&format!(
                    "opened {} ({} on workspace {})",
                    name,
                    now_list.len(),
                    wid.map(|w| w.to_string()).unwrap_or_else(|| "None".into())
                ));
            }
            return 0;
        }
    }
    io.complain(&format!("launched {name} and no window appeared within 8s"));
    1
}

/// Focus the pane with the most area, so the next window splits the biggest
/// thing on screen: four terminals come out two by two rather than one wide
/// column beside three stacked slivers.
async fn focus_biggest(io: &mut Io) -> i32 {
    let here = clients_on(active_workspace_id().await).await;
    if here.is_empty() {
        return 0;
    }
    let Some(biggest) = here.iter().rev().max_by_key(|c| {
        let size = c.get("size").and_then(|s| s.as_array());
        let w = size
            .and_then(|s| s.first())
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let h = size
            .and_then(|s| s.get(1))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        w * h
    }) else {
        return 0;
    };
    let addr = biggest
        .get("address")
        .and_then(|a| a.as_str())
        .unwrap_or("");
    dispatch(
        &format!("hl.dsp.focus({{ window = \"address:{addr}\" }})"),
        io,
    )
    .await
}

/// n windows, as square as this layout gets.
async fn desk_tile(count: &str, name: &str, io: &mut Io) -> i32 {
    let Ok(count) = count.parse::<i64>() else {
        io.complain("tile takes a number");
        return 2;
    };
    if !(1..=8).contains(&count) {
        io.complain("tile takes 1 to 8");
        return 2;
    }
    for i in 0..count {
        if i > 0 {
            let rc = focus_biggest(io).await;
            if rc != 0 {
                return rc;
            }
        }
        let rc = desk_open(name, true, io).await;
        if rc != 0 {
            return rc;
        }
    }
    if dry() {
        return 0;
    }
    io.say(&format!(
        "{count} {name} on workspace {}",
        active_workspace_id()
            .await
            .map(|w| w.to_string())
            .unwrap_or_else(|| "None".into())
    ));
    0
}

fn valid_ws_name(arg: &str) -> bool {
    static RE: std::sync::LazyLock<fancy_regex::Regex> = std::sync::LazyLock::new(|| {
        fancy_regex::Regex::new(r"\A[A-Za-z0-9_:+-]{1,32}\z").expect("ws name regex")
    });
    RE.is_match(arg).unwrap_or(false)
}

fn valid_dir(arg: &str) -> bool {
    static RE: std::sync::LazyLock<fancy_regex::Regex> = std::sync::LazyLock::new(|| {
        fancy_regex::Regex::new(r"\A[lrud]\z").expect("direction regex")
    });
    RE.is_match(arg).unwrap_or(false)
}

async fn cmd_desk_io(args: &[String], io: &mut Io) -> i32 {
    if args.is_empty() || matches!(args[0].as_str(), "help" | "-h" | "--help") {
        io.say(DESK_HELP);
        return 0;
    }
    let verb = args[0].as_str();
    let rest = &args[1..];
    let arg = rest.join(" ").trim().to_string();

    match verb {
        "list" => {
            let mut out = serde_json::Map::new();
            for what in ["clients", "workspaces", "monitors", "activewindow"] {
                out.insert(
                    what.to_string(),
                    hypr_json(what).await.unwrap_or(Value::Null),
                );
            }
            // `indent=1` and a 20000-char cap, the way the script prints it
            // — `text[:20000]` counts chars, so window titles with unicode
            // end at a char edge rather than a byte one that panics.
            let mut text = json_indent1(&Value::Object(out));
            if text.chars().count() > 20000 {
                text = text.chars().take(20000).collect();
            }
            io.say(&text);
            0
        }
        "empty" => desk_empty(io).await,
        "open" => desk_open(&arg, false, io).await,
        "tile" => {
            let parts: Vec<String> = if rest.is_empty() {
                vec!["2".to_string()]
            } else {
                rest.to_vec()
            };
            let name = {
                let n = parts[1..].join(" ");
                if n.is_empty() {
                    "terminal".to_string()
                } else {
                    n
                }
            };
            desk_tile(&parts[0], &name, io).await
        }
        "workspace" => {
            if !valid_ws_name(&arg) {
                return agent::usage_io(
                    "workspace takes a number, a name, e+1, e-1 or previous",
                    io,
                );
            }
            dispatch(&format!("hl.dsp.focus({{ workspace = \"{arg}\" }})"), io).await
        }
        "move" => {
            if !valid_ws_name(&arg) {
                return agent::usage_io("move takes a workspace", io);
            }
            dispatch(
                &format!("hl.dsp.window.move({{ workspace = \"{arg}\" }})"),
                io,
            )
            .await
        }
        "focus" => {
            if !valid_dir(&arg) {
                return agent::usage_io("focus takes l, r, u or d", io);
            }
            dispatch(&format!("hl.dsp.focus({{ direction = \"{arg}\" }})"), io).await
        }
        "swap" => {
            if !valid_dir(&arg) {
                return agent::usage_io("swap takes l, r, u or d", io);
            }
            dispatch(
                &format!("hl.dsp.window.swap({{ direction = \"{arg}\" }})"),
                io,
            )
            .await
        }
        "preselect" => {
            if !valid_dir(&arg) {
                return agent::usage_io("preselect takes l, r, u or d", io);
            }
            dispatch(&format!("hl.dsp.layout(\"preselect {arg}\")"), io).await
        }
        "split" => dispatch("hl.dsp.layout(\"togglesplit\")", io).await,
        "float" => dispatch("hl.dsp.window.float({ action = \"toggle\" })", io).await,
        "fullscreen" => dispatch("hl.dsp.window.fullscreen({ mode = \"fullscreen\" })", io).await,
        "close" => dispatch("hl.dsp.window.close()", io).await,
        other => agent::usage_io(
            &format!(
                "no such verb {}. `oxy-agent desk help`",
                agent::py_repr(other)
            ),
            io,
        ),
    }
}

/// Whether the speakers are muted, read the way the volume keyword reads it.
/// None when the question cannot be answered, which makes `mute` fall back
/// to the toggle rather than guess.
pub async fn muted_now() -> Option<bool> {
    let sink = run_capture(
        &["omarchy-audio-output-sink".to_string()],
        Duration::from_secs(4),
    )
    .await
    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())?;
    if sink.is_empty() {
        return None;
    }
    let out = run_capture(
        &["pactl".to_string(), "get-sink-mute".to_string(), sink],
        Duration::from_secs(4),
    )
    .await
    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())?;
    Some(out.contains("yes"))
}

/// The `desk` subcommand: prints what the verbs said on the real streams.
pub async fn cmd_desk(args: &[String]) -> i32 {
    let mut io = Io::default();
    let rc = cmd_desk_io(args, &mut io).await;
    print!("{}", io.out);
    eprint!("{}", io.err);
    let _ = std::io::Write::flush(&mut std::io::stdout());
    rc
}

/// The same, with output captured — `run_step` uses it so a plan's stdout
/// stays the event stream the card reads.
pub(crate) async fn desk_capture(args: &[String]) -> (i32, String, String) {
    let mut io = Io::default();
    let rc = cmd_desk_io(args, &mut io).await;
    (rc, io.out, io.err)
}

/// `focus_biggest` + `desk_open`, captured — the tile step's body.
pub(crate) async fn tile_capture(split: bool, app: &str) -> (i32, String, String) {
    let mut io = Io::default();
    let mut rc = if split {
        focus_biggest(&mut io).await
    } else {
        0
    };
    if rc == 0 {
        rc = desk_open(app, true, &mut io).await;
    }
    (rc, io.out, io.err)
}

/// `desk <verb> ...` as one line somebody can read on a card — the step
/// labels for a Bash row that called into here. Only the run stream's
/// `describe` calls it, so it is unix-only like the run loop.
#[cfg(unix)]
pub fn desk_line(rest: &str) -> String {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.is_empty() {
        return "the desktop".into();
    }
    let verb = parts[0];
    let arg = parts[1..].join(" ");
    match verb {
        "empty" => "Go to an empty workspace".into(),
        "workspace" => format!("Go to workspace {arg}"),
        "move" => format!("Move this window to {arg}"),
        "open" => format!(
            "Open {}",
            if arg.is_empty() {
                "an application"
            } else {
                &arg
            }
        ),
        "tile" => {
            if arg.is_empty() {
                "Tile".into()
            } else {
                format!("Open {}", arg.replacen(' ', " × ", 1))
            }
        }
        "focus" => format!("Focus {arg}"),
        "swap" => format!("Swap {arg}"),
        "split" => "Flip the split".into(),
        "float" => "Float this window".into(),
        "fullscreen" => "Fullscreen".into(),
        "close" => "Close this window".into(),
        "list" => "Look at the desktop".into(),
        _ => rest.to_string(),
    }
}
