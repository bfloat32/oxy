//! `kill:` — find something that is running, see what it is costing, end it.
//! A port of `bin/oxy-kill`, on sysinfo rather than `ps | awk`.
//!
//! The rules that mattered there still hold:
//!
//!   * A bare `kill:` lists only your own processes, and never the tree this
//!     daemon is standing on — ancestors of this pid are excluded by walking
//!     ppid, not by name.
//!   * No kernel threads and no zombies: neither can be signalled.
//!   * One row per program, not per process: a child folds into its parent
//!     when the parent runs the same executable, and the row carries the whole
//!     family's CPU and memory.
//!   * Ordering is cost: `cpu% + megabytes / 64`.
//!
//! sysinfo's `cpu_usage` is the delta since the last refresh, which the
//! extension's `refreshMs` supplies for free — the snapshot file the script
//! kept in $XDG_RUNTIME_DIR exists to compute exactly this.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::{Ctx, NativeExt, NativeOutcome};

const MAX_ROWS: usize = 20;

pub struct Kill {
    sys: sysinfo::System,
}

impl Default for Kill {
    fn default() -> Self {
        Self::new()
    }
}

impl Kill {
    pub fn new() -> Kill {
        Kill {
            sys: sysinfo::System::new(),
        }
    }
}

/// The windows Hyprland knows about, keyed by pid. One cheap subprocess,
/// same as the script; absent on any other compositor, which simply means no
/// Focus action and no title match.
#[cfg(unix)]
fn windows() -> HashMap<u32, Win> {
    let Ok(out) = std::process::Command::new("hyprctl")
        .args(["clients", "-j"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return HashMap::new();
    };
    let Ok(list) = serde_json::from_slice::<Vec<Value>>(&out.stdout) else {
        return HashMap::new();
    };
    let mut by_pid: HashMap<u32, Win> = HashMap::new();
    for w in list {
        let Some(pid) = w.get("pid").and_then(|p| p.as_i64()) else {
            continue;
        };
        if pid <= 0 || !w.get("mapped").and_then(|m| m.as_bool()).unwrap_or(false) {
            continue;
        }
        let entry = by_pid.entry(pid as u32).or_insert_with(|| Win {
            addr: String::new(),
            class: String::new(),
            title: String::new(),
            workspace: String::new(),
            count: 0,
        });
        if entry.count == 0 {
            entry.addr = w
                .get("address")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .into();
            entry.class = w.get("class").and_then(|v| v.as_str()).unwrap_or("").into();
            entry.title = w.get("title").and_then(|v| v.as_str()).unwrap_or("").into();
            entry.workspace = w
                .get("workspace")
                .and_then(|s| s.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .into();
        }
        entry.count += 1;
    }
    by_pid
}

#[cfg(not(unix))]
fn windows() -> HashMap<u32, Win> {
    HashMap::new()
}

struct Win {
    addr: String,
    class: String,
    title: String,
    workspace: String,
    count: usize,
}

fn user_of(uid: Option<&sysinfo::Uid>) -> String {
    uid.and_then(|u| {
        sysinfo::Users::new_with_refreshed_list()
            .get_user_by_id(u)
            .map(|u| u.name().to_string())
    })
    .unwrap_or_else(|| uid.map(|u| format!("uid {}", **u)).unwrap_or_default())
}

impl NativeExt for Kill {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move {
            let query = ctx.arg.trim().to_lowercase();
            let bare = query.is_empty();

            // Refresh first: the ancestry walk and the uid lookup below read
            // the table this fills. sysinfo's cpu_usage is the delta since
            // the previous refresh, which refreshMs supplies.
            let wins = windows();
            self.sys
                .refresh_processes(sysinfo::ProcessesToUpdate::All, true);
            self.sys.refresh_memory();
            let my_uid = sysinfo::get_current_pid()
                .ok()
                .and_then(|pid| self.sys.process(pid))
                .and_then(|p| p.user_id().cloned());

            // The daemon's own ancestry: everything whose death takes the
            // launcher with it. Structural, so a rename cannot defeat it.
            let mut forbidden: HashSet<usize> = HashSet::new();
            if let Ok(mut pid) = sysinfo::get_current_pid() {
                for _ in 0..64 {
                    forbidden.insert(pid.as_u32() as usize);
                    match self.sys.process(pid).and_then(|p| p.parent()) {
                        Some(parent) if parent.as_u32() > 1 => pid = parent,
                        _ => break,
                    }
                }
            }
            forbidden.insert(1);

            let me = my_uid.clone();
            struct Cand {
                pid: usize,
                ppid: usize,
                uid: Option<sysinfo::Uid>,
                cpu: f32,
                mem: u64,
                age: u64,
                name: String,
                exe: String,
                cmd: String,
                stopped: bool,
            }

            let mut cands: HashMap<usize, Cand> = HashMap::new();
            for (pid, proc_) in self.sys.processes() {
                let pid = pid.as_u32() as usize;
                let cmdline: Vec<String> = proc_
                    .cmd()
                    .iter()
                    .map(|c| c.to_string_lossy().into_owned())
                    .collect();
                let cmd = cmdline.join(" ");
                // A kernel thread has a bracketed — or on Linux, empty —
                // command line. A Windows process may expose none at all and
                // is still a process; its name and exe carry it.
                if cmd.starts_with('[') || (cfg!(unix) && cmd.is_empty()) {
                    continue;
                }
                if forbidden.contains(&pid) {
                    continue;
                }
                // The same net the script cast for the shell it ran under.
                if cmd.contains("quickshell")
                    || cmd.contains("omarchy") && cmd.contains("shell")
                    || cmd.contains("oxyd")
                {
                    continue;
                }
                if bare && proc_.user_id() != me.as_ref() {
                    continue;
                }
                let exe = proc_
                    .exe()
                    .map(|e| e.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let mut name = proc_.name().to_string_lossy().into_owned();
                // The kernel keeps fifteen characters of a name; anything
                // that long is probably cut off, and the binary finishes the
                // word when the two agree.
                if name.len() >= 15 {
                    let base = exe.rsplit('/').next().unwrap_or("");
                    if base.starts_with(&name) {
                        name = base.to_string();
                    }
                }
                cands.insert(
                    pid,
                    Cand {
                        pid,
                        ppid: proc_.parent().map(|p| p.as_u32() as usize).unwrap_or(0),
                        uid: proc_.user_id().cloned(),
                        cpu: proc_.cpu_usage(),
                        mem: proc_.memory(),
                        age: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0)
                            .saturating_sub(proc_.start_time()),
                        name,
                        exe,
                        cmd,
                        stopped: matches!(
                            proc_.status(),
                            sysinfo::ProcessStatus::Stop | sysinfo::ProcessStatus::Zombie
                        ),
                    },
                );
            }

            // Fold children into a parent running the same executable, so
            // Chrome is one row with a count rather than forty renderers.
            fn leader_of(
                pid: usize,
                cands: &HashMap<usize, Cand>,
                memo: &mut HashMap<usize, usize>,
                depth: usize,
            ) -> usize {
                if let Some(l) = memo.get(&pid) {
                    return *l;
                }
                if depth > 48 {
                    return pid;
                }
                memo.insert(pid, pid);
                if let Some(c) = cands.get(&pid)
                    && let Some(parent) = cands.get(&c.ppid)
                    && c.ppid != pid
                    && parent.exe == c.exe
                    && !c.exe.is_empty()
                {
                    let l = leader_of(c.ppid, cands, memo, depth + 1);
                    memo.insert(pid, l);
                    return l;
                }
                pid
            }

            let mut memo: HashMap<usize, usize> = HashMap::new();
            for pid in cands.keys().copied().collect::<Vec<_>>() {
                leader_of(pid, &cands, &mut memo, 0);
            }

            struct Fam {
                cpu: f32,
                mem: u64,
                members: usize,
                hit: bool,
                win: Option<Win>,
            }
            let mut fams: HashMap<usize, Fam> = HashMap::new();
            for c in cands.values() {
                let leader = memo[&c.pid];
                let fam = fams.entry(leader).or_insert(Fam {
                    cpu: 0.0,
                    mem: 0,
                    members: 0,
                    hit: false,
                    win: None,
                });
                fam.cpu += c.cpu;
                fam.mem += c.mem;
                fam.members += 1;
                if fam.win.is_none()
                    && let Some(w) = wins.get(&(c.pid as u32))
                {
                    fam.win = Some(Win {
                        addr: w.addr.clone(),
                        class: w.class.clone(),
                        title: w.title.clone(),
                        workspace: w.workspace.clone(),
                        count: w.count,
                    });
                }
                if let Some(w) = wins.get(&(c.pid as u32))
                    && let Some(fam_win) = fam.win.as_mut()
                    && fam_win.addr != w.addr
                {
                    fam_win.count += w.count;
                }
                let hit = bare
                    || format!(
                        "{} {} {} {}",
                        c.name,
                        c.cmd,
                        wins.get(&(c.pid as u32))
                            .map(|w| w.class.as_str())
                            .unwrap_or(""),
                        wins.get(&(c.pid as u32))
                            .map(|w| w.title.as_str())
                            .unwrap_or("")
                    )
                    .to_lowercase()
                    .contains(&query);
                if hit {
                    fam.hit = true;
                }
            }

            let mut order: Vec<usize> = fams
                .iter()
                .filter(|(_, f)| f.hit)
                .map(|(l, _)| *l)
                .collect();
            // cost = cpu% + megabytes / 64, so a gigabyte weighs about as much
            // as 16% of a core.
            order.sort_by(|a, b| {
                let cost = |l: &usize| {
                    let f = &fams[l];
                    f.cpu + (f.mem as f32 / 1024.0 / 1024.0 / 64.0)
                };
                cost(b)
                    .partial_cmp(&cost(a))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            order.truncate(MAX_ROWS);

            let total = self.sys.total_memory();
            let used = self.sys.used_memory();
            let sys_cpu = self
                .sys
                .cpus()
                .first()
                .map(|c| c.cpu_usage())
                .unwrap_or(0.0);
            let home = crate::dirs::home().to_string_lossy().into_owned();

            let mut rows = Vec::new();
            for (i, leader) in order.iter().enumerate() {
                let fam = &fams[leader];
                let Some(c) = cands.get(leader) else { continue };
                let mut cmd = c.cmd.clone();
                if !home.is_empty() && cmd.starts_with(&format!("{home}/")) {
                    cmd = format!("~/{}", &cmd[home.len() + 1..]);
                }
                if cmd.len() > 96 {
                    cmd = format!("{}\u{2026}", &cmd[..95]);
                }

                // A window title says what the thing is; a command line says
                // what it was started as. For something the user opened, the
                // first is the answer.
                let subtitle = fam
                    .win
                    .as_ref()
                    .filter(|w| !w.title.is_empty())
                    .map(|w| w.title.clone())
                    .unwrap_or_else(|| cmd.clone());

                let mut actions = Vec::new();
                if let Some(w) = fam.win.as_ref() {
                    // Focus first: "I could not find it" is half of why anyone
                    // types this, and a panel whose default row is harmless is
                    // a kinder panel.
                    actions.push(json!({
                        "title": "Focus Window",
                        "exec": format!("hyprctl dispatch 'hl.dsp.focus({{ window = \"address:{}\" }})'", w.addr),
                    }));
                }
                actions.push(json!({
                    "title": "Terminate", "shortcut": "↵",
                    "exec": format!("kill -TERM {leader}"),
                }));
                actions.push(json!({
                    "title": "Force Kill", "exec": format!("kill -KILL {leader}"),
                }));
                actions.push(json!({
                    "title": "Copy PID",
                    "exec": format!("printf %s {leader} | wl-copy"),
                }));

                let mut row = json!({
                    "id": leader.to_string(),
                    "title": c.name,
                    "subtitle": subtitle,
                    "view": "processes",
                    "exec": format!("kill -TERM {leader}"),
                    "score": 90000 - i as i64 * 100,
                    "pid": leader,
                    "user": user_of(c.uid.as_ref()),
                    "own": c.uid == me,
                    "cpu": format!("{:.1}", fam.cpu).parse::<f64>().unwrap_or(0.0),
                    "cpuLive": true,
                    "mem": fam.mem,
                    "memShare": if total > 0 { fam.mem as f64 / total as f64 } else { 0.0 },
                    "age": c.age,
                    "count": fam.members,
                    "stopped": c.stopped,
                    "windowed": fam.win.is_some(),
                    "cmd": cmd,
                    "actions": actions,
                });
                if let Some(w) = fam.win.as_ref() {
                    row["winTitle"] = json!(w.title);
                    row["winClass"] = json!(w.class);
                    row["workspace"] = json!(w.workspace);
                    row["windows"] = json!(w.count);
                }
                if i == 0 {
                    // The machine rides on the first row, the way `git:`
                    // carries its repo: per-row numbers mean little without
                    // the total.
                    row["machine"] = json!({
                        "cpu": format!("{:.1}", sys_cpu).parse::<f64>().unwrap_or(0.0),
                        "memUsed": used,
                        "memTotal": total,
                        "procs": cands.len(),
                        "matched": order.len(),
                        "bare": bare,
                    });
                }
                rows.push(row);
            }
            NativeOutcome::Rows(rows)
        })
    }
}
