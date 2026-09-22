//! What `bin/oxy-repo` knows, as a library: where the repos are, which one a
//! query names, and the cached facts about each. `repo:` lists them;
//! `git:`/`branch:`/`stash:` start from `resolve`, which is the same matching
//! the script's `--resolve` mode does — pure string work over the cached
//! list, so the question "which repo" never forks `git status` per candidate
//! again (227ms a keystroke before that mode existed).
//!
//! Every piece that reads the environment or spawns a command sits in a thin
//! wrapper over a pure core so the logic is unit-testable without unsafe
//! `env::set_var`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use super::run;
use crate::provider::native::util::on_path;
use crate::provider::process;
use crate::settings::paths;

/// `repo:`'s listing cache lives two minutes, the script's CACHE_TTL: a repo
/// cloned in the last two minutes is not the one a launcher is reaching for.
const LIST_TTL_SECS: u64 = 120;
/// Rows a `repo:` answer carries — the script's MAX_ROWS.
pub(crate) const MAX_ROWS: usize = 12;
/// A cached dirty count older than this is re-derived in the background; the
/// worktree can move without anything under .git noticing (DIRTY_RECHECK).
const DIRTY_RECHECK_SECS: u64 = 3;
/// Ceiling on per-repo cache entries; a working set is far smaller.
const CACHE_KEEP: usize = 300;
/// Bump when the state-line format changes — the script's CACHE_FORMAT.
const REPO_STATE_FORMAT: u32 = 4;
/// The byte a repo path rides in on, because a tab inside one must not split
/// the line (the scripts' SEP — unit separator, never whitespace-collapsed).
pub(crate) const US: char = '\u{1f}';

const EXCLUDED_DIRS: &[&str] = &["node_modules", ".cache", "vendor", "target", ".venv"];

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn mtime(path: &Path) -> u64 {
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn cache_home() -> PathBuf {
    std::env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| paths::home().join(".cache"))
}

/// `$XDG_STATE_HOME/omarchy` — the scripts' `$STATE`, holding the repo list
/// and the pin.
pub(crate) fn oxy_state_dir() -> PathBuf {
    paths::state_home().join("omarchy")
}

fn repo_pin() -> PathBuf {
    oxy_state_dir().join("oxy-repo")
}

/// `~/path` for the row's `detail` — the scripts' `${repo/#$HOME/\~}`.
pub(crate) fn display_path(repo: &Path) -> String {
    let home = paths::home();
    let s = repo.to_string_lossy();
    match s.strip_prefix(&*home.to_string_lossy()) {
        Some(rest) if rest.starts_with('/') || rest.starts_with('\\') => format!("~{rest}"),
        _ => s.into_owned(),
    }
}

/// The scripts' `short_age`/`age`: `0m` under a minute (not `now` — these
/// families say minutes from zero), then h/d/mo/y.
pub(crate) fn age(secs: i64) -> String {
    let secs = secs.max(0);
    match secs {
        0..=3599 => format!("{}m", secs / 60),
        3600..=86399 => format!("{}h", secs / 3600),
        86400..=2591999 => format!("{}d", secs / 86400),
        2592000..=31535999 => format!("{}mo", secs / 2592000),
        _ => format!("{}y", secs / 31536000),
    }
}

// Roots and discovery
// ------------------------------------------------------------------

/// Pure root resolution: `OXY_REPO_ROOTS` (then `OXY_ROOTS`) wins over the
/// default candidate list — somebody who exported it in a terminal a second
/// ago means it more than a form filled in last month.
/// `:`-separated, the script's `IFS=: read` — except a `:` that is a drive
/// letter (`C:/…` or `C:\…`) belongs to its path, which is the shape
/// `OXY_REPO_ROOTS` arrives in on Windows.
fn split_roots(setting: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = setting.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ':'
            && cur.len() == 1
            && cur.chars().next().is_some_and(|d| d.is_ascii_alphabetic())
            && matches!(chars.peek(), Some('/') | Some('\\'))
        {
            cur.push(':');
        } else if c == ':' {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

fn roots_from(setting: Option<&str>, home: &Path) -> Vec<PathBuf> {
    let candidates: Vec<String> = match setting {
        Some(s) if !s.is_empty() => split_roots(s),
        _ => [
            "localhost",
            "Projects",
            "projects",
            "Work",
            "work",
            "src",
            "code",
            "dev",
            "repos",
            "git",
            "Developer",
        ]
        .iter()
        .map(|n| home.join(n).to_string_lossy().into_owned())
        .collect(),
    };
    candidates
        .into_iter()
        .map(|c| {
            if let Some(rest) = c.strip_prefix('~') {
                home.join(rest.trim_start_matches(['/', '\\']))
            } else {
                PathBuf::from(c)
            }
        })
        .filter(|c| c.is_dir())
        .collect()
}

pub(crate) fn repo_roots() -> Vec<PathBuf> {
    let setting = std::env::var("OXY_REPO_ROOTS")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var("OXY_ROOTS").ok().filter(|v| !v.is_empty()));
    roots_from(setting.as_deref(), &paths::home())
}

/// Is `path` an absolute path — `/x` or `C:/x` (git on Windows writes drive
/// letters into `gitdir:` lines). The script's `[A-Za-z]:/*` rule.
fn is_abs(path: &str) -> bool {
    let b = path.as_bytes();
    b.first().is_some_and(|c| *c == b'/')
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && (b[2] == b'/' || b[2] == b'\\'))
}

