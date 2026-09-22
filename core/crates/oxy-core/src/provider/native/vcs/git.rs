//! `git:` — the state of the repo you are in, without opening a terminal
//! to ask. A port of `bin/oxy-git`.
//!
//! A bare `git:` is not a search, it is "show me this repo" — one row whose
//! `gitrepo` view draws the whole picture: branch, drift, uncommitted work,
//! recent commits, the remote. Anything typed after the colon is a search
//! again, and a panel cannot be filtered, so the grouped rows take over.
//!
//! The repo comes from `repos::resolve` — the same string matching
//! `oxy-repo --resolve` does, so `git:omarchy` names a repo and `git:fix`
//! filters the one you are in. Every `git` call rides `run::git_in`
//! (`--no-optional-locks -C <repo>` through `bash -c`), so a read never
//! takes the lock a commit is holding.

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use super::{repos, run};
use crate::provider::native::util::{on_path, shq};
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::support::quote::quote;

/// The script's log format, verbatim: `$'…'` ANSI-C quoting is what turns
/// `\x1f` into a real unit separator once the line reaches `bash -c` —
/// plain single quotes would hand git a literal backslash and split
/// nothing.
const LOG_FORMAT: &str = "--format=$'%h\\x1f%s\\x1f%an\\x1f%ct'";

/// Rows the commit search shows — the script's `shown < 16`.
const MAX_SHOWN: usize = 16;

#[derive(Default)]
pub struct Git;

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The script's `keep`: the needle is matched against what the row says —
/// "title detail group", lowercased — never pushed down into the git calls,
/// because the sections have nothing in common to filter on. `needle`
/// arrives already lowercased.
fn keep(needle: &str, title: &str, detail: &str, group: &str) -> bool {
    needle.is_empty()
        || format!("{title} {detail} {group}")
            .to_lowercase()
            .contains(needle)
}

/// `2 staged,1 changed` — the script joins with `IFS=", "`, and bash uses
/// only the first byte of IFS when expanding `"${parts[*]}"`: a bare comma,
/// no space. Verified against the script; the missing space is not a typo.
fn summary(status: &repos::Status) -> String {
    let mut parts = Vec::new();
    if status.staged > 0 {
        parts.push(format!("{} staged", status.staged));
    }
    if status.changed > 0 {
        parts.push(format!("{} changed", status.changed));
    }
    if status.conflicted > 0 {
        parts.push(format!("{} conflicted", status.conflicted));
    }
    if parts.is_empty() {
        parts.push("clean".to_string());
    }
    parts.join(",")
}

/// The diff-or-log decision the panel and the status row share: Enter shows
/// what changed, and on a clean repo — where `git diff` opens a terminal
/// that closes before it can be read — it shows the log instead.
fn primary(quoted: &str, status: &repos::Status) -> (&'static str, String) {
    let base = format!("omarchy-launch-tui --app-id=org.omarchy.git git -C {quoted}");
    if status.staged + status.changed + status.conflicted > 0 {
        ("Show Diff", format!("{base} diff"))
    } else {
        ("Show Log", format!("{base} log --stat -20"))
    }
}

/// One `hash␟subject␟author␟ct` line → the commit object the panel
/// carries. `age` stays a number of seconds — the `gitrepo` view renders
/// it. jq's `select(length >= 4)` drops a short line; `tonumber` failing on
/// the timestamp aborts the whole filter in the script, so a bad stamp
/// costs the list, not just the line.
fn parse_commits(stdout: &str, now: i64) -> Vec<Value> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let f: Vec<&str> = line.split('\u{1f}').collect();
        if f.len() < 4 {
            continue;
        }
        let Ok(ts) = f[3].trim().parse::<i64>() else {
            return Vec::new();
        };
        out.push(json!({
            "hash": f[0],
            "subject": f[1],
            "author": f[2],
            "age": now - ts,
        }));
    }
    out
}

