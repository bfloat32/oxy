//! `branch:` — every local branch of one repo, drawn rather than listed:
//! the checkout marker, the drift against its upstream and against the
//! trunk, and when it last moved. A port of `bin/oxy-git-branch`.
//!
//! What the script proved, kept:
//!   * Enter is a plain `git switch`, never a force — git's own refusal to
//!     overwrite uncommitted work is the confirmation, reported rather than
//!     swallowed, and a dirty tree grows a named "Stash and Switch" instead
//!     of stashing quietly.
//!   * The answer is three git calls cached against `branches_stamp`, so a
//!     keystroke that only changed the filter never forks git. The header's
//!     dirty counts are the exception — they read the worktree, which the
//!     stamp cannot see, so a header older than DIRTY_RECHECK pays one
//!     `git status` before the rows are served and rewrites the file.
//!   * Records ride on the unit separator, not a tab — a commit subject or
//!     an author name is data, and a tab inside either must not split the
//!     line it rides in. The header is the one line that is tab-separated.
//!   * `%(contents:subject)` keeps a paragraph break's newline, so the fold
//!     below is load-bearing: a fragment holding no separator is the rest
//!     of the record above it, glued back with a space.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use super::{repos, run};
use crate::provider::native::util::{on_path, shq};
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::support::quote::quote;

#[derive(Default)]
pub struct Branch;

/// The byte a record's fields ride in on — the scripts' SEP: unit
/// separator, never whitespace-collapsed.
const US: char = '\u{1f}';
/// `DIRTY_RECHECK` — a cached header's dirty counts this old are re-derived
/// with one `git status` before the answer is served.
const DIRTY_RECHECK_SECS: u64 = 3;
/// `CACHE_FORMAT` — goes in the cache path, so bumping it retires every
/// entry written under the old scheme. It also stands in for the script's
/// own `$0` in the stamp list: changing the reading throws the old answer
/// away.
const CACHE_FORMAT: u32 = 2;
/// The family's deadline for one `git` call — the manifest gives the whole
/// query 5000ms and the calls share it.
const GIT: Duration = Duration::from_secs(4);

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One `git status --porcelain=v2 --branch`, counted the way the script's
/// `count_state` counted it — same arg list, no `--untracked-files`.
async fn count_state(repo: &str) -> repos::Status {
    match run::git_in(repo, "status --porcelain=v2 --branch", GIT).await {
        Some(f) => repos::parse_porcelain(&f.stdout),
        None => repos::Status::default(),
    }
}

/// The trunk "behind" is measured against, chosen from the branches that
/// are actually there — naming a ref that does not exist makes for-each-ref
/// exit fatal rather than print a blank column.
fn trunk_of(names: &[String]) -> String {
    for wanted in ["main", "master"] {
        if names.iter().any(|n| n == wanted) {
            return wanted.to_string();
        }
    }
    names
        .iter()
        .find(|n| n.as_str() == "trunk" || n.as_str() == "develop")
        .cloned()
        .unwrap_or_default()
}

/// The awk fold after for-each-ref: `%(contents:subject)` keeps a
/// multi-line subject's newlines and the subject is the last field, so a
/// fragment containing no unit separator is the rest of the record above
/// it — glued back with a space, one line per branch again.
fn fold_records(stdout: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut held = String::new();
    for line in stdout.lines() {
        if !line.contains(US) && !held.is_empty() {
            held.push(' ');
            held.push_str(line);
            continue;
        }
        if !held.is_empty() {
            out.push(std::mem::take(&mut held));
        }
        held = line.to_string();
    }
    if !held.is_empty() {
        out.push(held);
    }
    out
}

/// The header is the one TAB-separated line: `head staged changed
/// conflicted trunk header_ts`, and `read` leaves the remainder in the
/// last name, so six fields and no more.
fn header_fields(line: &str) -> [&str; 6] {
    let mut out = [""; 6];
    for (i, part) in line.splitn(6, '\t').enumerate() {
        out[i] = part;
    }
    out
}

