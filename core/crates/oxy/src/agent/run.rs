//! One exchange and one run: `Turn` is the thing the user said plus
//! everything that came back, `Run` is the agent process being read into it.
//! The daemon owns both; this file is the stream grammar — the claude
//! tool_use/tool_result protocol, the prose-only streams, and the plan
//! runner's step/step_end/result.

use std::collections::HashMap;
use std::io::Read;
use std::io::Write;

use serde_json::{Value, json};

use crate::agent::daemon::Event;
use crate::agent::{self, Agent, Stream, desk, plan};

/// One tool call or desk step as the card draws it.
#[derive(Clone)]
pub struct StepEvt {
    /// Stable identity for `pending` lookups — Python hung the dict itself
    /// off the id, and a step pushed out of the `MAX_STEPS` window is the
    /// same "not in the list any more" here.
    pub seq: u64,
    pub kind: String,
    pub text: String,
    /// doing → did | failed | blocked.
    pub state: String,
}

/// One thing the user said and everything that came back from it.
pub struct Turn {
    pub agent: Agent,
    pub text: String,
    pub cwd: String,
    /// Carried so the run can be handed back the sentence without it ever
    /// being on a command line, direct or not.
    pub token: String,
    /// This sentence is run here, step by step, with no model involved.
    pub direct: bool,
    /// running → done | failed | stopped.
    pub state: String,
    pub steps: Vec<StepEvt>,
    pub answer: String,
    pub blocked: Vec<Value>,
    pub note: String,
    pub started: f64,
    pub finished: f64,
    seq_next: u64,
}

impl Turn {
    pub fn new(agent: Agent, text: String, cwd: String, token: String) -> Turn {
        let direct = agent.id == agent::direct_agent().id;
        Turn {
            agent,
            text,
            cwd,
            token,
            direct,
            state: "running".into(),
            steps: Vec::new(),
            answer: String::new(),
            blocked: Vec::new(),
            note: String::new(),
            started: agent::now(),
            finished: 0.0,
            seq_next: 0,
        }
    }

    fn elapsed(&self) -> i64 {
        let end = if self.finished > 0.0 {
            self.finished
        } else {
            agent::now()
        };
        (end - self.started) as i64
    }

    fn push_step(&mut self, kind: &str, text: String) -> u64 {
        self.seq_next += 1;
        let seq = self.seq_next;
        self.steps.push(StepEvt {
            seq,
            kind: kind.to_string(),
            text,
            state: "doing".into(),
        });
        if self.steps.len() > agent::MAX_STEPS {
            let drop = self.steps.len() - agent::MAX_STEPS;
            self.steps.drain(..drop);
        }
        seq
    }

    /// What a person wants to watch: which step it is on, the last few it
    /// finished, and the answer. Not the tool names, not the JSON, and not
    /// a spinner, which answers no question anybody has.
    pub fn view(&self) -> Value {
        // While it runs, the newest step is the one being watched, whether
        // or not its result has come back yet.
        let (rest, now) = if self.state == "running" && !self.steps.is_empty() {
            (
                &self.steps[..self.steps.len() - 1],
                self.steps
                    .last()
                    .map(|s| s.text.clone())
                    .unwrap_or_default(),
            )
        } else {
            (&self.steps[..], String::new())
        };
        let tail_start = rest.len().saturating_sub(agent::TAIL);
        let tail = &rest[tail_start..];
        let did: Vec<Value> = tail
            .iter()
            .map(|e| json!({"text": e.text, "kind": e.kind, "state": e.state}))
            .collect();
        let mut seen = json!({
            "you": self.text,
            "state": self.state,
            "now": now,
            "did": did,
            "earlier": tail_start,
            "steps": self.steps.len(),
            "answer": self.answer.trim(),
            "blocked": self.blocked,
            "note": self.note,
            "elapsed": self.elapsed(),
            "startedAt": self.started as i64,
        });
        // Carried only when true, and the card says it in words. A run done
        // here and a run a model did are different kinds of promise.
        if self.direct {
            seen["direct"] = json!(true);
        }
        seen
    }
}