/// The script's `emit`: a row is printed only when `keep` passes, and the
/// score drops a hundred per *emitted* row — an offered-but-filtered row
/// costs nothing. A non-empty `goto` turns the primary action into a
/// launcher `query` and blanks the row's `exec`: that is how the branch
/// row reaches `branch:` instead of running a command. The signature is the
/// script's argv, kept positional so the two read side by side.
#[allow(clippy::too_many_arguments)]
fn emit(
    needle: &str,
    score: &mut i64,
    id: &str,
    title: &str,
    detail: &str,
    subtitle: &str,
    accessory: &str,
    group: &str,
    primary: &str,
    exec: &str,
    extra: Vec<Value>,
    goto: &str,
) -> Option<Value> {
    if !keep(needle, title, detail, group) {
        return None;
    }
    let first = if goto.is_empty() {
        json!({"title": primary, "shortcut": "↵", "exec": exec})
    } else {
        json!({"title": primary, "shortcut": "↵", "query": goto})
    };
    let mut actions = Vec::with_capacity(extra.len() + 1);
    actions.push(first);
    actions.extend(extra);
    let row = json!({
        "id": id,
        "title": title,
        "detail": detail,
        "subtitle": subtitle,
        "accessory": accessory,
        "group": group,
        "exec": if goto.is_empty() { exec } else { "" },
        "score": *score,
        "actions": actions,
    });
    *score -= 100;
    Some(row)
}

/// A bare `git:` — one panel row carrying the whole repo: state, drift,
/// stashes, the last commits, and the things you were about to open anyway.
async fn panel(
    repo_str: &str,
    quoted: &str,
    name: &str,
    display: &str,
    status: &repos::Status,
    remote: &str,
    now: i64,
) -> NativeOutcome {
    // "Did I stash that" is asked from here, and the answer is a number —
    // `stash list | wc -l`.
    let stashes = run::git_in(repo_str, "stash list", Duration::from_secs(3))
        .await
        .map(|f| f.stdout.lines().count() as u64)
        .unwrap_or(0);
    let commits = run::git_in(
        repo_str,
        &format!("log -6 {LOG_FORMAT}"),
        Duration::from_secs(3),
    )
    .await
    .map(|f| parse_commits(&f.stdout, now))
    .unwrap_or_default();
    let (ahead, behind) = repos::parse_ab(&status.ab);
    let (primary_title, exec) = primary(quoted, status);
    NativeOutcome::Rows(vec![json!({
        "id": "git-panel",
        "view": "gitrepo",
        "score": 99000,
        "title": name,
        "subtitle": status.branch,
        "detail": display,
        "repo": {
            "name": name,
            "path": display,
            "branch": status.branch,
            "upstream": status.upstream,
            "ahead": ahead,
            "behind": behind,
            "staged": status.staged,
            "changed": status.changed,
            "conflicted": status.conflicted,
            "stashes": stashes,
            "remote": remote,
        },
        "commits": commits,
        "exec": exec,
        "actions": [
            {"title": primary_title, "shortcut": "↵", "exec": exec},
            // The depth is one keypress away rather than a syntax to know —
            // both land on keywords you can also type.
            {"title": "Branches", "query": format!("branch:{name}")},
            {"title": "Stashes", "query": format!("stash:{name}")},
            {"title": "Open in Lazygit",
             "exec": format!("omarchy-launch-tui --app-id=org.omarchy.lazygit lazygit -p {quoted}")},
            // herdr takes no path — it is a session manager, not a viewer.
            // A session named after the repo is the closest thing it has to
            // "open this project".
            {"title": "Open in Herdr",
             "exec": format!("setsid uwsm-app -- xdg-terminal-exec --dir={quoted} herdr --session {}", quote(name))},
            {"title": "Open in Editor", "exec": format!("omarchy-launch-editor {quoted}")},
            {"title": "Open Terminal Here",
             "exec": format!("setsid uwsm-app -- xdg-terminal-exec --dir={quoted}")},
        ],
    })])
}

