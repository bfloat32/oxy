//! `docker:` — every container, running ones first. A port of
//! `bin/oxy-docker`: `docker ps -aq` names the ids, one
//! `docker inspect --type container` carries health, restart count, the
//! real start time and the port map as a structure, and the script's jq
//! emit is `rows::build`.
//!
//!   docker:          every container, running then restarting then stopped
//!   docker:pg        by name, image, compose service, id or published port
//!
//! The manifest's `when` asks the daemon rather than the binary —
//! `docker info --format '{{.ServerVersion}}'` is the gate, re-probed per
//! query because the daemon can stop after the gate ran. A dead daemon is
//! `Empty`: the script printed nothing in that state, and `Fallback`
//! would only pay for the same dead end in shell.
//!
//! The read path stays read-only — `info`, `ps`, `inspect`, `stats` — so
//! the 2s `refreshMs` can never mark, start or stop anything. The verbs
//! live inside the action `exec` strings alone, the way the script kept
//! them.
//!
//! `docker stats --no-stream` costs a full second every time — it waits
//! for a second sample before it can divide one CPU reading by another —
//! and at an 80ms debounce the keyword cannot carry it. So the numbers
//! come out of a file nobody waits on: a query reads it while it is fresh
//! and arms a sampler when it is not, and the first `docker:` of a
//! session simply carries no CPU or memory until the next beat lands it.

mod rows;
#[cfg(test)]
mod tests;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::native::util::on_path;
use crate::provider::process::{self, Finished};
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::support::quote::quote;

pub struct Docker;

/// Per-call bound — the script had none, but a wedged daemon that answers
/// nothing is the difference between "no containers" and a hung launcher.
/// The manifest allows the whole answer 6s.
const CALL: Duration = Duration::from_secs(2);
/// Inspect gets more: it is the one call whose cost grows with the fleet.
const INSPECT: Duration = Duration::from_secs(4);
/// The sampler's bound — `docker stats` is a second by design; a wedged
/// one must not hold the flag forever.
const SAMPLE: Duration = Duration::from_secs(8);

/// The script's `--format`, verbatim: one JSON object per container
/// carrying only what the view reads. `--format '{{json .}}'` would be a
/// template shorter and twenty kilobytes per container longer. The Labels
/// guard is not decoration: `index` on a container with no labels at all
/// aborts the whole template, so one unlabelled container would empty the
/// keyword rather than losing its own compose name.
const INSPECT_TEMPLATE: &str = concat!(
    "{\"id\":{{json .Id}},\"name\":{{json .Name}},\"image\":{{json .Config.Image}},",
    "\"restarts\":{{json .RestartCount}},\"state\":{{json .State}},",
    "\"ports\":{{json .NetworkSettings.Ports}},\"memCap\":{{json .HostConfig.Memory}},",
    "\"nanoCpus\":{{json .HostConfig.NanoCpus}},",
    "\"project\":{{if .Config.Labels}}{{json (index .Config.Labels \"com.docker.compose.project\")}}{{else}}\"\"{{end}},",
    "\"service\":{{if .Config.Labels}}{{json (index .Config.Labels \"com.docker.compose.service\")}}{{else}}\"\"{{end}}}}"
);

/// `docker <args>` through `process::run` — the family's one shell-out,
/// the way `vcs/run.rs` wraps `git`. Quoting is the caller's job.
async fn docker(args: &str, timeout: Duration) -> Option<Finished> {
    process::run(&format!("docker {args}"), timeout).await
}

/// `${XDG_RUNTIME_DIR:-/tmp}/oxy-docker-stats.tsv` — the path the script
/// kept its stats cache on, so a sampler and a script-leg run share the
/// same file whichever leg wrote it.
fn stats_file() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    dir.join("oxy-docker-stats.tsv")
}

/// One sampler at a time — the part `flock -n` played in the script. The
/// worker serialises this provider's queries, so a process-local flag is
/// the same guarantee for a native run.
static SAMPLER: AtomicBool = AtomicBool::new(false);

/// Arm the sampler: `docker stats --no-stream --format …` on a task nobody
/// awaits, writing `file.<pid>` and renaming over the cache — the
/// script's `> tmp && mv tmp file || rm tmp`, so only a successful run
/// replaces the numbers and a dead daemon keeps the last true ones.
fn refresh_stats(file: PathBuf) {
    if SAMPLER.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::spawn(async move {
        let fin = docker(
            "stats --no-stream --format '{{.ID}}\t{{.CPUPerc}}\t{{.MemUsage}}\t{{.MemPerc}}'",
            SAMPLE,
        )
        .await;
        SAMPLER.store(false, Ordering::SeqCst);
        if let Some(fin) = fin
            && fin.code == Some(0)
        {
            let mut tmp = file.clone().into_os_string();
            tmp.push(format!(".{}", std::process::id()));
            let tmp = PathBuf::from(tmp);
            if std::fs::write(&tmp, &fin.stdout).is_ok() && std::fs::rename(&tmp, &file).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    });
}

/// The stats cache → (age in seconds, the `$stats` map the jq built).
/// The ages are the script's arithmetic: `99999` when the file is absent
/// or its mtime unreadable, `now - mtime` otherwise — a future stamp is
/// simply fresh, here as there. Older than ten seconds is dropped rather
/// than drawn: a container idle for a minute showing 40% is worse than
/// one showing nothing.
fn read_stats(file: &std::path::Path, now: i64) -> (i64, Map<String, Value>) {
    let age = std::fs::metadata(file)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|t| now - t.as_secs() as i64)
        .unwrap_or(99999);
    if age <= 10
        && let Ok(text) = std::fs::read_to_string(file)
    {
        return (age, rows::stats_map(&text));
    }
    (age, Map::new())
}