/// One agent process, reading its stream into one Turn.
pub struct Run {
    pub turn: Turn,
    pub agent: Agent,
    pub resume: String,
    pub session: String,
    buf: Vec<u8>,
    proc: Option<std::process::Child>,
    /// tool_use_id → (step seq, step text) — the text survives the step
    /// scrolling out of `steps`, which is what `blocked` reports.
    pending: HashMap<String, (u64, String)>,
    noise: Vec<String>,
    /// The last time a card asked about this run.
    pub seen: f64,
    /// Whether a card has ever asked. False means it was started from a
    /// shell with no launcher attached, and such a run is not part of
    /// anybody's conversation: it leaves the transcript when it dies.
    pub watched: bool,
}

impl Run {
    pub fn new(turn: Turn, resume: String) -> Run {
        let agent = turn.agent.clone();
        Run {
            turn,
            agent,
            resume,
            session: String::new(),
            buf: Vec::new(),
            proc: None,
            pending: HashMap::new(),
            noise: Vec::new(),
            seen: agent::now(),
            watched: false,
        }
    }

    /// The argv this run spawns — the CLIs' own flags, spelled the way the
    /// script spelled them.
    fn command(&self) -> Vec<String> {
        if self.turn.direct {
            // Its own process, for the same reason an agent gets one: the
            // daemon has to keep answering the card while windows are
            // opening, and Escape has to be able to kill the whole group
            // mid-plan.
            return vec![
                std::env::current_exe()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| "oxy-agent".into()),
                "plan".into(),
                self.turn.token.clone(),
            ];
        }
        let a = &self.agent;
        match a.id.as_str() {
            "claude" => {
                // The instruction goes down stdin, not on the command line:
                // the tool-list options are variadic and would eat it, and
                // keeping the sentence out of argv keeps it out of `ps`.
                let mut cmd = vec![
                    "claude".into(),
                    "-p".into(),
                    "--output-format".into(),
                    "stream-json".into(),
                    "--verbose".into(),
                    "--append-system-prompt".into(),
                    agent::BRIEF.into(),
                    "--add-dir".into(),
                    oxy_core::settings::paths::home()
                        .to_string_lossy()
                        .into_owned(),
                    "--allowedTools".into(),
                    agent::ALLOWED.join(","),
                    "--disallowedTools".into(),
                    agent::DENIED.join(","),
                ];
                // The second sentence continues the first. Without this
                // every `do:` is a stranger who has to be told the directory
                // and the desktop again.
                if !self.resume.is_empty() {
                    cmd.push("--resume".into());
                    cmd.push(self.resume.clone());
                }
                cmd
            }
            "codex" => vec![
                "codex".into(),
                "exec".into(),
                "--sandbox".into(),
                "workspace-write".into(),
                "--skip-git-repo-check".into(),
                "--color".into(),
                "never".into(),
                "-".into(),
            ],
            "gemini" => vec!["gemini".into(), "--approval-mode".into(), "plan".into()],
            _ => vec![a.bin.clone(), self.turn.text.clone()],
        }
    }

    /// Spawn the process and feed it. `sink` is the daemon's inbox: the
    /// reader thread posts the child's merged stdout+stderr there as
    /// `Event::Chunk`s and one `Event::RunEof` at the end, so the stream
    /// arrives with the same immediacy the script's `select` gave it.
    /// A spawn or feed failure marks the turn failed rather than leaving
    /// a card that says running forever.
    pub async fn start(
        &mut self,
        sink: tokio::sync::mpsc::UnboundedSender<Event>,
    ) -> Result<(), String> {
        let argv = self.command();
        let mut cmd = std::process::Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(&self.turn.cwd)
            .stdin(std::process::Stdio::piped());
        // stderr folded into the same stream the card reads, the way the
        // script's stderr=STDOUT did it: one pipe, both kinds of line.
        let (reader, writer) = std::io::pipe().map_err(|e| format!("pipe: {e}"))?;
        let writer2 = writer.try_clone().map_err(|e| e.to_string())?;
        cmd.stdout(std::process::Stdio::from(writer));
        cmd.stderr(std::process::Stdio::from(writer2));
        // Its own process group, which is the whole of the cancellation
        // story: an agent forks compilers and greps, and killing the one
        // pid it told us about leaves those running.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut proc = cmd.spawn().map_err(|e| e.to_string())?;

        if matches!(self.agent.id.as_str(), "claude" | "codex" | "gemini") {
            if let Some(why) = Self::feed(&mut proc, &self.instruction(), &self.agent.title) {
                self.proc = Some(proc);
                self.kill(&why).await;
                self.turn.state = "failed".into();
                return Ok(());
            }
        } else {
            // Closed straight away, so a CLI that reads stdin gets an
            // instruction and an EOF rather than waiting for a terminal
            // that is not there.
            drop(proc.stdin.take());
        }
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; 65536];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if sink.send(Event::Chunk(buf[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                }
            }
            let _ = sink.send(Event::RunEof);
        });
        self.proc = Some(proc);
        Ok(())
    }

    /// What goes down stdin. Claude carries its briefing in argv
    /// (--append-system-prompt); the others have no such flag, so it is
    /// part of the prompt itself.
    fn instruction(&self) -> String {
        if self.agent.id == "claude" {
            self.turn.text.clone()
        } else {
            format!("{}\n\n{}", self.turn.text, agent::BRIEF)
        }
    }

    /// The instruction down the pipe: all of it, or a reason it did not go.
    /// Half an instruction is not a smaller version of the instruction.
    fn feed(proc: &mut std::process::Child, instruction: &str, title: &str) -> Option<String> {
        let data = instruction.as_bytes();
        let mut stdin = proc.stdin.take()?;
        let mut sent = 0usize;
        while sent < data.len() {
            match stdin.write(&data[sent..]) {
                Ok(0) => break,
                Ok(n) => sent += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    return Some(format!(
                        "only {sent} of {} characters of the instruction reached {title}: {e}",
                        data.len()
                    ));
                }
            }
        }
        drop(stdin);
        if sent < data.len() {
            return Some(format!(
                "only {sent} of {} characters of the instruction reached {title}",
                data.len()
            ));
        }
        None
    }

    /// SIGTERM the whole group, then SIGKILL what ignored it — the script's
    /// `killpg` pair, shelled out because there is no libc dep here.
    #[cfg(unix)]
    fn signal_group(pid: u32, sig: &str) {
        let _ = std::process::Command::new("kill")
            .args([sig, &format!("-{pid}")])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    /// `proc.poll()`'s code: the number, a negative signal on Unix the way
    /// Python reports `-15`, or `None` when the status could not be had.
    fn exited(&mut self) -> Option<i32> {
        match self.proc.as_mut()?.try_wait() {
            Ok(Some(status)) => status.code().or_else(|| signal_code(&status)),
            _ => None,
        }
    }

    /// SIGTERM the whole group, then SIGKILL what ignored it — the script's
    /// `killpg` pair, shelled out because there is no libc dep here.
    pub async fn kill(&mut self, why: &str) {
        if let Some(proc) = self.proc.as_mut() {
            #[cfg(unix)]
            if proc.try_wait().ok().flatten().is_none() {
                Self::signal_group(proc.id(), "-TERM");
                let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
                while proc.try_wait().ok().flatten().is_none()
                    && std::time::Instant::now() < deadline
                {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                if proc.try_wait().ok().flatten().is_none() {
                    Self::signal_group(proc.id(), "-KILL");
                }
            }
            #[cfg(not(unix))]
            let _ = proc.kill();
        }
        self.finish_stop(why).await;
    }

    /// The state change a stop lands on the turn.
    async fn finish_stop(&mut self, why: &str) {
        if self.turn.state != "running" {
            return;
        }
        self.turn.state = "stopped".into();
        self.turn.note = why.to_string();
        self.turn.finished = agent::now();
        self.close_open_steps();
        // A stopped plan leaves the windows it already opened on screen,
        // so the card says how many rather than ending on "stopped" and
        // leaving somebody to count them.
        if self.turn.direct && self.turn.answer.is_empty() {
            let done = self.turn.steps.iter().filter(|e| e.state == "did").count();
            let total = plan::plan_for(&self.turn.text)
                .await
                .map(|p| p.steps.len())
                .unwrap_or(0);
            self.turn.answer = if total > 0 {
                format!("stopped after {done} of {total} steps")
            } else {
                "stopped part way".into()
            };
        }
    }

    fn close_open_steps(&mut self) {
        for e in &mut self.turn.steps {
            if e.state == "doing" {
                e.state = "did".into();
            }
        }
    }

    pub fn alive(&mut self) -> bool {
        self.proc
            .as_mut()
            .map(|p| matches!(p.try_wait(), Ok(None)))
            .unwrap_or(false)
    }

    /// A chunk of the merged stream — buffered and split into the lines
    /// `absorb` reads.
    pub fn absorb_chunk(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
        while let Some(nl) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=nl).collect();
            let line = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
            self.absorb(&line);
        }
    }

    /// The `!alive() and running` half of the script's `drain()`: a child
    /// that has gone ends the turn on its exit code, and whatever it left
    /// unflushed in the buffer is absorbed first.
    pub fn check_finished(&mut self) -> bool {
        if self.alive() || self.turn.state != "running" {
            return false;
        }
        if !self.buf.is_empty() {
            let line = String::from_utf8_lossy(&self.buf).into_owned();
            self.buf.clear();
            self.absorb(&line);
        }
        let code = self.exited();
        self.turn.finished = agent::now();
        self.close_open_steps();
        if code == Some(0) {
            self.turn.state = "done".into();
        } else {
            self.turn.state = "failed".into();
            if self.turn.note.is_empty() {
                if !self.noise.is_empty() {
                    self.turn.note = self.noise.join(" ");
                } else if self.turn.direct {
                    self.turn.note = "the desktop did not take that step".into();
                } else {
                    self.turn.note = format!("{} exited {}", self.agent.title, code_str(code));
                }
            }
        }
        true
    }

    fn absorb(&mut self, line: &str) {
        if self.turn.direct {
            self.absorb_plan(line);
        } else if self.agent.stream == Stream::Claude {
            self.absorb_claude(line);
        } else {
            self.absorb_plain(line);
        }
    }

    /// The deterministic runner's stream. Three events, because there are
    /// only three things the card can say about a step: it started, it
    /// ended, and here is what is now true.
    fn absorb_plan(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let Ok(ev) = serde_json::from_str::<Value>(line) else {
            self.push_noise(line);
            return;
        };
        if !ev.is_object() {
            return;
        }
        match ev.get("type").and_then(|t| t.as_str()) {
            Some("step") => {
                let text = ev.get("text").and_then(|t| t.as_str()).unwrap_or("");
                self.turn.push_step("desk", agent::short(text, 90));
            }
            Some("step_end") => {
                let ok = ev.get("ok").and_then(|o| o.as_bool()).unwrap_or(false);
                if let Some(entry) = self
                    .turn
                    .steps
                    .iter_mut()
                    .rev()
                    .find(|e| e.state == "doing")
                {
                    entry.state = if ok { "did" } else { "failed" }.into();
                }
                if !ok
                    && let Some(why) = ev.get("why").and_then(|w| w.as_str())
                    && !why.is_empty()
                {
                    self.turn.note = agent::short(why, 200);
                }
            }
            Some("result") => {
                let text = ev.get("result").and_then(|t| t.as_str()).unwrap_or("");
                self.turn.answer = agent::short(text, agent::MAX_ANSWER);
            }
            _ => {}
        }
    }

    /// No step list, because these CLIs do not publish one this can read.
    /// Their prose streams and the view says so.
    fn absorb_plain(&mut self, line: &str) {
        let text = agent::strip_ansi(line);
        let text = text.trim_end();
        if text.trim().is_empty() {
            return;
        }
        self.turn.answer.push('\n');
        self.turn.answer.push_str(text);
        tail_chars(&mut self.turn.answer, agent::MAX_ANSWER);
    }

    fn push_noise(&mut self, line: &str) {
        self.noise.push(agent::short(&agent::strip_ansi(line), 200));
        if self.noise.len() > 4 {
            let drop = self.noise.len() - 4;
            self.noise.drain(..drop);
        }
    }

    /// The claude `stream-json` grammar: init hands over the session id,
    /// assistant parts open steps and append text, user parts close them,
    /// permission_denied is the refusal the card has to name.
    fn absorb_claude(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let Ok(ev) = serde_json::from_str::<Value>(line) else {
            // stderr is folded into the same pipe, so a line that is not an
            // event is the CLI complaining. Kept, because a run that fails
            // with an empty card and no reason is the worst thing this can
            // do.
            self.push_noise(line);
            return;
        };
        if !ev.is_object() {
            return;
        }
        let kind = ev.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let subtype = ev.get("subtype").and_then(|t| t.as_str());

        if kind == "system" && subtype == Some("init") {
            // Kept so the next sentence resumes this session rather than
            // starting a conversation with somebody who has never met you.
            if let Some(id) = ev.get("session_id").and_then(|s| s.as_str()) {
                self.session = id.to_string();
            }
            return;
        }

        if kind == "assistant" {
            if let Some(parts) = ev
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
            {
                for part in parts {
                    match part.get("type").and_then(|t| t.as_str()) {
                        Some("tool_use") => self.step(part),
                        Some("text") => {
                            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                self.turn.answer.push_str(text);
                                tail_chars(&mut self.turn.answer, agent::MAX_ANSWER);
                            }
                        }
                        _ => {}
                    }
                }
            }
            return;
        }

        if kind == "system" && subtype == Some("permission_denied") {
            // The refusals are the half of the transcript that has to be
            // shown. An agent that quietly could not do the thing, and then
            // wrote a paragraph about what it would have done, is how
            // somebody comes to believe the launcher did something it did
            // not.
            let id = ev.get("tool_use_id").and_then(|t| t.as_str());
            let entry = id.and_then(|id| self.pending.remove(id));
            let what = entry
                .as_ref()
                .map(|(_, text)| text.clone())
                .or_else(|| {
                    ev.get("tool_name")
                        .and_then(|t| t.as_str())
                        .map(String::from)
                })
                .unwrap_or_else(|| "a tool".to_string());
            if let Some((seq, _)) = entry
                && let Some(step) = self.turn.steps.iter_mut().find(|s| s.seq == seq)
            {
                step.state = "blocked".into();
            }
            let why = ev.get("message").and_then(|m| m.as_str()).unwrap_or("");
            self.turn
                .blocked
                .push(json!({"text": what, "why": agent::short(why, 200)}));
            return;
        }

        if kind == "user" {
            if let Some(parts) = ev
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
            {
                for part in parts {
                    if part.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                        continue;
                    }
                    let Some(id) = part.get("tool_use_id").and_then(|t| t.as_str()) else {
                        continue;
                    };
                    let Some((seq, _)) = self.pending.remove(id) else {
                        continue;
                    };
                    let Some(step) = self.turn.steps.iter_mut().find(|s| s.seq == seq) else {
                        continue;
                    };
                    if step.state == "blocked" {
                        continue;
                    }
                    step.state = if part
                        .get("is_error")
                        .and_then(|e| e.as_bool())
                        .unwrap_or(false)
                    {
                        "failed"
                    } else {
                        "did"
                    }
                    .into();
                }
            }
            return;
        }

        if kind == "result" {
            if let Some(text) = ev.get("result").and_then(|t| t.as_str())
                && !text.trim().is_empty()
            {
                self.turn.answer = tail_str(text, agent::MAX_ANSWER);
            }
            if let Some(sub) = subtype
                && sub != "success"
            {
                self.turn.note = sub.to_string();
            }
        }
    }

    fn step(&mut self, part: &Value) {
        let name = part.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let inp = part.get("input").cloned().unwrap_or(Value::Null);
        let (kind, text) = describe(name, &inp);
        let seq = self.turn.push_step(kind, text.clone());
        if let Some(id) = part.get("id").and_then(|i| i.as_str()) {
            self.pending.insert(id.to_string(), (seq, text));
        }
    }
}

