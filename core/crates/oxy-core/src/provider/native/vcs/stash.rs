//! `stash:` — what is in a repo's stashes, without applying one to find
//! out. A port of `bin/oxy-git-stash`.
//!
//!   stash:               every local repo holding a stash, newest first
//!   stash:omarchy        that repo's stashes
//!   stash:omarchy login  only the ones whose message or files say login
//!
//! What the script proved, kept:
//!   * Bare `stash:` is always the picker, never "the repo you are in" —
//!     that repo usually holds nothing, and which one it is is a question
//!     a launcher spawned globally cannot answer. Naming a repo drills in.
//!   * `stash@{0}: WIP on main` is the same line every time and answers
//!     nothing, so each row carries the files the stash touches and how
//!     much of each — and `stash show` runs `--include-untracked`, because
//!     a stashed untracked file is exactly what plain `stash show` hides.
//!   * Apply before pop, both named for what they leave behind; drop is
//!     not offered — it destroys work nothing else has a copy of, and a
//!     launcher four seconds deep is the wrong place to be offered that.
//!   * One flat record per stash (`ref␟ts␟branch␟message␟file:a:d…`)
//!     is what a cache file can hold and a needle can filter without a
//!     parser — message, branch and file names all ride the same line.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use super::{repos, run};
use crate::provider::native::util::{on_path, shq};
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::support::quote::quote;

#[derive(Default)]
pub struct Stash;

/// `MAX` — more than this and the launcher is a scrollback; twelve is
/// already more stashes than anybody keeps on purpose.
const MAX: usize = 12;
/// The script's CACHE_FORMAT, riding in the cache path — bumping it
/// retires every entry written under an older reading rather than hoping
/// each key still means what it used to. A native has no `$0` to stamp;
/// this number is what plays it.
const CACHE_FORMAT: u32 = 2;
/// Every call below is a local read that answers in milliseconds or never:
/// the picker's probe, the list and each `stash show` share one deadline.
const GIT: Duration = Duration::from_secs(3);

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// The bare keyword: every repo holding something, newest stash first
// ------------------------------------------------------------------

/// A repo that answered `rev-parse refs/stash`. `sort_line` is the
/// `newest␟name␟display␟count` record the script piped through
/// `sort -rn`, kept so the ordering can be replicated rather than
/// approximated.
struct StashedRepo {
    newest: u64,
    name: String,
    display: String,
    count: usize,
    sort_line: String,
}

/// `sort -rn` over the record lines: the leading number decides, and
/// GNU's last resort for equal keys compares the whole line — reversed
/// like everything else.
fn sort_stashed(found: &mut [StashedRepo]) {
    found.sort_by(|a, b| {
        b.newest
            .cmp(&a.newest)
            .then_with(|| b.sort_line.cmp(&a.sort_line))
    });
}

/// `"{count} stash" / "{count} stashes" · newest {age}` — the count kept
/// as a word the way the jq program wrote it.
fn picker_subtitle(count: usize, newest: u64, now: u64) -> String {
    let noun = if count == 1 { "stash" } else { "stashes" };
    format!(
        "{count} {noun} · newest {}",
        repos::age((now as i64).saturating_sub(newest as i64))
    )
}

fn picker_row(repo: &StashedRepo, i: usize, now: u64) -> Value {
    json!({
        "id": format!("stash-repo-{}", repo.name),
        "view": "list",
        "group": "Stashed Work",
        "title": repo.name.as_str(),
        "subtitle": picker_subtitle(repo.count, repo.newest, now),
        "detail": repo.display.as_str(),
        "icon": "󰆓",
        // jq's `input_line_number` is 0-based: the first row scores 99000.
        "score": 99000 - i as i64,
        "actions": [{
            "title": "Show These Stashes",
            "query": format!("stash:{}", repo.name),
        }],
    })
}