/// `git:word` — the same facts as grouped rows: status, the branch, then
/// recent commits, each surviving `keep` and scored a hundred apart.
async fn search(
    repo_str: &str,
    quoted: &str,
    name: &str,
    display: &str,
    status: &repos::Status,
    needle: &str,
    now: i64,
) -> NativeOutcome {
    let mut rows = Vec::new();
    let mut score = 90000i64;

    let (primary_title, primary_exec) = primary(quoted, status);
    if let Some(row) = emit(
        needle,
        &mut score,
        "status",
        &summary(status),
        display,
        name,
        "",
        "Status",
        primary_title,
        &primary_exec,
        vec![
            json!({"title": "Open Terminal Here",
                   "exec": format!("setsid uwsm-app -- xdg-terminal-exec --dir={quoted}")}),
            json!({"title": "Full Status",
                   "exec": format!("omarchy-launch-tui --app-id=org.omarchy.git git -C {quoted} status")}),
        ],
        "",
    ) {
        rows.push(row);
    }

    let (ahead, behind) = repos::parse_ab(&status.ab);
    let mut track = if status.upstream.is_empty() {
        "no upstream".to_string()
    } else {
        status.upstream.clone()
    };
    if ahead > 0 {
        track = format!("{track} ↑{ahead}");
    }
    if behind > 0 {
        track = format!("{track} ↓{behind}");
    }
    if let Some(row) = emit(
        needle,
        &mut score,
        "branch",
        &status.branch,
        &track,
        name,
        "",
        "Branch",
        "Every Branch",
        "true",
        vec![
            json!({"title": "Copy Branch Name",
                   "exec": format!("printf %s {} | wl-copy", quote(&status.branch))}),
            json!({"title": "Fetch",
                   "exec": format!("out=$(git -C {quoted} fetch --all --prune 2>&1) && omarchy-notification-send \"Fetched\" \"$out\" || omarchy-notification-send -u normal \"git fetch failed\" \"$out\"")}),
        ],
        &format!("branch:{name}"),
    ) {
        rows.push(row);
    }

    // Forty read, sixteen shown: with a word after `git:` this is "when did
    // I do a thing", and eight commits does not reach back far enough —
    // but a one-letter needle matches nearly every subject, so the cap
    // stops paying for rows nobody will scroll to.
    if let Some(f) = run::git_in(
        repo_str,
        &format!("log -40 {LOG_FORMAT}"),
        Duration::from_secs(4),
    )
    .await
    {
        let mut shown = 0;
        for line in f.stdout.lines() {
            let f: Vec<&str> = line.split('\u{1f}').collect();
            if f.len() < 4 || f[0].is_empty() {
                continue;
            }
            let (hash, subject, author) = (f[0], f[1], f[2]);
            // Asked before `emit` too, the way the script does — the cap
            // counts kept rows, not offered ones.
            if !keep(needle, subject, author, "Recent Commits") {
                continue;
            }
            if shown >= MAX_SHOWN {
                break;
            }
            shown += 1;
            let ts: i64 = f[3].trim().parse().unwrap_or(0);
            let show =
                format!("omarchy-launch-tui --app-id=org.omarchy.git git -C {quoted} show {hash}");
            if let Some(row) = emit(
                needle,
                &mut score,
                &format!("commit:{hash}"),
                subject,
                author,
                hash,
                &repos::age(now - ts),
                "Recent Commits",
                "Show Commit",
                &show,
                vec![json!({"title": "Copy Hash",
                            "exec": format!("printf %s {} | wl-copy", quote(hash))})],
                "",
            ) {
                rows.push(row);
            }
        }
    }
    NativeOutcome::Rows(rows)
}