/// `[[ ! $header_ts =~ ^[0-9]+$ ]] || ((now - header_ts > DIRTY_RECHECK))` —
/// an unparseable stamp is stale, and a stamp in the future is not.
fn header_stale(header_ts: &str, now: u64) -> bool {
    if header_ts.is_empty() || !header_ts.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    now.saturating_sub(header_ts.parse().unwrap_or(0)) > DIRTY_RECHECK_SECS
}

/// A record line → its eight fields: `marker ref upstream track ab ts
/// author subject`. `read` with IFS=$'\x1f' leaves the remainder in the
/// last name, so a subject keeps whatever separators it owns.
fn record_fields(line: &str) -> [&str; 8] {
    let mut out = [""; 8];
    for (i, part) in line.splitn(8, US).enumerate() {
        out[i] = part;
    }
    out
}

/// `*"$needle"*` over the lowered `"ref subject"` — the leftover after the
/// repo name, the script's `hay` check.
fn wanted(ref_: &str, subject: &str, needle: &str) -> bool {
    needle.is_empty() || format!("{ref_} {subject}").to_lowercase().contains(needle)
}

/// `%(ahead-behind:)` prints `2 1` — anything else (the empty column a
/// trunkless repo writes, or a hand-edited cache line) is no drift, the
/// jq filter's `[0, 0]`.
fn split_ab(ab: &str) -> (u64, u64) {
    let mut parts = ab.split(' ');
    let (Some(a), Some(b), None) = (parts.next(), parts.next(), parts.next()) else {
        return (0, 0);
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    if digits(a) && digits(b) {
        (a.parse().unwrap_or(0), b.parse().unwrap_or(0))
    } else {
        (0, 0)
    }
}

/// The slow path: three git calls, then the lines the cache file holds —
/// the stamp first, the header second, one folded record per branch after,
/// freshest committerdate first.
async fn collect(repo: &str, want: &str) -> Vec<String> {
    let names: Vec<String> = run::git_in(
        repo,
        "for-each-ref --format='%(refname:short)' refs/heads",
        GIT,
    )
    .await
    .map(|f| f.stdout.lines().map(str::to_string).collect())
    .unwrap_or_default();

    let trunk = trunk_of(&names);
    let st = count_state(repo).await;

    let mut lines = vec![
        want.to_string(),
        // The trailing field is the header's own age — the dirty counts
        // ride on it, for the worktree recheck on a stale hit.
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            st.branch,
            st.staged,
            st.changed,
            st.conflicted,
            trunk,
            now_secs()
        ),
    ];

    // Real unit-separator bytes inside the single quotes — git does not
    // read backslash escapes in --format, so the byte itself goes in.
    let mut fmt = format!(
        "%(HEAD){US}%(refname:short){US}%(upstream:short){US}%(upstream:track,nobracket){US}"
    );
    if !trunk.is_empty() {
        fmt.push_str(&format!("%(ahead-behind:refs/heads/{trunk})"));
    }
    fmt.push_str(&format!(
        "{US}%(committerdate:unix){US}%(authorname){US}%(contents:subject)"
    ));

    if let Some(f) = run::git_in(
        repo,
        &format!("for-each-ref --sort=-committerdate --format='{fmt}' refs/heads"),
        GIT,
    )
    .await
    {
        lines.extend(fold_records(&f.stdout));
    }
    lines
}

/// What every row shares: the repo's quoting and display names, the header
/// the view draws across the top, and the `now` the ages count back from.
struct RepoCtx {
    quoted: String,
    name: String,
    display: String,
    head: String,
    trunk: String,
    staged: u64,
    changed: u64,
    conflicted: u64,
    dirty: u64,
    now: u64,
}