/// The real git directory — a file rather than a directory inside a worktree
/// or submodule, holding one `gitdir: <path>` line.
pub(crate) fn git_dir(repo: &Path) -> Option<PathBuf> {
    let dotgit = repo.join(".git");
    if dotgit.is_file() {
        let body = std::fs::read_to_string(&dotgit).ok()?;
        let line = body.lines().next()?.trim_end();
        let target = line.strip_prefix("gitdir: ")?;
        let dir = if is_abs(target) {
            PathBuf::from(target)
        } else {
            repo.join(target)
        };
        return Some(dir);
    }
    dotgit.is_dir().then_some(dotgit)
}

/// One discovery pass over a root: `.git` directories, and `.git` *files*
/// whose named gitdir still exists (worktrees and submodules — a stale file
/// left behind by a deleted worktree is not a repo). `ignore::WalkBuilder`
/// keeps fd's semantics: hidden entries included (`.git` is hidden), gitignore
/// respected, links not followed, six levels like `--max-depth 6`.
fn discover_in(root: &Path, out: &mut Vec<PathBuf>) {
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(false)
        .max_depth(Some(6))
        .follow_links(false)
        .filter_entry(|e| {
            e.depth() == 0 || !EXCLUDED_DIRS.contains(&e.file_name().to_string_lossy().as_ref())
        });
    for entry in builder.build().flatten() {
        if entry.file_name() != ".git" {
            continue;
        }
        let Some(parent) = entry.path().parent() else {
            continue;
        };
        let is_repo = if entry.file_type().is_some_and(|t| t.is_dir()) {
            true
        } else {
            // A `.git` file points at the real gitdir; it counts only while
            // that directory is still there.
            git_dir(parent).is_some_and(|d| d.is_dir())
        };
        if is_repo {
            out.push(parent.to_path_buf());
        }
    }
}