impl NativeExt for Git {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        let arg = ctx.arg.clone();
        let roots = repos::roots_setting(&ctx.settings);
        Box::pin(async move {
            if !on_path("git") {
                return NativeOutcome::Fallback;
            }
            let Some((repo, leftover)) = repos::resolve(&arg, roots.as_deref()).await else {
                return NativeOutcome::Empty;
            };
            // `-e $repo/.git`, the script's own check after --resolve.
            if !repo.join(".git").exists() {
                return NativeOutcome::Empty;
            }
            let needle = leftover.to_lowercase();
            let repo_str = repo.to_string_lossy().into_owned();
            let quoted = shq(&repo_str);
            let name = repo
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let display = repos::display_path(&repo);
            let now = now_secs();

            let status = run::git_in(
                &repo_str,
                "status --porcelain=v2 --branch",
                Duration::from_secs(4),
            )
            .await
            .map(|f| repos::parse_porcelain(&f.stdout))
            .unwrap_or_default();
            // A repo with no branch — or a git that failed — is the
            // script's silent `exit 0`.
            if status.branch.is_empty() {
                return NativeOutcome::Empty;
            }
            let remote = run::git_in(&repo_str, "remote get-url origin", Duration::from_secs(3))
                .await
                .map(|f| f.stdout.trim().to_string())
                .unwrap_or_default();

            if needle.is_empty() {
                panel(&repo_str, &quoted, &name, &display, &status, &remote, now).await
            } else {
                search(&repo_str, &quoted, &name, &display, &status, &needle, now).await
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_with(staged: u64, changed: u64, conflicted: u64) -> repos::Status {
        repos::Status {
            staged,
            changed,
            conflicted,
            ..Default::default()
        }
    }

    #[test]
    fn keep_matches_what_the_row_says() {
        assert!(keep("", "anything", "", ""));
        assert!(keep("clean", "clean", "~/repo", "Status"));
        // The group is part of the haystack: "stat" lives in "status".
        assert!(keep("stat", "1 staged", "~/repo", "Status"));
        assert!(keep("zzz", "a", "b", "zzzrow"));
        assert!(!keep("nosuch", "clean", "~/repo", "Status"));
    }

    #[test]
    fn keep_lowercases_the_row_not_the_needle() {
        // The caller lowers the needle once; keep lowers the haystack.
        assert!(keep("main", "MAIN", "", ""));
        assert!(!keep("MAIN", "main", "", ""));
    }

    #[test]
    fn summary_counts_then_joins_on_a_bare_comma() {
        // bash's `IFS=", "` joins on the first byte — "1 staged,1 changed".
        assert_eq!(summary(&status_with(1, 1, 0)), "1 staged,1 changed");
        assert_eq!(summary(&status_with(2, 0, 0)), "2 staged");
        assert_eq!(
            summary(&status_with(3, 2, 1)),
            "3 staged,2 changed,1 conflicted"
        );
        assert_eq!(summary(&repos::Status::default()), "clean");
    }

    #[test]
    fn commit_lines_become_panel_objects() {
        let got = parse_commits("abc1234\u{1f}did the thing\u{1f}Devin\u{1f}1000\n", 1060);
        assert_eq!(
            got,
            vec![json!({"hash": "abc1234", "subject": "did the thing",
                        "author": "Devin", "age": 60})]
        );
    }

    #[test]
    fn commit_parse_skips_short_lines() {
        // jq's `select(length >= 4)`: a truncated line is dropped, not read.
        let got = parse_commits(
            "abc\u{1f}sub\u{1f}auth\u{1f}10\nshortline\na\u{1f}b\ndef\u{1f}x\u{1f}y\u{1f}20\n",
            100,
        );
        assert_eq!(got.len(), 2);
        assert_eq!(got[1]["hash"], "def");
    }

    #[test]
    fn commit_parse_poisoned_stamp_empties_the_list() {
        // jq's `tonumber` failure aborts the whole filter — the script then
        // emits `[]`, not a partial list.
        assert!(parse_commits("a\u{1f}b\u{1f}c\u{1f}notanumber\n", 100).is_empty());
    }

    #[test]
    fn emit_scores_only_emitted_rows() {
        let mut score = 90000;
        assert!(
            emit(
                "zzz",
                &mut score,
                "a",
                "t",
                "d",
                "s",
                "",
                "G",
                "p",
                "e",
                vec![],
                ""
            )
            .is_none()
        );
        // A filtered row costs nothing: the score did not move.
        assert_eq!(score, 90000);
        let first = emit(
            "",
            &mut score,
            "a",
            "t",
            "d",
            "s",
            "",
            "G",
            "p",
            "e",
            vec![],
            "",
        )
        .unwrap();
        let second = emit(
            "",
            &mut score,
            "b",
            "t",
            "d",
            "s",
            "",
            "G",
            "p",
            "e",
            vec![],
            "",
        )
        .unwrap();
        assert_eq!(first["score"], 90000);
        assert_eq!(second["score"], 89900);
    }

    #[test]
    fn emit_goto_turns_the_primary_into_a_query() {
        let mut score = 90000;
        let row = emit(
            "",
            &mut score,
            "branch",
            "main",
            "no upstream",
            "repo",
            "",
            "Branch",
            "Every Branch",
            "true",
            vec![],
            "branch:repo",
        )
        .unwrap();
        // goto blanks the row's exec and swaps exec for query on the action.
        assert_eq!(row["exec"], "");
        assert_eq!(
            row["actions"][0],
            json!({"title": "Every Branch", "shortcut": "↵", "query": "branch:repo"})
        );
    }

    #[test]
    fn emit_appends_extra_actions_after_the_primary() {
        let mut score = 90000;
        let row = emit(
            "",
            &mut score,
            "status",
            "clean",
            "~/r",
            "r",
            "",
            "Status",
            "Show Log",
            "git log",
            vec![json!({"title": "Full Status", "exec": "git status"})],
            "",
        )
        .unwrap();
        assert_eq!(row["actions"].as_array().unwrap().len(), 2);
        assert_eq!(
            row["actions"][0],
            json!({"title": "Show Log", "shortcut": "↵", "exec": "git log"})
        );
        assert_eq!(row["actions"][1]["title"], "Full Status");
    }
}