/// `stash:` on its own. `oxy-repo --paths` hands over every known repo;
/// `rev-parse` on its gitdir (a `.git` *file* resolves its `gitdir:` line)
/// is the probe, `wc -l` on `stash list` the count, `log -1 %ct` the sort
/// key — all by gitdir so no checkout has to exist around it.
async fn picker_rows(now: u64) -> Vec<Value> {
    let mut found: Vec<StashedRepo> = Vec::new();
    for path in repos::load_repos() {
        let Some(dir) = repos::git_dir(&path) else {
            continue;
        };
        if !dir.is_dir() {
            continue;
        }
        let dir = dir.to_string_lossy().into_owned();
        let has_stash = run::git_at(&dir, "rev-parse --verify -q refs/stash", GIT)
            .await
            .is_some_and(|f| f.code == Some(0));
        if !has_stash {
            continue;
        }
        // `wc -l` counts newlines, not lines — git always ends the list
        // with one, but the byte count is what the script read.
        let count = run::git_at(&dir, "stash list", GIT)
            .await
            .map(|f| f.stdout.bytes().filter(|b| *b == b'\n').count())
            .unwrap_or(0);
        if count == 0 {
            continue;
        }
        // `|| newest=0` — a repo whose log read fails still lists, last.
        let newest = run::git_at(&dir, "log -1 --format=%ct refs/stash", GIT)
            .await
            .and_then(|f| f.stdout.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let display = repos::display_path(&path);
        let sort_line = format!(
            "{newest}{}{name}{}{display}{}{count}",
            repos::US,
            repos::US,
            repos::US
        );
        found.push(StashedRepo {
            newest,
            name,
            display,
            count,
            sort_line,
        });
    }
    sort_stashed(&mut found);
    found
        .iter()
        .enumerate()
        .map(|(i, repo)| picker_row(repo, i, now))
        .collect()
}

// One repo's stashes: the records and their rows
// ------------------------------------------------------------------

/// `%gs`'s two named shapes: `WIP on <branch>: <msg>` and
/// `On <branch>: <msg>` — anything else is a subject that names no branch
/// and `message` takes it whole. The glob wanted `": "` after the prefix
/// at all; the strips then ran one after the other (`${subject#WIP on }`,
/// then `${..#On }`), so "WIP on On x: m" names branch `x`, not `On x`.
/// The branch ends at the first `:`, the message starts after the first
/// `": "` — a branch name carrying a colon splits oddly, identically.
fn split_subject(subject: &str) -> (String, String) {
    let rest = subject
        .strip_prefix("WIP on ")
        .or_else(|| subject.strip_prefix("On "));
    let Some(rest) = rest else {
        return (String::new(), subject.to_string());
    };
    if !rest.contains(": ") {
        return (String::new(), subject.to_string());
    }
    let body = subject.strip_prefix("WIP on ").unwrap_or(subject);
    let body = body.strip_prefix("On ").unwrap_or(body);
    let message = body.split_once(": ").map(|(_, m)| m).unwrap_or("");
    let branch = body.split(':').next().unwrap_or("");
    (branch.to_string(), message.to_string())
}

/// One `path:added:deleted` field of a record. The path may itself carry
/// `:` so the last two fields own the numbers and everything before them
/// is the path — the jq program's `$p[0:-2] | join(":")`.
#[derive(Debug, PartialEq)]
struct FileStat {
    path: String,
    added: u64,
    deleted: u64,
}

fn parse_file(field: &str) -> FileStat {
    let parts: Vec<&str> = field.split(':').collect();
    let n = parts.len();
    // `$p[-2] // "0" | tonumber` — a field shorter than two parts has no
    // added number at all; a non-number reads as 0 rather than dying the
    // way jq's `tonumber` would.
    let num = |i: Option<usize>| {
        i.and_then(|i| parts.get(i))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    };
    FileStat {
        path: parts[..n.saturating_sub(2)].join(":"),
        added: num(n.checked_sub(2)),
        deleted: num(n.checked_sub(1)),
    }
}

/// A collected record: the flat `␟` line as it sits in the cache file —
/// ref, commit time, the branch it was taken on, the message, then the
/// files as `path:added:deleted` fields.
#[derive(Debug)]
struct Record {
    ref_: String,
    ts: u64,
    branch: String,
    message: String,
    files: Vec<FileStat>,
}

fn parse_record(line: &str) -> Record {
    let mut f = line.split(repos::US);
    Record {
        ref_: f.next().unwrap_or("").to_string(),
        ts: f.next().and_then(|s| s.parse().ok()).unwrap_or(0),
        branch: f.next().unwrap_or("").to_string(),
        message: f.next().unwrap_or("").to_string(),
        files: f.filter(|s| !s.is_empty()).map(parse_file).collect(),
    }
}

/// `[[ -n $line ]] || continue`, then `[[ -z $needle || ${line,,} ==
/// *"$needle"* ]]` — the needle searches the whole record line lowered:
/// ref, branch, message and every file name all carry it.
fn keeps(line: &str, needle: &str) -> bool {
    !line.is_empty() && (needle.is_empty() || line.to_lowercase().contains(needle))
}

/// The cold path: `[want]` then one record per stash, newest first. The
/// `--format` carries the `$'…\x1f…'` ANSI-C form verbatim through
/// `bash -c`, so bash turns the escapes into real unit-separator bytes
/// before git sees them — the same thing the script's quoting did. Each
/// `stash show --numstat` is its own fork; `-` counts (a binary file) are
/// written down as 0.
async fn collect(repo: &Path, want: &str) -> Vec<String> {
    let mut lines = vec![want.to_string()];
    let repo = repo.to_string_lossy();
    let Some(list) = run::git_in(&repo, "stash list --format=$'%gd\\x1f%ct\\x1f%gs'", GIT).await
    else {
        return lines;
    };
    for entry in list.stdout.lines().take(MAX) {
        let mut f = entry.splitn(3, repos::US);
        let Some(ref_) = f.next() else { continue };
        if ref_.is_empty() {
            continue;
        }
        let ts = f.next().unwrap_or("");
        let subject = f.next().unwrap_or("");
        let (branch, message) = split_subject(subject);
        let mut record = format!("{ref_}\u{1f}{ts}\u{1f}{branch}\u{1f}{message}");
        let show = format!("stash show --numstat --include-untracked {}", quote(ref_));
        if let Some(files) = run::git_in(&repo, &show, GIT).await {
            for line in files.stdout.lines() {
                let mut nf = line.splitn(3, '\t');
                let (Some(added), Some(deleted), Some(path)) = (nf.next(), nf.next(), nf.next())
                else {
                    continue;
                };
                if path.is_empty() {
                    continue;
                }
                let added = if added == "-" { "0" } else { added };
                let deleted = if deleted == "-" { "0" } else { deleted };
                record.push(repos::US);
                record.push_str(&format!("{path}:{added}:{deleted}"));
            }
        }
        lines.push(record);
    }
    lines
}

/// What every row of a named answer repeats: the repo's `%q` for the exec
/// strings, its base name for `branch:` and the `repo` block, and its
/// `~/`-contracted display path.
struct RepoWords {
    quoted: String,
    name: String,
    display: String,
}

/// One record as a row — the jq program's object verbatim: `age` stays a
/// number (the accessory is the string), the files list is what "did I
/// stash that" reads, and the first kept row carries the `repo` block.
fn stash_row(rec: &Record, i: usize, kept: usize, repo: &RepoWords, now: u64) -> Value {
    let sh = quote(&rec.ref_);
    let show = format!(
        "omarchy-launch-tui --app-id=org.omarchy.git git -C {} stash show -p --include-untracked {sh}",
        repo.quoted
    );
    let n = rec.files.len();
    let age = (now as i64).saturating_sub(rec.ts as i64);
    let title = if rec.message.is_empty() {
        rec.ref_.as_str()
    } else {
        rec.message.as_str()
    };
    let subtitle = if rec.branch.is_empty() {
        rec.ref_.clone()
    } else {
        format!("{}  on {}", rec.ref_, rec.branch)
    };
    let files: Vec<Value> = rec
        .files
        .iter()
        .map(|f| json!({ "path": f.path.as_str(), "added": f.added, "deleted": f.deleted }))
        .collect();
    let mut row = json!({
        "id": format!("stash:{}", rec.ref_),
        "view": "gitstashes",
        "score": 90000 - i as i64 * 100,
        "group": "Stashes",
        "title": title,
        "subtitle": subtitle,
        "detail": format!("{n} {}", if n == 1 { "file" } else { "files" }),
        "accessory": repos::age(age),
        "ref": rec.ref_.as_str(),
        "branch": rec.branch.as_str(),
        "message": rec.message.as_str(),
        "age": age,
        "files": files,
        "added": rec.files.iter().map(|f| f.added).sum::<u64>(),
        "deleted": rec.files.iter().map(|f| f.deleted).sum::<u64>(),
        "exec": show,
        "actions": [
            { "title": "Show Stash", "shortcut": "↵", "exec": show },
            // Apply before pop, and both named for what they leave behind.
            // A stash you applied is still there to apply again; a stash
            // you popped is not.
            {
                "title": "Apply, and Keep the Stash",
                "exec": format!(
                    "out=$(git -C {} stash apply {sh} 2>&1) \
                     && omarchy-notification-send {} \
                     || omarchy-notification-send -u normal \"git stash apply failed\" \"$out\"",
                    repo.quoted,
                    quote(&format!("Applied {}", rec.ref_)),
                ),
            },
            {
                "title": "Pop, and Remove It",
                "exec": format!(
                    "out=$(git -C {} stash pop {sh} 2>&1) \
                     && omarchy-notification-send {} \
                     || omarchy-notification-send -u normal \"git stash pop failed\" \"$out\"",
                    repo.quoted,
                    quote(&format!("Popped {}", rec.ref_)),
                ),
            },
            { "title": "Branches", "query": format!("branch:{}", repo.name) },
        ],
    });
    if i == 0 {
        row["repo"] = json!({
            "name": repo.name.as_str(),
            "path": repo.display.as_str(),
            "count": kept,
        });
    }
    row
}

impl NativeExt for Stash {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        // The script trimmed its `$1` itself; `arg_for` already
        // whitespace-normalizes, so this only guards a hand-built Ctx.
        let query = ctx.arg.trim().to_string();
        Box::pin(async move {
            if !on_path("git") {
                return NativeOutcome::Fallback;
            }
            let now = now_secs();

            // Bare `stash:` is always the picker — checked before resolve,
            // which would otherwise pin the answer to the current repo and
            // leave the picker unreachable whenever that repo holds one.
            if query.is_empty() {
                let rows = picker_rows(now).await;
                return if rows.is_empty() {
                    NativeOutcome::Empty
                } else {
                    NativeOutcome::Rows(rows)
                };
            }

            let Some((repo, leftover)) = repos::resolve(&query).await else {
                return NativeOutcome::Empty;
            };
            let needle = leftover.to_lowercase();
            let Some(gitdir) = repos::git_dir(&repo) else {
                return NativeOutcome::Empty;
            };

            // A stash cannot be pushed, popped or dropped without the
            // stash ref moving — the stamp is that ref's set, so a commit
            // does not throw this answer away.
            let want = repos::stashes_stamp(&gitdir).to_string();
            let cache_dir = repos::named_cache_dir("git-stashes", CACHE_FORMAT);
            let cache_file = cache_dir.join(repos::cache_key(&repo));
            let records = match repos::read_stamped(&cache_file, &want) {
                Some(records) => records,
                None => {
                    let lines = collect(&repo, &want).await;
                    // The script wrote the file whenever collect printed
                    // anything — even a bare stamp line, caching the empty
                    // answer too. Here the write waits for a record: a
                    // stamp-only file only buys a skipped `stash list`,
                    // and the answer either way is empty.
                    if lines.len() > 1 && repos::write_lines(&cache_file, &lines) {
                        repos::prune_named_cache("git-stashes", &cache_dir);
                    }
                    lines.into_iter().skip(1).collect()
                }
            };
            if records.is_empty() {
                return NativeOutcome::Empty;
            }

            let kept: Vec<&String> = records.iter().filter(|line| keeps(line, &needle)).collect();
            if kept.is_empty() {
                return NativeOutcome::Empty;
            }

            let words = RepoWords {
                quoted: shq(&repo.to_string_lossy()),
                name: repo
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                display: repos::display_path(&repo),
            };
            let count = kept.len();
            let rows = kept
                .iter()
                .enumerate()
                .map(|(i, line)| stash_row(&parse_record(line), i, count, &words, now))
                .collect();
            NativeOutcome::Rows(rows)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn subject_splits_the_wip_and_on_shapes() {
        assert_eq!(
            split_subject("WIP on main: 3fa1c7d init"),
            ("main".to_string(), "3fa1c7d init".to_string())
        );
        assert_eq!(
            split_subject("On feature: typed message"),
            ("feature".to_string(), "typed message".to_string())
        );
    }

    #[test]
    fn a_subject_without_the_shape_stays_whole() {
        assert_eq!(
            split_subject("rebased bits"),
            ("".to_string(), "rebased bits".to_string())
        );
        // "WIP on main" alone has no ": " — the glob never matched.
        assert_eq!(
            split_subject("WIP on main"),
            ("".to_string(), "WIP on main".to_string())
        );
    }

    #[test]
    fn the_prefix_strips_run_one_after_the_other() {
        // ${subject#WIP on } then ${..#On } — a branch named "On x" loses
        // both prefixes, the way the script's sequential strips worked.
        assert_eq!(
            split_subject("WIP on On x: m"),
            ("x".to_string(), "m".to_string())
        );
        // Branch ends at the first ':'; message starts at the first ": ".
        assert_eq!(
            split_subject("WIP on bra:nch: msg"),
            ("bra".to_string(), "msg".to_string())
        );
    }

    #[test]
    fn a_file_field_leaves_the_colons_in_the_path() {
        assert_eq!(
            parse_file("dir:with:colon/f.rs:4:2"),
            FileStat {
                path: "dir:with:colon/f.rs".into(),
                added: 4,
                deleted: 2,
            }
        );
        assert_eq!(
            parse_file("a.rs:1:0"),
            FileStat {
                path: "a.rs".into(),
                added: 1,
                deleted: 0,
            }
        );
        // Fewer than two ':' fields: no added number, no path.
        assert_eq!(
            parse_file("lone"),
            FileStat {
                path: "".into(),
                added: 0,
                deleted: 0,
            }
        );
    }

    #[test]
    fn a_record_line_splits_into_fields_and_files() {
        let r = parse_record(
            "stash@{0}\u{1f}1699\u{1f}main\u{1f}half a refactor\u{1f}app.py:1:1\u{1f}sketch.txt:1:0",
        );
        assert_eq!(r.ref_, "stash@{0}");
        assert_eq!(r.ts, 1699);
        assert_eq!(r.branch, "main");
        assert_eq!(r.message, "half a refactor");
        assert_eq!(r.files.len(), 2);
        assert_eq!(r.files[1].path, "sketch.txt");

        // Empty middle fields survive; empty file fields are the
        // `select(length > 0)` skips.
        let r = parse_record("stash@{1}\u{1f}7\u{1f}\u{1f}msg\u{1f}");
        assert_eq!(r.branch, "");
        assert!(r.files.is_empty());
    }

    #[test]
    fn the_needle_searches_the_whole_lowered_line() {
        let line = "stash@{0}\u{1f}1699\u{1f}main\u{1f}Half a Refactor\u{1f}sketch.txt:1:0";
        assert!(keeps(line, ""));
        assert!(keeps(line, "sketch")); // a file name
        assert!(keeps(line, "refactor")); // the message, lowered
        assert!(keeps(line, "main")); // the branch field
        assert!(keeps(line, "stash@{0}")); // the ref itself
        assert!(!keeps(line, "zzz"));
        assert!(!keeps("", ""));
    }

    #[test]
    fn picker_subtitle_counts_like_the_script() {
        assert_eq!(picker_subtitle(1, 0, 180), "1 stash · newest 3m");
        assert_eq!(picker_subtitle(2, 0, 180), "2 stashes · newest 3m");
        assert_eq!(picker_subtitle(2, 0, 7200), "2 stashes · newest 2h");
    }

    #[test]
    fn picker_sort_is_newest_then_the_whole_line_reversed() {
        let mut found = vec![
            StashedRepo {
                newest: 5,
                name: "a".into(),
                display: "d".into(),
                count: 1,
                sort_line: "5\u{1f}a\u{1f}d\u{1f}1".into(),
            },
            StashedRepo {
                newest: 9,
                name: "z".into(),
                display: "d".into(),
                count: 1,
                sort_line: "9\u{1f}z\u{1f}d\u{1f}1".into(),
            },
            StashedRepo {
                newest: 5,
                name: "b".into(),
                display: "d".into(),
                count: 1,
                sort_line: "5\u{1f}b\u{1f}d\u{1f}1".into(),
            },
        ];
        sort_stashed(&mut found);
        assert_eq!(found[0].name, "z"); // newest first
        assert_eq!(found[1].name, "b"); // a tie compares the line, reversed
        assert_eq!(found[2].name, "a");
    }

    #[test]
    fn picker_row_is_the_scripts_object() {
        let repo = StashedRepo {
            newest: 0,
            name: "omarchy".into(),
            display: "~/omarchy".into(),
            count: 2,
            sort_line: String::new(),
        };
        let row = picker_row(&repo, 0, 180);
        assert_eq!(row["id"], json!("stash-repo-omarchy"));
        assert_eq!(row["view"], json!("list"));
        assert_eq!(row["group"], json!("Stashed Work"));
        assert_eq!(row["title"], json!("omarchy"));
        assert_eq!(row["subtitle"], json!("2 stashes · newest 3m"));
        assert_eq!(row["detail"], json!("~/omarchy"));
        assert_eq!(row["icon"], json!("󰆓"));
        assert_eq!(row["score"], json!(99000));
        assert_eq!(picker_row(&repo, 3, 0)["score"], json!(98997));
        assert_eq!(
            row["actions"][0],
            json!({"title": "Show These Stashes", "query": "stash:omarchy"})
        );
    }

    #[test]
    fn stash_row_carries_the_files_and_the_actions() {
        let words = RepoWords {
            quoted: "/repos/oxy".into(),
            name: "oxy".into(),
            display: "~/oxy".into(),
        };
        let rec = parse_record(
            "stash@{0}\u{1f}1699\u{1f}main\u{1f}half a refactor\u{1f}app.py:3:1\u{1f}sketch.txt:1:0",
        );
        let row = stash_row(&rec, 0, 1, &words, 1699 + 3600);
        assert_eq!(row["id"], json!("stash:stash@{0}"));
        assert_eq!(row["view"], json!("gitstashes"));
        assert_eq!(row["score"], json!(90000));
        assert_eq!(row["group"], json!("Stashes"));
        assert_eq!(row["title"], json!("half a refactor"));
        assert_eq!(row["subtitle"], json!("stash@{0}  on main"));
        assert_eq!(row["detail"], json!("2 files"));
        assert_eq!(row["accessory"], json!("1h"));
        assert_eq!(row["ref"], json!("stash@{0}"));
        assert_eq!(row["branch"], json!("main"));
        assert_eq!(row["age"], json!(3600));
        assert_eq!(
            row["files"],
            json!([
                { "path": "app.py", "added": 3, "deleted": 1 },
                { "path": "sketch.txt", "added": 1, "deleted": 0 },
            ])
        );
        assert_eq!(row["added"], json!(4));
        assert_eq!(row["deleted"], json!(1));

        let show = "omarchy-launch-tui --app-id=org.omarchy.git git -C /repos/oxy \
                    stash show -p --include-untracked 'stash@{0}'";
        assert_eq!(row["exec"], json!(show));
        let titles: Vec<&str> = row["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["title"].as_str().unwrap())
            .collect();
        assert_eq!(
            titles,
            [
                "Show Stash",
                "Apply, and Keep the Stash",
                "Pop, and Remove It",
                "Branches"
            ]
        );
        assert_eq!(row["actions"][0]["shortcut"], json!("↵"));
        assert_eq!(row["actions"][0]["exec"], json!(show));
        assert_eq!(
            row["actions"][1]["exec"],
            json!(
                "out=$(git -C /repos/oxy stash apply 'stash@{0}' 2>&1) \
                 && omarchy-notification-send 'Applied stash@{0}' \
                 || omarchy-notification-send -u normal \"git stash apply failed\" \"$out\""
            )
        );
        assert_eq!(
            row["actions"][2]["exec"],
            json!(
                "out=$(git -C /repos/oxy stash pop 'stash@{0}' 2>&1) \
                 && omarchy-notification-send 'Popped stash@{0}' \
                 || omarchy-notification-send -u normal \"git stash pop failed\" \"$out\""
            )
        );
        assert_eq!(row["actions"][3]["query"], json!("branch:oxy"));
        // The first kept row alone carries the repo block.
        assert_eq!(
            row["repo"],
            json!({"name": "oxy", "path": "~/oxy", "count": 1})
        );
    }

    #[test]
    fn later_rows_have_no_repo_block_and_lose_score() {
        let words = RepoWords {
            quoted: "/r".into(),
            name: "r".into(),
            display: "/r".into(),
        };
        let rec = parse_record("stash@{2}\u{1f}9\u{1f}\u{1f}");
        let row = stash_row(&rec, 2, 3, &words, 100);
        assert_eq!(row["score"], json!(89800));
        assert!(row.get("repo").is_none());
        // Empty message → the ref titles; empty branch → no " on …".
        assert_eq!(row["title"], json!("stash@{2}"));
        assert_eq!(row["subtitle"], json!("stash@{2}"));
        assert_eq!(row["detail"], json!("0 files"));
        assert_eq!(row["accessory"], json!("1m")); // 100 - 9 = 91s
        assert_eq!(row["age"], json!(91));
    }
}
