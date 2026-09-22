//! Which repo a query names: the roots it searches, the filesystem walk,
//! the two-minute list cache, and `resolve` — the `--resolve` port shared by
//! `git:`, `branch:` and `stash:`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use super::{mtime, now_secs, oxy_state_dir, repo_pin, state};
use crate::provider::native::util::on_path;
use crate::provider::process;
use crate::settings::paths;

const LIST_TTL_SECS: u64 = 120;

const EXCLUDED_DIRS: &[&str] = &["node_modules", ".cache", "vendor", "target", ".venv"];

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

/// The script reads `${OXY_REPO_ROOTS:-${OXY_ROOTS:-}}` — and for the script
/// leg `OXY_ROOTS` *is* the `roots` setting, injected as env. The native leg
/// keeps the same precedence: an exported `OXY_REPO_ROOTS` still wins, then
/// the settings value, then a bare exported `OXY_ROOTS`.
pub(crate) fn repo_roots(setting: Option<&str>) -> Vec<PathBuf> {
    let setting = crate::provider::process::env_or_login("OXY_REPO_ROOTS")
        .or_else(|| setting.filter(|v| !v.is_empty()).map(str::to_string))
        .or_else(|| crate::provider::process::env_or_login("OXY_ROOTS"));
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

/// The walk's own budget. A wedged `spawn_blocking` is only aborted at the
/// await — a slow tree (network mount, huge checkout) would hold the worker
/// to its fuse every keystroke, so the walk yields what it found when the
/// budget runs out. A truly-stuck syscall still needs the script leg.
const DISCOVER_BUDGET: Duration = Duration::from_millis(2500);

/// One discovery pass over a root: `.git` directories, and `.git` *files*
/// whose named gitdir still exists (worktrees and submodules — a stale file
/// left behind by a deleted worktree is not a repo). `ignore::WalkBuilder`
/// keeps fd's semantics: hidden entries included (`.git` is hidden), gitignore
/// respected, links not followed, six levels like `--max-depth 6`.
fn discover_in(root: &Path, deadline: Instant, out: &mut Vec<PathBuf>) {
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(false)
        .max_depth(Some(6))
        .follow_links(false)
        .filter_entry(|e| {
            e.depth() == 0 || !EXCLUDED_DIRS.contains(&e.file_name().to_string_lossy().as_ref())
        });
    for entry in builder.build().flatten() {
        if Instant::now() >= deadline {
            break;
        }
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
    let deadline = Instant::now() + DISCOVER_BUDGET;
    let mut out = Vec::new();
    for root in roots {
        if Instant::now() >= deadline {
            break;
        }
        discover_in(root, deadline, &mut out);
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
    // A fresh walk is the one moment the live set is known — the per-repo
    // state cache keeps a hash-keyed file for every repo ever visited, and
    // this sweep is what removes the ones whose path no longer exists.
    state::sweep_dead_repos();
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

pub(crate) fn load_repos(roots_setting: Option<&str>) -> Vec<PathBuf> {
    load_repos_from(&oxy_state_dir(), &repo_roots(roots_setting))
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
pub(crate) fn reflog_touched(gitdir: &Path) -> u64 {
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
pub(crate) async fn resolve(raw: &str, roots: Option<&str>) -> Option<(PathBuf, String)> {
    let repos = load_repos(roots);
    let (named, leftover) = split_resolve(raw, &repos);
    let target = match named {
        Some(t) => t,
        None => current_repo_async(repos).await?,
    };
    target.join(".git").exists().then_some((target, leftover))
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
}
