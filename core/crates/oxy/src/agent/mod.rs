//! `oxy-agent` — the `do:` keyword's agent backend, ported from
//! `bin/oxy-agent` (which stays as the script fallback). A sentence in the
//! search box, done by a local agent:
//!
//!   do: open a new workspace and split it into four terminals
//!   do: find every TODO in this repo and list them
//!   do:@codex explain what changed on this branch
//!
//! The socket contract is unchanged — `{epoch, query}` lines in, `{epoch,
//! rows}` answers and pushes out — so the engine's `socket.rs` and
//! `ResultAgent.qml` read exactly what the Python daemon wrote.
//!
//! Subcommands (same as the script's):
//!
//!   search <query>   one JSON row, for the launcher (cheap: no agent, no daemon)
//!   serve            the daemon: owns the agent process, answers the socket
//!   send <token>     start the instruction behind that token (called by Enter)
//!   plan <token>     run that token's steps directly, with no model involved
//!   stop             stop whatever is running
//!   new              forget the conversation
//!   term <token>     hand the instruction to an interactive agent in a terminal
//!   desk <verb> ...  the desktop, with the Lua dispatchers already spelled right
//!   agents           which agent CLIs are installed

pub mod client;
pub mod daemon;
pub mod desk;
pub mod plan;
pub mod planrun;
pub mod previews;
pub mod rows;
// The run/daemon pair needs a unix socket and a process group to be itself;
// on other platforms `serve` says so and the search/desk/plan/term paths
// still answer.
#[cfg(unix)]
pub mod run;
pub mod sha256;

use std::path::PathBuf;
use std::sync::LazyLock;

use oxy_core::settings::paths as dirs;

/// `$XDG_STATE_HOME/omarchy` — where the socket and the previews file live.
pub fn state_dir() -> PathBuf {
    dirs::state_home().join("omarchy")
}

#[cfg(unix)]
pub fn socket_path() -> PathBuf {
    state_dir().join("oxy-agent.sock")
}

pub fn previews_path() -> PathBuf {
    state_dir().join("oxy-agent-previews.json")
}

// How long a run survives with nobody watching. The launcher re-asks this
// extension every refresh tick while its rows are on screen, so silence means
// the card is gone. Escape does not rely on this: the row carries its own stop
// command and the launcher runs it. This is the backstop for the launcher being
// closed, killed, or crashing mid-run.
#[cfg(unix)]
pub const GRACE: f64 = 2.5;
// How long a conversation outlives the last question about it. The card is a
// chat while you are in it and a receipt for a few seconds after, and then it
// is over: the launcher stops asking the moment it closes or you type a
// different keyword, so this is what makes opening `do:` again start on the
// examples instead of on what happened before lunch.
#[cfg(unix)]
pub const VISIT: f64 = 8.0;
// The daemon exists to hold one agent process and the conversation around it.
// With neither, it is a few megabytes of nothing, so it leaves.
#[cfg(unix)]
pub const IDLE_EXIT: f64 = 900.0;

// The preview store is a handshake, not a history: it holds what was drawn
// long enough for Enter or Ctrl+K to name it, and every keystroke writes one,
// so an entry has to be able to go stale on its own. A draft dies with the
// typing that made it; one that was actually sent is the token behind a run
// and lives an hour, which is longer than any card that could still be asking
// about it.
pub const PREVIEW_TTL: f64 = 600.0;
pub const SENT_TTL: f64 = 3600.0;
pub const PREVIEW_MAX: usize = 16;

#[cfg(unix)]
pub const MAX_STEPS: usize = 200;
#[cfg(unix)]
pub const MAX_ANSWER: usize = 8000;
/// One stream line's ceiling. Real events are kilobytes at most; anything
/// past this is a child writing noise with no newline in it, and the
/// buffer would otherwise grow for as long as it kept going.
#[cfg(unix)]
pub const MAX_LINE: usize = 256 * 1024;
#[cfg(unix)]
pub const MAX_TURNS: usize = 12;
// How many finished steps stay under the live one. Three is what fits beside
// an answer in a card twelve lines tall, and the ones before them are counted
// rather than drawn.
#[cfg(unix)]
pub const TAIL: usize = 3;