/// `$(nproc 2>/dev/null || echo 1)` — and the `--argjson` gate: jq dies
/// on a value that is not JSON, which is `None` here. An absent nproc is
/// the `|| echo 1`, never an error.
async fn host_cores() -> Option<f64> {
    let out = process::run("nproc", CALL)
        .await
        .map(|f| f.stdout.trim().to_string())
        .unwrap_or_default();
    if out.is_empty() {
        return Some(1.0);
    }
    serde_json::from_str::<f64>(&out).ok()
}

/// `awk '/^MemTotal:/ {print $2}' /proc/meminfo` × 1024 — a read, not a
/// call, so it is a read here too. The file being absent is the script's
/// `|| echo 0`; the file being present but carrying no MemTotal line fed
/// bash's arithmetic an empty operand and killed the run — the `None`
/// here is that death.
fn host_mem() -> Option<f64> {
    let Ok(text) = std::fs::read_to_string("/proc/meminfo") else {
        return Some(0.0);
    };
    let rest = text.lines().find_map(|l| l.strip_prefix("MemTotal:"))?;
    // An empty or non-numeric $2 is a bare word to bash's `$(( ))` — an
    // unset variable, a zero.
    let kb = rest
        .split_whitespace()
        .next()
        .and_then(|n| n.parse::<f64>().ok())
        .unwrap_or(0.0);
    Some(kb * 1024.0)
}

impl NativeExt for Docker {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        // `needle="${query,,}"` — the argument lowercased, untrimmed, as
        // the script took it.
        let needle = ctx.arg.to_lowercase();
        Box::pin(async move {
            // `command -v docker` — the manifest's `when` re-checked, as
            // every native does: a worker asks natives even when the gate
            // fails, so an absent docker declines to the script leg
            // rather than answering for it.
            if !on_path("docker") {
                return NativeOutcome::Fallback;
            }
            // `docker info` and `docker ps` are independent — sequential
            // 2s calls could sum past the manifest timeout under load, so
            // they run together. `info` is the gate: installed-but-down
            // prints nothing in the script, so Empty — not Fallback, which
            // would only pay for the same dead end in shell.
            let (info, ps) = tokio::join!(
                docker("info --format '{{.ServerVersion}}'", CALL),
                docker("ps --all --quiet", CALL),
            );
            let up = info.is_some_and(|f| f.code == Some(0));
            if !up {
                return NativeOutcome::Empty;
            }

            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);

            // The live numbers — read the cache if it is fresh, arm the
            // sampler if it is old enough to be worth redoing, and drop
            // it from the tiles past ten seconds.
            let file = stats_file();
            let (age, stats) = read_stats(&file, now);
            if age > 2 {
                refresh_stats(file);
            }

            // `docker ps --all --quiet` — one id per line, split on
            // whitespace like the script's unquoted `$ids`.
            let Some(ps) = ps else {
                return NativeOutcome::Empty;
            };
            // A `docker` that is not dockerd (a PATH wrapper) could put
            // shell-active text on this line — ids are hex by definition,
            // and anything else is dropped before it reaches the inspect
            // command.
            let ids: Vec<&str> = ps
                .stdout
                .split_whitespace()
                .filter(|id| {
                    (12..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_hexdigit())
                })
                .collect();
            if ids.is_empty() {
                return NativeOutcome::Empty;
            }

            // `inspect` and `nproc` are independent — joined so the two
            // bounded legs sum to the manifest's window rather than past it.
            let inspect_cmd = format!(
                "inspect --type container --format {} {}",
                quote(INSPECT_TEMPLATE),
                ids.join(" ")
            );
            let (fin, cores) = tokio::join!(docker(&inspect_cmd, INSPECT), host_cores());
            let Some(fin) = fin else {
                return NativeOutcome::Empty;
            };
            // `jq --slurp` — one object per inspect line; a line that is
            // not JSON is where jq died and the script answered nothing.
            let mut items = Vec::with_capacity(ids.len());
            for line in fin.stdout.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(item) = serde_json::from_str::<Value>(line) else {
                    return NativeOutcome::Empty;
                };
                items.push(item);
            }

            // What the machine has, so an unlimited container can be
            // drawn against something — `cores` already ran beside inspect.
            let (Some(cores), Some(hostmem)) = (cores, host_mem()) else {
                return NativeOutcome::Empty;
            };

            match rows::build(&items, &stats, now, cores, hostmem, &needle) {
                Some(rows) if !rows.is_empty() => NativeOutcome::Rows(rows),
                _ => NativeOutcome::Empty,
            }
        })
    }
}
