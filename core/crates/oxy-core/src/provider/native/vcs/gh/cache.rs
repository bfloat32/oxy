//! The script's answer cache, kept byte-identical: one directory
//! (`$XDG_STATE_HOME/omarchy/oxy-gh`), entries named `<fp>-<key>.json`
//! where `fp` fingerprints the gh credentials and `key` is md5 of the
//! question. A native run and a script run read each other's entries —
//! which is why the digests are real md5s and not the family's usual
//! `DefaultHasher`.
//!
//! `peek` is the non-blocking read — a fresh entry answers, a stale one
//! answers and arms `warm` to replace it — and `pull` is the blocking one
//! for questions the typed text cannot answer at all. `warm` is what the
//! script's `setsid --fork` was: a spawned task nobody awaits, writing
//! `file.new` over `file` when the request lands.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use super::md5::md5_hex;
use crate::provider::process;
use crate::settings::paths;

/// `GH_TIMEOUT` — every request's bound, as `timeout` inside the line so
/// the semantics (exit 124, empty stdout) match the script's.
pub(crate) const GH_TIMEOUT: Duration = Duration::from_secs(8);

/// `CACHE_KEEP` — entries older than the newest 200 are evicted on the
/// same walk that counts them.
const CACHE_KEEP: usize = 200;

/// `tried_recently`'s window: a failed warm is not retried for five
/// seconds, which is what stops `refreshMs` from firing a request a second
/// against a dead network.
const TRIED_SECS: i64 = 5;

/// What one query's cache context needs. `cold` is the `!offline` flag's
/// second half: read nothing an earlier request left behind.
pub(crate) struct Session {
    pub offline: bool,
    pub cold: bool,
    pub fp: String,
    pub dir: PathBuf,
    pub now: i64,
}

pub(crate) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `${XDG_STATE_HOME:-~/.local/state}/omarchy/oxy-gh`.
pub(crate) fn cache_dir() -> PathBuf {
    paths::state_home().join("omarchy").join("oxy-gh")
}

/// `auth_fingerprint`: `mtime:size` of gh's `hosts.yml` plus the token
/// envs, md5'd — a logout or token swap can never serve somebody else's
/// repositories, and anything written under another fingerprint is swept
/// rather than aged out.
pub(crate) fn auth_fingerprint() -> String {
    let hosts = std::env::var_os("GH_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| paths::home().join(".config").join("gh"))
        .join("hosts.yml");
    let stamp = std::fs::metadata(&hosts)
        .ok()
        .filter(|m| m.is_file())
        .and_then(|m| {
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|t| format!("{}:{}", t.as_secs(), m.len()))
        })
        .unwrap_or_default();
    let material = format!(
        "{stamp}|{}|{}",
        std::env::var("GH_TOKEN").unwrap_or_default(),
        std::env::var("GITHUB_TOKEN").unwrap_or_default()
    );
    md5_hex(material.as_bytes())[..12].to_string()
}

/// `key_for` — md5 of the question, 16 hex chars.
pub(crate) fn key_for(input: &str) -> String {
    md5_hex(input.as_bytes())[..16].to_string()
}

/// `cache_file`: the entry a key lives under for this fingerprint.
fn cache_file(s: &Session, key: &str) -> PathBuf {
    s.dir.join(format!("{}-{key}.json", s.fp))
}

/// `mkdir` plus the two sweeps the script runs on every invocation:
/// foreign-fingerprint entries deleted, the current fingerprint capped at
/// CACHE_KEEP oldest-first, and orphan `.lock` files (a warm that never
/// wrote its answer) removed.
pub(crate) fn housekeeping(dir: &Path, fp: &str) {
    let _ = std::fs::create_dir_all(dir);
    let prefix = format!("{fp}-");
    let mut kept: Vec<(u64, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".json") {
                continue;
            }
            if name.starts_with(&prefix) {
                kept.push((mtime(&e.path()), e.path()));
            } else {
                let _ = std::fs::remove_file(e.path());
                let _ = std::fs::remove_file(lock_of(&e.path()));
            }
        }
    }
    if kept.len() > CACHE_KEEP {
        kept.sort_by_key(|(t, _)| *t);
        let excess = kept.len() - CACHE_KEEP;
        for (_, p) in kept.into_iter().take(excess) {
            let _ = std::fs::remove_file(&p);
            let _ = std::fs::remove_file(lock_of(&p));
        }
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(base) = name.strip_suffix(".lock")
                && base.ends_with(".json")
                && !e.path().with_file_name(base).exists()
            {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

fn mtime(p: &Path) -> u64 {
    p.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|t| t.as_secs())
        .unwrap_or(0)
}

/// `<file>.lock` — appended, not an extension swap: `x.json.lock`.
fn lock_of(file: &Path) -> PathBuf {
    let mut s = file.as_os_str().to_os_string();
    s.push(".lock");
    PathBuf::from(s)
}

/// `<file>.new` — the write target `mv`'d over the entry.
fn new_of(file: &Path) -> PathBuf {
    let mut s = file.as_os_str().to_os_string();
    s.push(".new");
    PathBuf::from(s)
}

/// `fresh`: present, non-empty, younger than the ttl.
fn fresh(file: &Path, ttl: u64, now: i64) -> bool {
    let ok = std::fs::metadata(file).ok().is_some_and(|m| m.len() > 0);
    if !ok {
        return false;
    }
    let age = now - mtime(file) as i64;
    (0..ttl as i64).contains(&age)
}

/// `usable` — `jq -e .`: parses to a value that is neither null nor false.
/// A GraphQL NOT_FOUND is an answer and is cached; a torn body is not.
fn usable(body: &str) -> bool {
    if body.is_empty() {
        return false;
    }
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(v) => !matches!(v, serde_json::Value::Null | serde_json::Value::Bool(false)),
        Err(_) => false,
    }
}