// --------------------------------------------------------------- the agents

/// How an agent CLI's output is read: `Claude` is the parsed step stream,
/// `Plain` is prose only with no step list to show, `Plan` is the
/// deterministic runner's own step/step_end/result stream.
#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(unix), allow(dead_code))]
pub enum Stream {
    Claude,
    Plain,
    Plan,
}

#[derive(Clone)]
pub struct Agent {
    pub id: String,
    pub title: String,
    pub bin: String,
    // Read only by the run loop, which is unix-only.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub stream: Stream,
    /// Whether this CLI can be told to act without a human at the prompt. A
    /// `plan`-mode agent belongs on a card that says so, not on `do:`.
    pub acts: bool,
    /// `Some(name-as-typed)` when the user asked for this agent by name and
    /// its binary is not installed — an answer in its own right, not a
    /// silent fall back to another.
    pub missing: Option<String>,
}

impl Agent {
    fn stock(id: &str, title: &str, bin: &str, stream: Stream, acts: bool) -> Agent {
        Agent {
            id: id.into(),
            title: title.into(),
            bin: bin.into(),
            stream,
            acts,
            missing: None,
        }
    }

    pub fn is_missing(&self) -> bool {
        self.missing.is_some()
    }
}

/// The installable agent CLIs, in preference order.
pub fn agents() -> Vec<Agent> {
    vec![
        Agent::stock("claude", "Claude", "claude", Stream::Claude, true),
        Agent::stock("codex", "Codex", "codex", Stream::Plain, true),
        Agent::stock("gemini", "Gemini", "gemini", Stream::Plain, false),
    ]
}

/// Not an agent, and named on the card so nobody mistakes it for one. This is
/// what runs a sentence that has exactly one reading: the desk verbs, in the
/// order the card printed before Enter. It needs no CLI installed, so `do:
/// open four terminals` works on a machine with no agent at all.
pub fn direct_agent() -> Agent {
    Agent::stock("oxy", "Oxy", "", Stream::Plan, true)
}

// Pre-approved. Everything a person does at a desktop is here, including the
// shell, because "open a new workspace and split it into four terminals" is a
// shell command five times over and any policy that cannot express it is a
// policy that makes this keyword a lie.
pub const ALLOWED: &[&str] = &[
    "Bash",
    "Read",
    "Write",
    "Edit",
    "NotebookEdit",
    "Glob",
    "Grep",
    "WebFetch",
    "WebSearch",
    "TodoWrite",
    "Task",
    "Skill",
    "ToolSearch",
];

// The other half. Nothing here is forbidden on this machine: it is the set of
// things whose cost, when the model is wrong, is not a window in the wrong
// place. Each one is either irreversible (the file is gone, the commit is
// public, the key is rotated) or reaches past the desktop session entirely.
// They come back as a refusal you can see, and Ctrl+K is the way to do them
// with a human answering the prompt.
pub const DENIED: &[&str] = &[
    // Gone means gone.
    "Bash(rm:*)",
    "Bash(rmdir:*)",
    "Bash(shred:*)",
    "Bash(dd:*)",
    "Bash(mkfs:*)",
    "Bash(truncate:*)",
    "Bash(chown:*)",
    "Bash(mkswap:*)",
    "Bash(fdisk:*)",
    "Bash(parted:*)",
    "Bash(wipefs:*)",
    // Not your privileges to take.
    "Bash(sudo:*)",
    "Bash(pkexec:*)",
    "Bash(doas:*)",
    "Bash(su:*)",
    // The machine's software, and the machine's session.
    "Bash(pacman:*)",
    "Bash(yay:*)",
    "Bash(paru:*)",
    "Bash(systemctl:*)",
    "Bash(loginctl:*)",
    "Bash(reboot:*)",
    "Bash(poweroff:*)",
    "Bash(shutdown:*)",
    "Bash(omarchy update:*)",
    "Bash(omarchy pkg:*)",
    "Bash(omarchy install:*)",
    "Bash(omarchy system:*)",
    "Bash(omarchy refresh:*)",
    "Bash(omarchy reinstall:*)",
    // Credentials. A read is as bad as a write here.
    "Bash(pass:*)",
    "Bash(gpg:*)",
    "Bash(ssh-keygen:*)",
    "Bash(ssh-add:*)",
    "Bash(secret-tool:*)",
    "Bash(passwd:*)",
    "Bash(keyctl:*)",
    // Off this machine. Anything here is seen by somebody else the moment it
    // works, which is the definition of not being undoable from here.
    "Bash(git push:*)",
    "Bash(gh:*)",
    "Bash(glab:*)",
    "Bash(ssh:*)",
    "Bash(scp:*)",
    "Bash(rsync:*)",
    "Bash(sftp:*)",
    "Bash(curl:*)",
    "Bash(wget:*)",
    // History a reflog does not get back.
    "Bash(git reset --hard:*)",
    "Bash(git clean:*)",
    "Bash(git filter-branch:*)",
    "Bash(git push --force:*)",
    // Itself. An agent that restarts the daemon it is streaming into loses
    // the card it was writing to, and one that starts another run makes two.
    "Bash(oxy-agent send:*)",
    "Bash(oxy-agent serve:*)",
    "Bash(oxy-agent plan:*)",
    "Bash(oxy-agent stop:*)",
    "Bash(oxy-agent new:*)",
];