/// One kept record → the script's jq object, field for field. `i` is the
/// index among the kept rows — the current branch was already moved to the
/// front, so the score falls away from the row somebody is most likely on.
fn branch_row(i: usize, count: usize, f: &[&str; 8], cx: &RepoCtx) -> Value {
    let current = f[0] == "*";
    let ref_ = f[1];
    let upstream = f[2];
    let (ahead, behind, gone) = repos::parse_track(f[3]);
    let (trunk_ahead, trunk_behind) = split_ab(f[4]);
    let ts: i64 = f[5].parse().unwrap_or(0);
    let author = f[6];
    let subject = f[7];

    let quoted = &cx.quoted;
    let sh = quote(ref_);
    let diff = format!("omarchy-launch-tui --app-id=org.omarchy.git git -C {quoted} diff");
    let switch = format!(
        "out=$(git -C {quoted} switch {sh} 2>&1) && omarchy-notification-send {} \
         || omarchy-notification-send -u normal \"git switch failed\" \"$out\"",
        quote(&format!("Now on {ref_}"))
    );

    let mut actions = vec![if current {
        json!({ "title": "Show Diff", "shortcut": "↵", "exec": diff })
    } else {
        json!({ "title": "Switch", "shortcut": "↵", "exec": switch })
    }];
    // Named, never implied: a switch that quietly stashed for you is a
    // switch that lost your work somewhere you did not look.
    if !current && cx.dirty > 0 {
        actions.push(json!({
            "title": "Stash and Switch",
            "exec": format!(
                "out=$(git -C {quoted} stash push -u -m {} 2>&1 && git -C {quoted} switch {sh} 2>&1) \
                 && omarchy-notification-send {} \
                 || omarchy-notification-send -u normal \"stash and switch failed\" \"$out\"",
                quote(&format!("before switching to {ref_}")),
                quote(&format!("Stashed, now on {ref_}"))
            ),
        }));
    }
    // A graph belongs in a window that stays open, not in a launcher that
    // is 600 pixels wide and lasts four seconds.
    actions.push(json!({
        "title": "Log This Branch",
        "exec": format!("omarchy-launch-tui --app-id=org.omarchy.git git -C {quoted} log --oneline --graph --decorate -60 {sh}"),
    }));
    actions.push(json!({
        "title": "Copy Branch Name",
        "exec": format!("printf %s {sh} | wl-copy"),
    }));
    actions.push(json!({
        "title": "Stashes",
        "query": format!("stash:{}", cx.name),
    }));

    let mut row = json!({
        "id": format!("branch:{ref_}"),
        "view": "gitbranches",
        "score": 90000 - i as i64 * 100,
        "group": "Branches",
        "title": ref_,
        "subtitle": subject,
        "detail": if gone {
            "upstream gone".to_string()
        } else if upstream.is_empty() {
            "no upstream".to_string()
        } else {
            upstream.to_string()
        },
        "accessory": repos::age(cx.now as i64 - ts),
        "name": ref_,
        "current": current,
        "upstream": upstream,
        "gone": gone,
        "ahead": ahead,
        "behind": behind,
        "trunk": cx.trunk,
        "trunkAhead": trunk_ahead,
        "trunkBehind": trunk_behind,
        "age": cx.now as i64 - ts,
        "author": author,
        "subject": subject,
        "exec": if current { diff } else { switch },
        "actions": actions,
    });
    // The header the view draws across the top rides on the first row:
    // which repo, and whether switching is going to be refused.
    if i == 0 {
        row["repo"] = json!({
            "name": cx.name,
            "path": cx.display,
            "branch": cx.head,
            "trunk": cx.trunk,
            "staged": cx.staged,
            "changed": cx.changed,
            "conflicted": cx.conflicted,
            "count": count,
        });
    }
    row
}