fn discover(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        discover_in(root, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

/// The discovered list, cached for LIST_TTL_SECS and written atomically
/// (temp then rename — three extensions ask at the same keystroke and a
/// truncated write read as "no repos" until the next run).
fn load_repos_from(state: &Path, roots: &[PathBuf]) -> Vec<PathBuf> {
    let cache = state.join("oxy-repos.list");
    if let Ok(meta) = cache.metadata()
        && let Ok(t) = meta.modified()
        && let Ok(d) = t.duration_since(UNIX_EPOCH)
    {
        let age = now_secs().saturating_sub(d.as_secs());
        if age < LIST_TTL_SECS
            && let Ok(body) = std::fs::read_to_string(&cache)
        {
            let repos: Vec<PathBuf> = body
                .lines()
                .filter(|l| !l.is_empty())
                .map(PathBuf::from)
                .collect();
            if !repos.is_empty() {
                return repos;
            }
        }
    }

    let repos = discover(roots);
    if !repos.is_empty() {
        let _ = std::fs::create_dir_all(state);
        let tmp = state.join(format!("oxy-repos.list.{}", std::process::id()));
        let body = repos
            .iter()
            .map(|r| r.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        if std::fs::write(&tmp, format!("{body}\n")).is_ok() {
            let _ = std::fs::rename(&tmp, &cache);
        }
        let _ = std::fs::remove_file(&tmp);
    }
    repos
}

pub(crate) fn load_repos() -> Vec<PathBuf> {
    load_repos_from(&oxy_state_dir(), &repo_roots())
}

// Which repo a word names
// ------------------------------------------------------------------

/// `up_to_repo`: walk ancestors until one holds `.git` (file or dir). A
/// relative start is made absolute against the working directory first, so
/// the loop always terminates.
fn up_to_repo(start: &Path) -> Option<PathBuf> {
    let mut d = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(start)
    };
    loop {
        if d.join(".git").exists() {
            return Some(d);
        }
        if !d.pop() {
            return None;
        }
    }
}

/// The branch without forking git — one read of `.git/HEAD`, for the filters
/// that need a fact about every candidate.
pub(crate) fn head_branch(gitdir: &Path) -> Option<String> {
    let line = std::fs::read_to_string(gitdir.join("HEAD")).ok()?;
    let line = line.lines().next()?.trim_end();
    Some(
        line.strip_prefix("ref: refs/heads/")
            .map(str::to_string)
            .unwrap_or_else(|| line.chars().take(7).collect()),
    )
}

/// When the reflog last moved — "the repo I am working in", better than the
/// mtime of a build artefact.
fn reflog_touched(gitdir: &Path) -> u64 {
    mtime(&gitdir.join("logs/HEAD"))
}

/// `find_repo`'s scoring: exact name, then name prefix, then name contains,
/// then path contains; ties break on the freshest reflog, same as the
/// listing.
fn match_score(needle: &str, repo: &Path) -> Option<u64> {
    let name = repo.file_name()?.to_string_lossy().to_lowercase();
    let path = repo.to_string_lossy().to_lowercase();
    if name == needle {
        Some(50000)
    } else if name.starts_with(needle) {
        Some(40000)
    } else if name.contains(needle) {
        Some(25000)
    } else if path.contains(needle) {
        Some(10000)
    } else {
        None
    }
}

fn find_repo(needle: &str, repos: &[PathBuf]) -> Option<PathBuf> {
    let needle = needle.to_lowercase();
    if needle.is_empty() {
        return None;
    }
    let mut best: Option<(PathBuf, u64, u64)> = None;
    for repo in repos {
        let Some(score) = match_score(&needle, repo) else {
            continue;
        };
        let Some(gd) = git_dir(repo) else { continue };
        let ts = reflog_touched(&gd);
        let better = match &best {
            None => true,
            Some((_, s, t)) => score > *s || (score == *s && ts > *t),
        };
        if better {
            best = Some((repo.clone(), score, ts));
        }
    }
    best.map(|(r, _, _)| r)
}

/// "The repo I am in", in the order of how deliberately the user said so:
/// `OXY_REPO`, then the pin this extension's action writes, then the cwd of
/// the terminal that had focus, then the freshest reflog.
fn current_repo_from(
    env_repo: Option<&str>,
    pin: &Path,
    terminal_cwd: Option<&str>,
    repos: &[PathBuf],
) -> Option<PathBuf> {
    if let Some(v) = env_repo.filter(|v| !v.is_empty()) {
        let p = PathBuf::from(v.trim_end_matches('/'));
        if let Some(r) = up_to_repo(&p) {
            return Some(r);
        }
    }
    if let Ok(body) = std::fs::read_to_string(pin)
        && let Some(line) = body.lines().next()
    {
        let p = PathBuf::from(line);
        if p.join(".git").exists() {
            return Some(p);
        }
    }
    if let Some(cwd) = terminal_cwd.filter(|v| !v.is_empty())
        && let Some(r) = up_to_repo(Path::new(cwd))
    {
        return Some(r);
    }
    repos
        .iter()
        .filter_map(|r| git_dir(r).map(|g| (reflog_touched(&g), r)))
        .max_by_key(|(ts, _)| *ts)
        .filter(|(ts, _)| *ts > 0)
        .map(|(_, r)| r.clone())
}

async fn current_repo_async(repos: Vec<PathBuf>) -> Option<PathBuf> {
    // The focused terminal's cwd — Omarchy's own terminal-in-cwd trick. Only
    // under Hyprland and only while the helper exists.
    let terminal_cwd = if std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
        .ok()
        .is_some_and(|v| !v.is_empty())
        && on_path("omarchy-cmd-terminal-cwd")
    {
        process::run("omarchy-cmd-terminal-cwd", Duration::from_secs(2))
            .await
            .map(|f| f.stdout.trim().to_string())
    } else {
        None
    };
    let env_repo = std::env::var("OXY_REPO").ok();
    current_repo_from(
        env_repo.as_deref(),
        &repo_pin(),
        terminal_cwd.as_deref(),
        &repos,
    )
}

/// `oxy-repo --resolve`'s matching half: the whole raw query as a repo name
/// first, then its first word, then nothing — the leftover is the filter.
/// Pure over the loaded list; the caller supplies the current repo or asks
/// for it.
fn split_resolve(raw: &str, repos: &[PathBuf]) -> (Option<PathBuf>, String) {
    let raw = raw.trim();
    if raw.is_empty() {
        return (None, String::new());
    }
    if let Some(t) = find_repo(raw, repos) {
        return (Some(t), String::new());
    }
    if let Some((first, rest)) = raw.split_once(char::is_whitespace)
        && let Some(t) = find_repo(first, repos)
    {
        return (Some(t), rest.trim_start().to_string());
    }
    // A word that names no repo is a filter on the one you are in.
    (None, raw.to_string())
}

/// The pure `resolve` — tests hand it the list and the current repo so it
/// never touches the environment.
#[cfg(test)]
pub(crate) fn resolve_with(
    raw: &str,
    repos: &[PathBuf],
    current: Option<PathBuf>,
) -> Option<(PathBuf, String)> {
    let (named, leftover) = split_resolve(raw, repos);
    let target = named.or(current)?;
    target.join(".git").exists().then_some((target, leftover))
}

/// `oxy-repo --resolve`: which repo this query is about, and what is left
/// over as the filter. No git calls — string work over the cached list.
pub(crate) async fn resolve(raw: &str) -> Option<(PathBuf, String)> {
    let repos = load_repos();
    let (named, leftover) = split_resolve(raw, &repos);
    let target = match named {
        Some(t) => t,
        None => current_repo_async(repos).await?,
    };
    target.join(".git").exists().then_some((target, leftover))
}

// Remotes
// ------------------------------------------------------------------

/// The `origin` url read straight out of `.git/config` — the same answer git
/// would give, without a fork per displayed row.
pub(crate) fn origin_url(gitdir: &Path) -> Option<String> {
    let body = std::fs::read_to_string(gitdir.join("config")).ok()?;
    let mut section = String::new();
    let mut url = None;
    for line in body.lines() {
        let line = line.trim_start();
        if line.starts_with('[') {
            section = line.to_string();
        } else if line.starts_with("url") && line.contains('=') {
            if !section.starts_with("[remote \"origin\"]") {
                continue;
            }
            url = line.split_once('=').map(|(_, v)| v.trim().to_string());
        }
    }
    url.filter(|u| !u.is_empty())
}

/// scp-style, ssh://, git:// and https:// all name the same page in a
/// browser.
pub(crate) fn web_url(remote: &str) -> Option<String> {
    let u = remote.trim_end_matches('/').trim_end_matches(".git");
    if let Some(rest) = u.strip_prefix("git@")
        && let Some((host, path)) = rest.split_once(':')
    {
        return Some(format!("https://{host}/{path}"));
    }
    for prefix in ["ssh://git@", "ssh://", "git://"] {
        if let Some(rest) = u.strip_prefix(prefix) {
            return Some(format!("https://{rest}"));
        }
    }
    if u.starts_with("http://") || u.starts_with("https://") {
        return Some(u.to_string());
    }
    None
}

/// `owner/name`, for handing to `gh` and `pr:` — first path segment + the
/// rest, the script's `${path%%/*}/${path#*/}`.
pub(crate) fn name_with_owner(web: &str) -> Option<String> {
    let path = web.strip_prefix("https://github.com/")?;
    let (owner, rest) = path.split_once('/')?;
    Some(format!("{owner}/{rest}"))
}

/// A repo's remote facts: url, browser page, github slug. Falls back to
/// `git remote get-url origin` when the config read missed.
pub(crate) async fn remote_facts(repo: &Path, gitdir: &Path) -> (String, String, String) {
    let remote = match origin_url(gitdir) {
        Some(u) => u,
        None => run::git_in(
            &repo.to_string_lossy(),
            "remote get-url origin",
            Duration::from_secs(3),
        )
        .await
        .map(|f| f.stdout.trim().to_string())
        .unwrap_or_default(),
    };
    if remote.is_empty() {
        return (String::new(), String::new(), String::new());
    }
    let web = web_url(&remote).unwrap_or_default();
    let slug = name_with_owner(&web).unwrap_or_default();
    (remote, web, slug)
}

// Per-repo state and its cache
// ------------------------------------------------------------------

/// What `git status --porcelain=v2 --branch` says about one repo, counted the
/// way the scripts count it: `entries` is every non-header line (repo:'s
/// `dirty`), while staged/changed/conflicted are the split `git:` and
/// `branch:` draw.
#[derive(Debug, Default, Clone)]
pub(crate) struct Status {
    pub branch: String,
    pub upstream: String,
    /// Raw `# branch.ab` payload, e.g. `+2 -1`.
    pub ab: String,
    pub entries: u64,
    pub staged: u64,
    pub changed: u64,
    pub conflicted: u64,
}

/// Parse porcelain v2 `--branch` output. `xy` is field two of a `1 `/`2 `
/// entry: X staged, Y in the worktree. Untracked lines count as changed.
pub(crate) fn parse_porcelain(stdout: &str) -> Status {
    let mut st = Status::default();
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            st.branch = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("# branch.upstream ") {
            st.upstream = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            st.ab = rest.to_string();
        } else if line.starts_with('#') {
            // other headers — ignored
        } else {
            st.entries += 1;
            if line.starts_with("u ") {
                st.conflicted += 1;
            } else if line.starts_with("1 ") || line.starts_with("2 ") {
                if let Some(xy) = line.split(' ').nth(1) {
                    let mut c = xy.chars();
                    if c.next().is_some_and(|x| x != '.') {
                        st.staged += 1;
                    }
                    if c.next().is_some_and(|y| y != '.') {
                        st.changed += 1;
                    }
                }
            } else if line.starts_with("? ") {
                st.changed += 1;
            }
        }
    }
    if st.branch == "(detached)" {
        st.branch = "detached".to_string();
    }
    st
}

/// `+2 -1` → `(2, 1)` — `repo:`'s drift numbers and the branch rows' upstream
/// drift share the reading.
pub(crate) fn parse_ab(ab: &str) -> (u64, u64) {
    let mut parts = ab.split(' ');
    let strip = |s: Option<&str>| {
        s.unwrap_or("0")
            .trim_start_matches(['+', '-'])
            .parse()
            .unwrap_or(0)
    };
    (strip(parts.next()), strip(parts.next()))
}

/// `%(upstream:track,nobracket)` is prose: "ahead 2, behind 1", or "gone", or
/// empty. The view draws the two numbers, so they are pulled out here rather
/// than shipped as a sentence. Returns (ahead, behind, gone).
pub(crate) fn parse_track(track: &str) -> (u64, u64, bool) {
    let gone = track == "gone";
    let num = |word: &str| -> u64 {
        track
            .split([',', ' '])
            .collect::<Vec<_>>()
            .windows(2)
            .find(|w| w[0] == word)
            .and_then(|w| w[1].parse().ok())
            .unwrap_or(0)
    };
    (num("ahead"), num("behind"), gone)
}

/// The newest mtime under .git that a repo's *state* cannot change without —
/// the script's exact list for the repo-state stamp. (`branch:` and `stash:`
/// stamp on different sets below.)
pub(crate) fn repo_stamp(gitdir: &Path) -> u64 {
    ["index", "HEAD", "refs", "packed-refs"]
        .iter()
        .map(|f| mtime(&gitdir.join(f)))
        .max()
        .unwrap_or(0)
}

/// `branch:`'s stamp: a branch cannot move, appear or be checked out without
/// one of these. The scripts also stamp their own `$0` — a native has no
/// script file, so its format version plays that role; bump it when the
/// reading changes.
pub(crate) fn branches_stamp(gitdir: &Path) -> u64 {
    [
        "index",
        "HEAD",
        "refs/heads",
        "refs",
        "packed-refs",
        "logs/HEAD",
    ]
    .iter()
    .map(|f| mtime(&gitdir.join(f)))
    .max()
    .unwrap_or(0)
}

/// `stash:`'s stamp: a stash cannot be pushed, popped or dropped without the
/// stash ref moving — a commit must not throw this answer away.
pub(crate) fn stashes_stamp(gitdir: &Path) -> u64 {
    ["refs/stash", "logs/refs/stash", "packed-refs"]
        .iter()
        .map(|f| mtime(&gitdir.join(f)))
        .max()
        .unwrap_or(0)
}

/// One `git status` + one `git log`, the two forks a repo row pays on a cold
/// cache — `--no-optional-locks` so a background read never holds the index.
pub(crate) async fn read_status(repo: &Path) -> Status {
    let Some(f) = run::git_in(
        &repo.to_string_lossy(),
        "status --porcelain=v2 --branch --untracked-files=normal",
        Duration::from_secs(4),
    )
    .await
    else {
        return Status::default();
    };
    parse_porcelain(&f.stdout)
}

/// `git log -1 --format=%ct` — when this repo last committed.
pub(crate) async fn last_commit(repo: &Path) -> Option<u64> {
    run::git_in(
        &repo.to_string_lossy(),
        "log -1 --format=%ct",
        Duration::from_secs(3),
    )
    .await
    .and_then(|f| f.stdout.trim().parse().ok())
}

/// `~/.cache/oxy/v<fmt>/<name>` — one dir per cache kind, made on demand.
pub(crate) fn named_cache_dir(name: &str, format: u32) -> PathBuf {
    let dir = cache_home()
        .join("oxy")
        .join(format!("v{format}"))
        .join(name);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// The per-repo cache key: 16 hex chars, the same length md5sum|cut gave.
/// `DefaultHasher::new()` is fixed-key — deterministic across runs, which a
/// cache filename needs, and the different algorithm means a native entry
/// never collides with a script-written one in the shared directory.
pub(crate) fn cache_key(path: &Path) -> String {
    let mut h = DefaultHasher::new();
    path.to_string_lossy().hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Read a stamped cache file: line one is the stamp and must equal `want`,
/// the rest are records. `None` on any mismatch — a wrong answer on disk is
/// recomputed, never served.
pub(crate) fn read_stamped(file: &Path, want: &str) -> Option<Vec<String>> {
    let body = std::fs::read_to_string(file).ok()?;
    let mut lines = body.lines();
    if lines.next()?.trim_end() != want {
        return None;
    }
    Some(lines.map(str::to_string).collect())
}

/// Atomic write (temp then rename) — the synchronous path and the background
/// refresher write the same entry, and a reader must never see half a file.
pub(crate) fn write_lines(file: &Path, lines: &[String]) -> bool {
    let Some(dir) = file.parent() else {
        return false;
    };
    let _ = std::fs::create_dir_all(dir);
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        file.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    let ok = std::fs::write(&tmp, lines.join("\n") + "\n").is_ok()
        && std::fs::rename(&tmp, file).is_ok();
    let _ = std::fs::remove_file(&tmp);
    ok
}

/// The scripts' prune rule for one named cache: delete every other format's
/// copy of it, then cap this one at CACHE_KEEP entries by oldest mtime.
pub(crate) fn prune_named_cache(name: &str, keep_dir: &Path) {
    let cache_root = cache_home().join("oxy");
    if let Ok(entries) = std::fs::read_dir(&cache_root) {
        for entry in entries.flatten() {
            let old = entry.path().join(name);
            if old.is_dir() && old != keep_dir {
                let _ = std::fs::remove_dir_all(&old);
            }
        }
    }
    let mut entries: Vec<(u64, PathBuf)> = std::fs::read_dir(keep_dir)
        .map(|rd| rd.flatten().map(|e| (mtime(&e.path()), e.path())).collect())
        .unwrap_or_default();
    let excess = entries.len().saturating_sub(CACHE_KEEP);
    if excess == 0 {
        return;
    }
    entries.sort_by_key(|(t, _)| *t);
    for (_, path) in entries.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

/// repo-state v4 line: `stamp␟branch␟upstream␟ab␟dirty␟committed␟dirty_ts`.
/// The row cache `repo:` reads per displayed repo; `repo_stamp` is the key
/// and the last field is when the dirty count was last derived.
#[derive(Debug, Clone)]
pub(crate) struct RepoState {
    pub stamp: u64,
    pub branch: String,
    pub upstream: String,
    pub ab: String,
    pub dirty: u64,
    pub committed: u64,
    pub dirty_ts: u64,
}

fn parse_state_line(line: &str) -> Option<RepoState> {
    let mut f = line.split(US);
    Some(RepoState {
        stamp: f.next()?.trim().parse().ok()?,
        branch: f.next()?.to_string(),
        upstream: f.next()?.to_string(),
        ab: f.next()?.to_string(),
        dirty: f.next()?.parse().unwrap_or(0),
        committed: f.next()?.parse().unwrap_or(0),
        dirty_ts: f.next()?.parse().unwrap_or(0),
    })
}

fn state_line(s: &RepoState) -> String {
    [
        s.stamp.to_string(),
        s.branch.clone(),
        s.upstream.clone(),
        s.ab.clone(),
        s.dirty.to_string(),
        s.committed.to_string(),
        s.dirty_ts.to_string(),
    ]
    .join(&US.to_string())
}

/// One repo's state for the listing, served from the v4 cache while the
/// stamp holds — a hit rewrites the line so its mtime says when it was last
/// wanted (that is what the pruning reads), and a dirty count older than
/// DIRTY_RECHECK spawns the background recompute rather than being trusted.
pub(crate) async fn repo_state(repo: &Path, gitdir: &Path) -> RepoState {
    let dir = named_cache_dir("repo-state", REPO_STATE_FORMAT);
    let key = dir.join(cache_key(repo));
    let stamp = repo_stamp(gitdir);

    if let Ok(cached_line) = std::fs::read_to_string(&key)
        && let Some(cached) = parse_state_line(cached_line.trim_end())
        && cached.stamp == stamp
    {
        // Rewritten so the file's mtime says when this answer was last
        // wanted — the only thing that keeps a cache of dead repos from
        // growing. `dirty_ts` passes through unchanged: refreshing it here
        // would make every hit look fresh and the revalidate never fire.
        write_state(&key, &cached);
        if now_secs().saturating_sub(cached.dirty_ts) > DIRTY_RECHECK_SECS {
            refresh_state_async(repo);
        }
        return cached;
    }

    let status = read_status(repo).await;
    let mut branch = status.branch.clone();
    if branch.is_empty() {
        branch = head_branch(gitdir).unwrap_or_else(|| "?".to_string());
    }
    if branch == "(detached)" {
        branch = "detached".to_string();
    }
    let committed = last_commit(repo)
        .await
        .unwrap_or_else(|| reflog_touched(gitdir));
    let state = RepoState {
        stamp,
        branch,
        upstream: status.upstream,
        ab: status.ab,
        dirty: status.entries,
        committed,
        dirty_ts: now_secs(),
    };
    write_state(&key, &state);
    state
}

fn write_state(key: &Path, s: &RepoState) {
    let _ = write_lines(key, &[state_line(s)]);
}

/// The background half of stale-while-revalidate: the script re-executes
/// itself for one repo with nobody waiting; the native spawns the same
/// recompute as a task — it must write the native-keyed entry, which a
/// delegated `oxy-repo --refresh-state` would not (its key is md5).
fn refresh_state_async(repo: &Path) {
    let repo = repo.to_path_buf();
    tokio::spawn(async move {
        let Some(gitdir) = git_dir(&repo) else { return };
        let stamp = repo_stamp(&gitdir);
        let status = read_status(&repo).await;
        let mut branch = status.branch;
        if branch.is_empty() {
            branch = head_branch(&gitdir).unwrap_or_else(|| "?".to_string());
        }
        let committed = last_commit(&repo).await.unwrap_or(0);
        let state = RepoState {
            stamp,
            branch,
            upstream: status.upstream,
            ab: status.ab,
            dirty: status.entries,
            committed,
            dirty_ts: now_secs(),
        };
        let dir = named_cache_dir("repo-state", REPO_STATE_FORMAT);
        write_state(&dir.join(cache_key(&repo)), &state);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_gitdir(root: &Path, name: &str, head: &str) -> PathBuf {
        let gd = root.join(name).join(".git");
        std::fs::create_dir_all(&gd).unwrap();
        std::fs::write(gd.join("HEAD"), head).unwrap();
        gd
    }

    #[test]
    fn roots_prefer_the_exported_setting() {
        let tmp = std::env::temp_dir().join(format!("oxy-roots-{}", std::process::id()));
        let a = tmp.join("a");
        let b = tmp.join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let home = tmp.join("home");
        std::fs::create_dir_all(home.join("repos")).unwrap();

        let setting = format!("{}:{}", a.display(), b.display());
        let roots = roots_from(Some(&setting), &home);
        assert_eq!(roots, vec![a.clone(), b.clone()]);

        let roots = roots_from(None, &home);
        assert_eq!(roots, vec![home.join("repos")]);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn git_dir_reads_worktree_pointers() {
        let tmp = std::env::temp_dir().join(format!("oxy-gitdir-{}", std::process::id()));
        let repo = tmp.join("wt");
        std::fs::create_dir_all(&repo).unwrap();
        let real = tmp.join("main/.git/worktrees/wt");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(repo.join(".git"), format!("gitdir: {}\n", real.display())).unwrap();
        assert_eq!(git_dir(&repo).as_deref(), Some(real.as_path()));
        // A stale pointer — the gitdir is gone — is not a repo.
        std::fs::remove_dir_all(&real).unwrap();
        assert!(git_dir(&repo).is_some()); // the file still resolves
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn head_branch_reads_head_without_git() {
        let tmp = std::env::temp_dir().join(format!("oxy-head-{}", std::process::id()));
        let gd = fixture_gitdir(&tmp, "r", "ref: refs/heads/main\n");
        assert_eq!(head_branch(&gd).as_deref(), Some("main"));
        let gd2 = fixture_gitdir(&tmp, "d", "abc1234def\n");
        assert_eq!(head_branch(&gd2).as_deref(), Some("abc1234"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_repo_scores_and_ties() {
        let tmp = std::env::temp_dir().join(format!("oxy-find-{}", std::process::id()));
        let gd = fixture_gitdir(&tmp, "omarchy", "ref: refs/heads/main\n");
        std::fs::create_dir_all(gd.join("logs")).unwrap();
        let repos = vec![tmp.join("omarchy"), tmp.join("other")];
        assert_eq!(find_repo("omarchy", &repos).unwrap(), tmp.join("omarchy"));
        assert_eq!(find_repo("omar", &repos).unwrap(), tmp.join("omarchy"));
        assert!(find_repo("nope", &repos).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_names_then_falls_back() {
        let tmp = std::env::temp_dir().join(format!("oxy-resolve-{}", std::process::id()));
        fixture_gitdir(&tmp, "omarchy", "ref: refs/heads/main\n");
        fixture_gitdir(&tmp, "oxy-fixture", "ref: refs/heads/main\n");
        let repos = vec![tmp.join("omarchy"), tmp.join("oxy-fixture")];
        let current = Some(tmp.join("oxy-fixture"));

        // A bare query is the repo you are in, no filter.
        let (t, l) = resolve_with("", &repos, current.clone()).unwrap();
        assert_eq!(t, tmp.join("oxy-fixture"));
        assert_eq!(l, "");

        // A repo name resolves, nothing left over.
        let (t, l) = resolve_with("omarchy", &repos, current.clone()).unwrap();
        assert_eq!(t, tmp.join("omarchy"));
        assert_eq!(l, "");

        // Name + words: first word names the repo, the rest filters.
        let (t, l) = resolve_with("omarchy fix", &repos, current.clone()).unwrap();
        assert_eq!(t, tmp.join("omarchy"));
        assert_eq!(l, "fix");

        // A word naming no repo filters the one you are in.
        let (t, l) = resolve_with("login", &repos, current).unwrap();
        assert_eq!(t, tmp.join("oxy-fixture"));
        assert_eq!(l, "login");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn porcelain_counts_the_way_the_scripts_do() {
        let out = "# branch.oid abc\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +2 -1\n1 .M N... 100644 100644 100644 abc def file.rs\n1 M. N... 100644 100644 100644 abc def staged.rs\nu UU N... 0 0 0 0 0 0 clash.rs\n? new.txt\n";
        let st = parse_porcelain(out);
        assert_eq!(st.branch, "main");
        assert_eq!(st.upstream, "origin/main");
        assert_eq!(st.ab, "+2 -1");
        assert_eq!(st.entries, 4);
        assert_eq!(st.staged, 1);
        assert_eq!(st.changed, 2);
        assert_eq!(st.conflicted, 1);
        assert_eq!(parse_ab(&st.ab), (2, 1));
    }

    #[test]
    fn track_prose_becomes_numbers() {
        assert_eq!(parse_track(""), (0, 0, false));
        assert_eq!(parse_track("ahead 2, behind 1"), (2, 1, false));
        assert_eq!(parse_track("ahead 4"), (4, 0, false));
        assert_eq!(parse_track("gone"), (0, 0, true));
    }

    #[test]
    fn remote_urls_map_the_way_the_script_does() {
        assert_eq!(
            web_url("git@github.com:bfloat32/oxy.git").as_deref(),
            Some("https://github.com/bfloat32/oxy")
        );
        assert_eq!(
            web_url("ssh://git@github.com/bfloat32/oxy").as_deref(),
            Some("https://github.com/bfloat32/oxy")
        );
        assert_eq!(
            web_url("https://github.com/a/b.git").as_deref(),
            Some("https://github.com/a/b")
        );
        assert_eq!(
            name_with_owner("https://github.com/bfloat32/oxy").as_deref(),
            Some("bfloat32/oxy")
        );
        assert!(name_with_owner("https://gitlab.com/a/b").is_none());
    }

    #[test]
    fn state_lines_round_trip() {
        let s = RepoState {
            stamp: 42,
            branch: "main".into(),
            upstream: String::new(),
            ab: "+1 -0".into(),
            dirty: 3,
            committed: 100,
            dirty_ts: 99,
        };
        let parsed = parse_state_line(&state_line(&s)).unwrap();
        assert_eq!(parsed.stamp, 42);
        assert_eq!(parsed.branch, "main");
        assert_eq!(parsed.dirty, 3);
        // An empty middle field survives the unit-separator round trip — the
        // whole reason the scripts moved off tabs.
        assert!(parsed.upstream.is_empty());
    }

    #[test]
    fn ages_read_like_the_scripts() {
        assert_eq!(age(0), "0m");
        assert_eq!(age(59), "0m");
        assert_eq!(age(60), "1m");
        assert_eq!(age(3600), "1h");
        assert_eq!(age(86400), "1d");
        assert_eq!(age(2592000), "1mo");
        assert_eq!(age(31536000), "1y");
        assert_eq!(age(-3), "0m");
    }
}