/// What the agent is told about where it woke up. Two things it cannot guess:
/// the shape of the card it is writing into, and that this machine's Hyprland
/// takes Lua.
#[cfg(unix)]
pub const BRIEF: &str = r#"You are running from a desktop launcher on the user's own machine.
They typed this instruction themselves and pressed Enter. That is your consent
to act: do the thing, do not ask whether to, and do not describe what you would
have done. If a tool is refused, say which one and stop; the user has a way to
re-run you in a terminal where they can approve it.

Your reply is drawn in a card about twelve lines tall. Answer in a few short
lines, no preamble, no markdown headings, no bullet lists longer than three
items. When you did something rather than found something out, say what is now
true: "four terminals on workspace 4", not "I have completed the task".

This is Omarchy (Arch + Hyprland). Hyprland is configured in Lua here, and
`hyprctl dispatch` takes a Lua expression, not the old string form: what you
pass is wrapped as `return hl.dispatch(<your argument>)`.

  wrong:  hyprctl dispatch workspace 9
  wrong:  hyprctl dispatch movetoworkspace 9
  right:  hyprctl dispatch 'hl.dsp.focus({ workspace = 9 })'
  right:  hyprctl dispatch 'hl.dsp.window.move({ workspace = 9 })'
  right:  hyprctl dispatch 'hl.dsp.focus({ window = "address:0x55..." })'
  right:  hyprctl dispatch 'hl.dsp.layout("togglesplit")'

The old string form is not an error you will see: under a Lua config it can
return `ok` and do nothing at all.

Prefer `oxy-agent desk`, which is those dispatchers with the syntax already
right and which waits for windows to actually appear before returning. Run
`oxy-agent desk help` for the verbs. Reading the desktop is plain
`hyprctl -j clients|workspaces|monitors|activewindow`.

  a new workspace with four terminals in a grid:
      oxy-agent desk empty
      oxy-agent desk tile 4 terminal

Before changing anything under ~/.config/hypr, ~/.config/omarchy, the bar, a
theme, a keybinding, or before reaching for an `omarchy` command, load the
`omarchy` skill: it is installed here and it is more current than you are."#;

pub const EXAMPLES: &[&str] = &[
    "open a new workspace and split it into four terminals",
    "find every TODO in this repo and list them",
    "what is taking up space in my downloads folder",
];

// ------------------------------------------------------------------ helpers

/// `shutil.which`: `name` (or `name.EXE` and friends on Windows) resolved
/// against PATH, first hit returned.
pub fn which(name: &str) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    if name.contains(['/', '\\']) {
        return executable(std::path::Path::new(name)).then(|| PathBuf::from(name));
    }
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(name);
        if executable(&candidate) {
            return Some(candidate);
        }
        #[cfg(windows)]
        for ext in ["exe", "bat", "cmd", "com"] {
            let cand = dir.join(format!("{name}.{ext}"));
            if executable(&cand) {
                return Some(cand);
            }
        }
    }
    None
}