impl NativeExt for Branch {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        let arg = ctx.arg.clone();
        Box::pin(async move {
            // The manifest's `when`, re-checked — a worker asks a native
            // even when it fails, and here that is a decline, not a list.
            if !on_path("git") {
                return NativeOutcome::Fallback;
            }
            // `oxy-repo --resolve`: which repo this query is about, and the
            // leftover as the filter — the same read `git:` and `stash:` make.
            let Some((repo, leftover)) = repos::resolve(&arg).await else {
                return NativeOutcome::Empty;
            };
            if !repo.join(".git").exists() {
                return NativeOutcome::Empty;
            }
            let Some(gitdir) = repos::git_dir(&repo) else {
                return NativeOutcome::Empty;
            };
            let needle = leftover.to_lowercase();
            let now = now_secs();
            let repo_str = repo.to_string_lossy().into_owned();

            // A branch cannot move, appear or be checked out without one of
            // the stamped files changing, so the whole answer is cached
            // against the newest of them.
            let want = repos::branches_stamp(&gitdir).to_string();
            let cache_dir = repos::named_cache_dir("git-branches", CACHE_FORMAT);
            let cache_file = cache_dir.join(repos::cache_key(&repo));

            let mut lines = match repos::read_stamped(&cache_file, &want) {
                Some(rest) => {
                    let mut v = Vec::with_capacity(rest.len() + 1);
                    v.push(want.clone());
                    v.extend(rest);
                    v
                }
                None => {
                    let collected = collect(&repo_str, &want).await;
                    if collected.len() > 1 && repos::write_lines(&cache_file, &collected) {
                        repos::prune_named_cache("git-branches", &cache_dir);
                    }
                    collected
                }
            };
            if lines.len() <= 1 {
                return NativeOutcome::Empty;
            }

            let h = header_fields(&lines[1]);
            let mut head = h[0].to_string();
            let mut staged: u64 = h[1].parse().unwrap_or(0);
            let mut changed: u64 = h[2].parse().unwrap_or(0);
            let mut conflicted: u64 = h[3].parse().unwrap_or(0);
            let trunk = h[4].to_string();

            // The header's dirty counts read the worktree, which the stamp
            // cannot see: a hit older than DIRTY_RECHECK re-derives them
            // now — one `git status`, one repo — and rewrites the cached
            // header so the next keystroke within the window is free again.
            if header_stale(h[5], now) {
                let st = count_state(&repo_str).await;
                // A status call that says nothing does not erase the branch
                // the cache already named; only the counts are re-derived.
                if !st.branch.is_empty() {
                    head = st.branch;
                }
                staged = st.staged;
                changed = st.changed;
                conflicted = st.conflicted;
                lines[1] = format!(
                    "{head}\t{staged}\t{changed}\t{conflicted}\t{trunk}\t{}",
                    now_secs()
                );
                let _ = repos::write_lines(&cache_file, &lines);
            }
            let dirty = staged + changed + conflicted;

            // Where you are now first, then whatever moved most recently —
            // the order for-each-ref's sort already produced. A list that
            // buries the checkout marker eight rows down has made you
            // search for the one fact you already knew.
            let mut kept: Vec<[&str; 8]> = Vec::new();
            for line in &lines[2..] {
                if line.is_empty() {
                    continue;
                }
                let f = record_fields(line);
                if f[1].is_empty() || !wanted(f[1], f[7], &needle) {
                    continue;
                }
                if f[0] == "*" {
                    kept.insert(0, f);
                } else {
                    kept.push(f);
                }
            }
            if kept.is_empty() {
                return NativeOutcome::Empty;
            }

            let cx = RepoCtx {
                quoted: shq(&repo_str),
                name: repo
                    .file_name()
                    .map(|f| f.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                display: repos::display_path(&repo),
                head,
                trunk,
                staged,
                changed,
                conflicted,
                dirty,
                now,
            };
            let count = kept.len();
            NativeOutcome::Rows(
                kept.iter()
                    .enumerate()
                    .map(|(i, f)| branch_row(i, count, f, &cx))
                    .collect(),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cx() -> RepoCtx {
        RepoCtx {
            quoted: shq("/home/u/oxy-fixture"),
            name: "oxy-fixture".into(),
            display: "~/oxy-fixture".into(),
            head: "main".into(),
            trunk: "main".into(),
            staged: 1,
            changed: 1,
            conflicted: 0,
            dirty: 2,
            now: 1_700_000_000,
        }
    }

    #[test]
    fn fold_glues_continuation_lines_back() {
        // A paragraph break in `contents:subject` lands as a line with no
        // unit separator — the rest of the record above it, with a space.
        let out = " *\u{1f}main\u{1f}up\u{1f}\u{1f}2 1\u{1f}9\u{1f}t\u{1f}first\nrest of it\n\u{1f}dev\u{1f}\u{1f}\u{1f}\u{1f}8\u{1f}t\u{1f}s";
        let lines = fold_records(out);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with("first rest of it"));
        assert!(lines[1].contains("dev"));
    }

    #[test]
    fn fold_keeps_a_leading_separatorless_fragment() {
        // awk's `held != ""` gate: a first line with no separator is held,
        // not glued — it flushes as its own line when a real record lands.
        let lines = fold_records("junk\n\u{1f}a\u{1f}\u{1f}\u{1f}\u{1f}1\u{1f}t\u{1f}s");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "junk");
        // And its `ref` field is empty, so the row loop skips it.
        assert!(record_fields(&lines[0])[1].is_empty());
    }

    #[test]
    fn trunk_is_chosen_from_branches_that_exist() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(trunk_of(&names(&["dev", "main", "x"])), "main");
        assert_eq!(trunk_of(&names(&["dev", "master"])), "master");
        // The last tier is the *first* name that is trunk or develop.
        assert_eq!(trunk_of(&names(&["develop", "trunk"])), "develop");
        assert_eq!(trunk_of(&names(&["trunk", "develop"])), "trunk");
        assert_eq!(trunk_of(&names(&["dev"])), "");
    }

    #[test]
    fn the_header_is_six_tab_fields_and_ages_out() {
        let h = header_fields("main\t1\t2\t0\tmain\t1700000000");
        assert_eq!(h, ["main", "1", "2", "0", "main", "1700000000"]);
        assert!(!header_stale(h[5], 1_700_000_002));
        assert!(header_stale(h[5], 1_700_000_004));
        // An absent or unparseable stamp is stale; a future one is not.
        assert!(header_stale("", 1_700_000_000));
        assert!(header_stale("abc", 1_700_000_000));
        assert!(!header_stale("9999999999", 1_700_000_000));
        // `read` leaves the remainder in the last name — a seventh field
        // rides inside header_ts and fails the numeric check, stale.
        let h = header_fields("main\t1\t2\t0\tmain\t9\textra");
        assert!(header_stale(h[5], 1_700_000_000));
    }

    #[test]
    fn the_needle_matches_ref_or_subject() {
        assert!(wanted("login", "fix the flow", "login"));
        assert!(wanted("login", "Fix the flow", "fix"));
        assert!(!wanted("login", "fix", "logout"));
        assert!(wanted("anything", "", ""));
    }

    #[test]
    fn ahead_behind_is_two_numbers_or_nothing() {
        assert_eq!(split_ab("2 1"), (2, 1));
        assert_eq!(split_ab("0 0"), (0, 0));
        // jq's `^[0-9]+ [0-9]+$`: one space, digits both sides.
        assert_eq!(split_ab(""), (0, 0));
        assert_eq!(split_ab("2"), (0, 0));
        assert_eq!(split_ab("2  1"), (0, 0));
        assert_eq!(split_ab("2 1 "), (0, 0));
        assert_eq!(split_ab("ahead 2"), (0, 0));
    }

    #[test]
    fn a_record_splits_on_us_and_only_us() {
        let f =
            record_fields("*\u{1f}main\u{1f}origin/main\u{1f}gone\u{1f}2 1\u{1f}9\u{1f}t\u{1f}sub");
        assert_eq!(f[0], "*");
        assert_eq!(f[1], "main");
        assert_eq!(f[2], "origin/main");
        assert_eq!(f[3], "gone");
        assert_eq!(f[4], "2 1");
        assert_eq!(f[5], "9");
        assert_eq!(f[6], "t");
        assert_eq!(f[7], "sub");
        // The last name takes the remainder — a separator in the subject
        // stays in the subject, and a short line reads "" for the rest.
        let f = record_fields("\u{1f}r\u{1f}\u{1f}\u{1f}\u{1f}1\u{1f}a\u{1f}x\u{1f}y");
        assert_eq!(f[7], "x\u{1f}y");
        let f = record_fields("\u{1f}only");
        assert_eq!(f[7], "");
    }

    #[test]
    fn the_row_is_the_scripts_object() {
        let f = record_fields(
            "\u{1f}login\u{1f}origin/login\u{1f}ahead 2, behind 1\u{1f}2 1\u{1f}1699999900\u{1f}t\u{1f}work",
        );
        let row = branch_row(1, 3, &f, &cx());
        assert_eq!(row["id"], json!("branch:login"));
        assert_eq!(row["view"], json!("gitbranches"));
        assert_eq!(row["score"], json!(89900));
        assert_eq!(row["group"], json!("Branches"));
        assert_eq!(row["title"], json!("login"));
        assert_eq!(row["subtitle"], json!("work"));
        assert_eq!(row["detail"], json!("origin/login"));
        assert_eq!(row["name"], json!("login"));
        assert_eq!(row["current"], json!(false));
        assert_eq!(row["gone"], json!(false));
        assert_eq!(row["ahead"], json!(2));
        assert_eq!(row["behind"], json!(1));
        assert_eq!(row["trunk"], json!("main"));
        assert_eq!(row["trunkAhead"], json!(2));
        assert_eq!(row["trunkBehind"], json!(1));
        assert_eq!(row["age"], json!(100));
        assert_eq!(row["accessory"], json!("1m"));
        assert_eq!(row["author"], json!("t"));
        assert_eq!(row["subject"], json!("work"));
        // Not the current row: Enter switches, and the refusal is reported.
        let exec = row["exec"].as_str().unwrap();
        assert!(exec.starts_with("out=$(git -C /home/u/oxy-fixture switch 'login' 2>&1)"));
        assert!(exec.contains("\"git switch failed\" \"$out\""));
        // No `repo` header except on the first row.
        assert!(row.get("repo").is_none());
    }

    #[test]
    fn actions_carry_the_switch_contract() {
        let f = record_fields("\u{1f}login\u{1f}\u{1f}\u{1f}\u{1f}9\u{1f}t\u{1f}s");
        let row = branch_row(1, 3, &f, &cx());
        let actions = row["actions"].as_array().unwrap();
        assert_eq!(actions[0]["title"], json!("Switch"));
        assert_eq!(actions[0]["shortcut"], json!("↵"));
        // Dirty tree + not current: the named stash-and-switch is offered.
        assert_eq!(actions[1]["title"], json!("Stash and Switch"));
        assert!(
            actions[1]["exec"]
                .as_str()
                .unwrap()
                .contains("stash push -u -m 'before switching to login'")
        );
        assert_eq!(actions[2]["title"], json!("Log This Branch"));
        assert_eq!(actions[3]["title"], json!("Copy Branch Name"));
        assert_eq!(actions[4]["title"], json!("Stashes"));
        assert_eq!(actions[4]["query"], json!("stash:oxy-fixture"));

        // A clean tree drops the offer rather than hiding it in a dialog.
        let mut clean = cx();
        clean.dirty = 0;
        let row = branch_row(1, 3, &f, &clean);
        let titles: Vec<&str> = row["actions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|a| a["title"].as_str())
            .collect();
        assert!(!titles.contains(&"Stash and Switch"));
    }

    #[test]
    fn the_current_row_diffs_and_carries_the_header() {
        let f = record_fields("*\u{1f}main\u{1f}\u{1f}gone\u{1f}0 0\u{1f}9\u{1f}t\u{1f}init");
        let row = branch_row(0, 2, &f, &cx());
        assert_eq!(row["current"], json!(true));
        assert_eq!(row["gone"], json!(true));
        assert_eq!(row["detail"], json!("upstream gone"));
        assert_eq!(
            row["exec"],
            json!("omarchy-launch-tui --app-id=org.omarchy.git git -C /home/u/oxy-fixture diff")
        );
        let actions = row["actions"].as_array().unwrap();
        assert_eq!(actions[0]["title"], json!("Show Diff"));
        // No stash offer on the branch you are already on.
        assert!(
            !actions
                .iter()
                .any(|a| a["title"].as_str() == Some("Stash and Switch"))
        );
        // The first kept row carries the repo header the view draws.
        assert_eq!(
            row["repo"],
            json!({
                "name": "oxy-fixture",
                "path": "~/oxy-fixture",
                "branch": "main",
                "trunk": "main",
                "staged": 1,
                "changed": 1,
                "conflicted": 0,
                "count": 2,
            })
        );
        // An upstream-less branch says so rather than inventing one.
        let f = record_fields("\u{1f}solo\u{1f}\u{1f}\u{1f}\u{1f}9\u{1f}t\u{1f}s");
        assert_eq!(branch_row(1, 2, &f, &cx())["detail"], json!("no upstream"));
    }
}