/// `proc.poll()`'s code printed the way Python's f-string prints it: the
/// number, or `None` when the status could not be had.
fn code_str(code: Option<i32>) -> String {
    code.map(|c| c.to_string()).unwrap_or_else(|| "None".into())
}

#[cfg(unix)]
fn signal_code(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal().map(|s| -s)
}

#[cfg(not(unix))]
fn signal_code(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// `s[-n:]` on chars — the tail a growing answer is capped at.
fn tail_str(s: &str, n: usize) -> String {
    let count = s.chars().count();
    if count <= n {
        return s.to_string();
    }
    s.chars().skip(count - n).collect()
}

fn tail_chars(s: &mut String, n: usize) {
    if s.chars().count() > n {
        *s = tail_str(s, n);
    }
}

/// One tool call as one line somebody can read. A step list is only worth
/// drawing if each line says what happened to something nameable; "Bash"
/// over and over is a progress bar with extra steps.
fn describe(name: &str, inp: &Value) -> (&'static str, String) {
    let get = |k: &str| inp.get(k).and_then(|v| v.as_str()).unwrap_or("");
    match name {
        "Read" => ("read", format!("Read {}", agent::tilde(get("file_path")))),
        "Glob" => ("find", format!("Find {}", agent::short(get("pattern"), 60))),
        "Grep" => (
            "find",
            format!("Search {}", agent::short(get("pattern"), 60)),
        ),
        "Write" | "Edit" | "NotebookEdit" => {
            ("write", format!("Write {}", agent::tilde(get("file_path"))))
        }
        "Skill" => (
            "think",
            format!("Read the {} skill", agent::short(get("skill"), 40)),
        ),
        "Bash" => {
            let cmd = agent::short(get("command"), 90);
            // `oxy-agent desk empty` reads as "empty", which says nothing.
            // The verb and its argument are what happened.
            if let Some(rest) = cmd.strip_prefix("oxy-agent desk ") {
                ("desk", desk::desk_line(rest))
            } else if cmd.starts_with("hyprctl") {
                ("desk", cmd)
            } else {
                let desc = get("description");
                (
                    "shell",
                    if desc.is_empty() {
                        cmd
                    } else {
                        desc.to_string()
                    },
                )
            }
        }
        "WebFetch" => ("web", format!("Fetch {}", agent::short(get("url"), 70))),
        "WebSearch" => (
            "web",
            format!("Search the web for {}", agent::short(get("query"), 60)),
        ),
        "Task" => (
            "think",
            agent::short(
                if get("description").is_empty() {
                    "Subagent"
                } else {
                    get("description")
                },
                70,
            ),
        ),
        "TodoWrite" => ("think", "Plan".into()),
        _ => (
            "think",
            if name.is_empty() {
                "step".into()
            } else {
                name.to_string()
            },
        ),
    }
}