pub fn on_path(name: &str) -> bool {
    which(name).is_some()
}

#[cfg(unix)]
fn executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn executable(path: &std::path::Path) -> bool {
    path.is_file()
}

pub fn agents_present() -> Vec<Agent> {
    agents().into_iter().filter(|a| on_path(&a.bin)).collect()
}

/// The agent for this query. A named one that is missing is an answer in its
/// own right, not a silent fall back to another: asking Codex and being
/// answered by Claude is worse than being told Codex is not here.
pub fn pick_agent(named: &str) -> Option<Agent> {
    if !named.is_empty() {
        let lowered = named.to_lowercase();
        for a in agents() {
            if a.id == lowered {
                return if on_path(&a.bin) {
                    Some(a)
                } else {
                    Some(Agent {
                        missing: Some(named.to_string()),
                        ..a
                    })
                };
            }
        }
        return Some(Agent {
            id: named.into(),
            title: named.into(),
            bin: named.into(),
            stream: Stream::Plain,
            acts: false,
            missing: Some(named.to_string()),
        });
    }
    let here = agents_present();
    if here.is_empty() {
        return None;
    }
    if let Some(a) = here.iter().find(|a| a.acts) {
        return Some(a.clone());
    }
    here.into_iter().next()
}

/// `@codex do the thing` -> ("codex", "do the thing").
pub fn split_query(text: &str) -> (String, String) {
    let text = text.trim();
    static AT: LazyLock<fancy_regex::Regex> =
        LazyLock::new(|| fancy_regex::Regex::new(r"^@([A-Za-z0-9_.-]+)\s*").expect("AT regex"));
    match AT.captures(text).ok().flatten() {
        Some(caps) => {
            let m = caps.get(0).unwrap();
            let name = caps.get(1).map(|g| g.as_str()).unwrap_or("");
            (name.to_string(), text[m.end()..].trim().to_string())
        }
        None => (String::new(), text.to_string()),
    }
}

/// Where a run happens. `oxy-repo --current` is where this launcher has
/// already written down what "the repo I am in" means for a window with no
/// working directory, so the agent inherits that answer rather than inventing
/// a second one. Asked once per process, because the daemon answers a
/// question every refresh tick and a fork per tick is what the socket exists
/// to avoid.
pub async fn workdir() -> String {
    static CACHE: tokio::sync::OnceCell<String> = tokio::sync::OnceCell::const_new();
    CACHE
        .get_or_init(|| async {
            let fallback = dirs::home().to_string_lossy().into_owned();
            let out = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                tokio::process::Command::new("oxy-repo")
                    .arg("--current")
                    .stdin(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .output(),
            )
            .await;
            if let Ok(Ok(out)) = out {
                let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !text.is_empty() && std::path::Path::new(&text).is_dir() {
                    return text;
                }
            }
            fallback
        })
        .await
        .clone()
}

/// `Enter` carries a token, not the sentence: sha256 of the preview triple,
/// first 16 hex chars — the `[0-9a-f]{16}` the cases assert on.
pub fn token_for(agent_id: &str, instruction: &str, cwd: &str) -> String {
    let raw = [agent_id, instruction, cwd].join("\0");
    sha256::hexdigest(raw.as_bytes())[..16].to_string()
}

/// `~` for the home prefix, the way the card spells it.
pub fn tilde(path: &str) -> String {
    let home = dirs::home().to_string_lossy().into_owned();
    if path.starts_with(&home) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_string()
    }
}

/// `" ".join(text.split())`, then an ellipsis at `n` chars.
pub fn short(text: &str, n: usize) -> String {
    let flat: Vec<&str> = text.split_whitespace().collect();
    let flat = flat.join(" ");
    if flat.chars().count() <= n {
        flat
    } else {
        let mut s: String = flat.chars().take(n - 1).collect();
        s.push('…');
        s
    }
}

