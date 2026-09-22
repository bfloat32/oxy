//! What is true inside one repo: the porcelain-v2 counts, the remote facts,
//! the three stamps, and the per-repo state cache with its
//! stale-while-revalidate refresh.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{US, cache_home, git_dir, head_branch, mtime, now_secs, reflog_touched};
use crate::provider::native::vcs::run;

const DIRTY_RECHECK_SECS: u64 = 3;

const CACHE_KEEP: usize = 300;

const REPO_STATE_FORMAT: u32 = 5;

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

/// repo-state v5 line: `stamp␟branch␟upstream␟ab␟dirty␟committed␟dirty_ts␟path`.
/// The row cache `repo:` reads per displayed repo; `repo_stamp` is the key
/// and the last two fields are when the dirty count was last derived and
/// which repo the entry belongs to — what the dead-repo sweep reads, since
/// the filename is only a hash.
#[derive(Debug, Clone)]
pub(crate) struct RepoState {
    pub stamp: u64,
    pub branch: String,
    pub upstream: String,
    pub ab: String,
    pub dirty: u64,
    pub committed: u64,
    pub dirty_ts: u64,
    pub path: String,
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
        path: f.next()?.to_string(),
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
        s.path.clone(),
    ]
    .join(&US.to_string())
}

/// One repo's state for the listing, served from the v5 cache while the
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
        path: repo.to_string_lossy().into_owned(),
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
            path: repo.to_string_lossy().into_owned(),
        };
        let dir = named_cache_dir("repo-state", REPO_STATE_FORMAT);
        write_state(&dir.join(cache_key(&repo)), &state);
    });
}

/// The eviction the v4 cache never had: a v5 entry carries its repo's path,
/// so a discovery refresh can delete entries whose repo no longer exists
/// instead of only capping by count. Script-written lines share the dir but
/// not the format — they fail the parse and are left alone, and so is an
/// entry whose path still exists: leaving the roots is not being deleted.
/// Runs on a fresh discovery, not per query — the answer is a day old at
/// worst, and a deleted repo costs a few dozen bytes until then.
pub(crate) fn sweep_dead_repos() {
    let dir = named_cache_dir("repo-state", REPO_STATE_FORMAT);
    prune_named_cache("repo-state", &dir);
    sweep_dead_repos_in(&dir);
}

fn sweep_dead_repos_in(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let file = entry.path();
        let gone = std::fs::read_to_string(&file)
            .ok()
            .and_then(|line| parse_state_line(line.trim_end()))
            .is_some_and(|s| !Path::new(&s.path).is_dir());
        if gone {
            let _ = std::fs::remove_file(&file);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            path: "/tmp/repo".into(),
        };
        let parsed = parse_state_line(&state_line(&s)).unwrap();
        assert_eq!(parsed.stamp, 42);
        assert_eq!(parsed.branch, "main");
        assert_eq!(parsed.dirty, 3);
        assert_eq!(parsed.path, "/tmp/repo");
        // An empty middle field survives the unit-separator round trip — the
        // whole reason the scripts moved off tabs.
        assert!(parsed.upstream.is_empty());
    }

    #[test]
    fn the_sweep_drops_dead_repos_and_keeps_the_rest() {
        let dir = std::env::temp_dir().join(format!("oxy-sweep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let live = dir.join("live-repo");
        std::fs::create_dir_all(&live).unwrap();
        let state = |path: &str| RepoState {
            stamp: 1,
            branch: "main".into(),
            upstream: String::new(),
            ab: String::new(),
            dirty: 0,
            committed: 0,
            dirty_ts: 0,
            path: path.into(),
        };
        // Ours, still on disk; ours, deleted; a seven-field line in the v4
        // shape — the sweep cannot know whose repo it was, so it stays.
        let ours_live = dir.join("aaaaaaaaaaaaaaaa");
        let ours_dead = dir.join("bbbbbbbbbbbbbbbb");
        let foreign = dir.join("cccccccccccccccc");
        write_state(&ours_live, &state(&live.to_string_lossy()));
        write_state(&ours_dead, &state(&dir.join("gone-repo").to_string_lossy()));
        std::fs::write(&foreign, "1\u{1f}main\u{1f}\u{1f}\u{1f}0\u{1f}0\u{1f}0\n").unwrap();

        sweep_dead_repos_in(&dir);

        assert!(ours_live.exists());
        assert!(!ours_dead.exists());
        assert!(foreign.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