/// `json_only` — `sed -n '/^[[{]/,$p'`: everything from the first line
/// that opens a JSON document, so a shim's chatter line in front of an
/// answer is not a parse failure. Bytes are kept verbatim — sed's `p`
/// does not strip the newline.
pub(crate) fn json_only(stdout: &str) -> String {
    let mut off = 0;
    for line in stdout.split_inclusive('\n') {
        if line.starts_with('[') || line.starts_with('{') {
            return stdout[off..].to_string();
        }
        off += line.len();
    }
    String::new()
}

/// `tried_recently` — the lock's mtime is the last warm attempt's stamp.
fn tried_recently(file: &Path, now: i64) -> bool {
    let lock = lock_of(file);
    if !lock.exists() {
        return false;
    }
    let age = now - mtime(&lock) as i64;
    (0..TRIED_SECS).contains(&age)
}

/// Warms currently in flight — the `flock -n` half, process-local. The
/// lock file itself is still stamped first, because `tried_recently`
/// (and the script leg, when it owns a query) reads the mtime.
static INFLIGHT: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// `warm`: stamp the lock, take the in-flight slot, and fetch into
/// `file.new` → `file` on a task nobody waits on. A request that fails
/// leaves the lock's mtime as the "we tried" record and nothing else.
fn warm(file: PathBuf, cmd: String) {
    // `exec 9>"$file.lock"` — the open itself is the stamp.
    if std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(lock_of(&file))
        .is_err()
    {
        return;
    }
    if !INFLIGHT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(file.clone())
    {
        return; // `flock -n` lost — a warm is already out
    }
    tokio::spawn(async move {
        // `timeout "$GH_TIMEOUT" "$@"` — the bound is inside the line,
        // like the script's, so an 8s kill is exit 124 and empty stdout.
        let out = process::run(
            &format!("timeout {} {cmd}", GH_TIMEOUT.as_secs()),
            GH_TIMEOUT + Duration::from_secs(2),
        )
        .await
        .map(|f| json_only(&f.stdout))
        .unwrap_or_default();
        if usable(&out) {
            let tmp = new_of(&file);
            if std::fs::write(&tmp, &out).is_ok() && std::fs::rename(&tmp, &file).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
        INFLIGHT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&file);
    });
}

/// Read a stale entry, the `[[ -s $file ]] && cat` tail of both readers.
fn stale(file: &Path) -> Option<String> {
    let ok = std::fs::metadata(file).ok().is_some_and(|m| m.len() > 0);
    if ok {
        std::fs::read_to_string(file).ok()
    } else {
        None
    }
}

/// `peek`: the non-blocking read. Cold misses outright; fresh answers;
/// offline serves whatever is there; otherwise a warm is armed once per
/// TRIED_SECS and the stale entry — if any — answers meanwhile.
pub(crate) async fn peek(s: &Session, key: &str, ttl: u64, cmd: &str) -> Option<String> {
    if s.cold {
        return None;
    }
    let file = cache_file(s, key);
    if fresh(&file, ttl, s.now) {
        return std::fs::read_to_string(&file).ok();
    }
    if s.offline {
        return stale(&file);
    }
    if !tried_recently(&file, s.now) {
        warm(file.clone(), cmd.to_string());
    }
    stale(&file)
}

/// `pull`: the blocking read, for the questions the text cannot answer.
/// The manifest's `timeoutMs` is the real ceiling; the `timeout` inside
/// the line is the script's own bound.
pub(crate) async fn pull(s: &Session, key: &str, ttl: u64, cmd: &str) -> Option<String> {
    if s.cold {
        return None;
    }
    let file = cache_file(s, key);
    if fresh(&file, ttl, s.now) {
        return std::fs::read_to_string(&file).ok();
    }
    if s.offline {
        return stale(&file);
    }
    let out = process::run(
        &format!("timeout {} {cmd}", GH_TIMEOUT.as_secs()),
        GH_TIMEOUT + Duration::from_secs(2),
    )
    .await
    .map(|f| json_only(&f.stdout))
    .unwrap_or_default();
    if usable(&out) {
        let tmp = new_of(&file);
        if std::fs::write(&tmp, &out).is_ok() {
            let _ = std::fs::rename(&tmp, &file);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
        return Some(out);
    }
    stale(&file)
}