/// `\x1b\[[0-9;?]*[A-Za-z]` — CSI sequences stripped, everything else kept.
#[cfg(unix)]
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            while matches!(chars.peek(), Some(c) if c.is_ascii_digit() || *c == ';' || *c == '?') {
                chars.next();
            }
            if matches!(chars.peek(), Some(c) if c.is_ascii_alphabetic()) {
                chars.next();
            } else {
                // Not a terminated CSI after all: keep the bytes.
                out.push_str("\x1b[");
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Wall-clock seconds — `time.time()`. `startedAt` goes to QML, which
/// compares it against `Date.now()/1000`, so this must be unix time, not a
/// monotonic clock.
pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Python's `repr` for a short string — used in error text (`%r`).
pub fn py_repr(s: &str) -> String {
    let (quote, escape_quote) = if s.contains('\'') && !s.contains('"') {
        ('"', '"')
    } else {
        ('\'', '\'')
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == escape_quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `shlex.quote` — bare when every char is a word char or in `@%+=:,./-`,
/// else single-quoted with the `'"'"'` splice.
pub fn shlex_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    let safe = |c: char| c.is_alphanumeric() || c == '_' || "@%+=:,./-".contains(c);
    if s.chars().all(safe) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// `json.dumps(v)` byte-for-byte: the default `", "`/`": "` separators and
/// `ensure_ascii`'s `\uXXXX` for everything past ASCII, so a transcript or
/// row that mentions '·' or '×' comes out the way the script printed it.
pub fn to_wire(v: &serde_json::Value) -> String {
    struct PyFmt;
    impl serde_json::ser::Formatter for PyFmt {
        fn begin_object_key<W: std::io::Write + ?Sized>(
            &mut self,
            w: &mut W,
            first: bool,
        ) -> std::io::Result<()> {
            if first { Ok(()) } else { w.write_all(b", ") }
        }
        fn begin_object_value<W: std::io::Write + ?Sized>(
            &mut self,
            w: &mut W,
        ) -> std::io::Result<()> {
            w.write_all(b": ")
        }
        fn begin_array_value<W: std::io::Write + ?Sized>(
            &mut self,
            w: &mut W,
            first: bool,
        ) -> std::io::Result<()> {
            if first { Ok(()) } else { w.write_all(b", ") }
        }
    }
    let mut buf = Vec::new();
    {
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, PyFmt);
        let _ = serde::Serialize::serialize(v, &mut ser);
    }
    let raw = String::from_utf8_lossy(&buf);
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        let code = c as u32;
        if code < 0x80 {
            out.push(c);
        } else if code < 0x1_0000 {
            out.push_str(&format!("\\u{code:04x}"));
        } else {
            // Astral characters as a surrogate pair, the way ensure_ascii
            // spells them.
            let v = code - 0x1_0000;
            out.push_str(&format!(
                "\\u{:04x}\\u{:04x}",
                0xd800 + (v >> 10),
                0xdc00 + (v & 0x3ff)
            ));
        }
    }
    out
}

/// `usage`, with the complaint captured into the desk I/O instead — a plan
/// step's stderr is how its `why` reaches the card.
pub fn usage_io(message: &str, io: &mut desk::Io) -> i32 {
    io.complain(message);
    2
}

// -------------------------------------------------------------------- main

/// argv past the program name. The trailing `_ =>` treats the whole argument
/// list as a query, which is how `oxy-agent open four terminals` — the
/// spelling `cases.py` uses — is the same thing as `search`.
pub async fn main(argv: Vec<String>) -> i32 {
    let Some(verb) = argv.first() else {
        return client::cmd_search("").await;
    };
    match verb.as_str() {
        "search" => client::cmd_search(&argv[1..].join(" ")).await,
        "serve" => daemon::serve().await,
        "send" => client::cmd_send(argv.get(1).map(String::as_str).unwrap_or("")).await,
        "stop" => client::cmd_stop().await,
        "new" => client::cmd_new().await,
        "term" => client::cmd_term(argv.get(1).map(String::as_str).unwrap_or("")).await,
        "plan" => planrun::cmd_plan(argv.get(1).map(String::as_str).unwrap_or("")).await,
        "desk" => desk::cmd_desk(&argv[1..]).await,
        "agents" => client::cmd_agents().await,
        _ => client::cmd_search(&argv.join(" ")).await,
    }
}
