//! `repo` — ported from `bin/oxy-repo`'s listing mode: every discovered
//! repository, ranked by name against the query and by which one's reflog
//! moved last, each drawn with the state line the shared `repos` layer
//! caches.
//!
//! The shape is the script's, kept because the costs it was shaped around
//! are real: ranking is string work over the cached repo list, and git is
//! paid for only by the dozen rows that survive — a keystroke never walks
//! the filesystem and never forks `git status` per candidate. The row JSON
//! is byte-identical to what the script's jq emitted, so the `repos` view
//! cannot tell which leg answered.

use std::cmp::Reverse;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use super::repos;
use crate::provider::native::util::{on_path, shq};
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::support::quote::quote;

/// `repo:` answers for `repo`.
#[derive(Default)]
pub struct Repo;

/// `date +%s`.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `stat -c %Y "$gitdir/logs/HEAD"` — when the reflog last moved, the
/// listing's tie-break for "the repo I am working in". A repo with no
/// reflog yet is a 0, the script's `|| touched=0`.
fn reflog_touched(gitdir: &Path) -> u64 {
    std::fs::metadata(gitdir.join("logs/HEAD"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The listing's score: no needle lists everything at the base score; a
/// needle prefers a directory-name prefix, then a hit inside the name,
/// then a hit anywhere on the path — all of it lowercase, the script's
/// `,,` on both sides. `None` is the script's `continue`: a needle that
/// matches nothing drops the repo.
fn rank_score(needle: &str, name: &str, path: &str) -> Option<u64> {
    let needle = needle.to_lowercase();
    if needle.is_empty() {
        return Some(1000);
    }
    let name = name.to_lowercase();
    let path = path.to_lowercase();
    if name.starts_with(&*needle) {
        Some(40000)
    } else if name.contains(&*needle) {
        Some(25000)
    } else if path.contains(&*needle) {
        Some(10000)
    } else {
        None
    }
}

/// The `branch:` filter: `[[ -n $branch_filter ]] && head_branch "$gitdir"
/// && [[ ${head,,} == *"${branch_filter,,}"* ]]`, with the read failure
/// folded into the `Option` — a repo whose HEAD cannot be read is the
/// script's `|| continue`. No filter passes everything.
fn branch_matches(head: Option<&str>, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    head.is_some_and(|h| h.to_lowercase().contains(&filter.to_lowercase()))
}

/// The `state` string: branch, then the dirty dot, then the drift arrows —
/// each part only while it is true.
fn state_line_str(branch: &str, dirty: u64, ahead: u64, behind: u64) -> String {
    let mut s = branch.to_string();
    if dirty > 0 {
        s.push_str(" ●");
    }
    if ahead > 0 {
        s.push_str(&format!(" ↑{ahead}"));
    }
    if behind > 0 {
        s.push_str(&format!(" ↓{behind}"));
    }
    s
}

/// `open_command` — what "open this repo" means, in order of who said so
/// most deliberately: `repoOpen` (its `{}` takes the quoted path wherever
/// it sits; a command without one gets the path appended, which is what
/// every editor expects anyway), then `VISUAL` under `setsid` — verbatim,
/// it carries its own flags — then the launcher's own editor.
fn open_command(repo_open: &str, visual: Option<&str>, path: &str) -> String {
    let quoted = shq(path);
    if !repo_open.is_empty() {
        if repo_open.contains("{}") {
            return repo_open.replace("{}", &quoted);
        }
        return format!("{repo_open} {quoted}");
    }
    match visual {
        Some(v) if !v.is_empty() => format!("setsid {v} {quoted}"),
        _ => format!("omarchy-launch-editor {quoted}"),
    }
}

/// A repo that survived ranking: its score, when its reflog last moved,
/// and the paths the row loop needs.
struct Ranked {
    score: u64,
    touched: u64,
    repo: PathBuf,
    gitdir: PathBuf,
}

/// The script's ranking pass — `load_repos`, then per repo: still there,
/// the name/path score, `git_dir`, the `branch:` filter against
/// `.git/HEAD`, and the reflog's mtime. All string and metadata work;
/// nothing in here forks.
fn rank(query: &str, branch_filter: &str, roots: Option<&str>) -> Vec<Ranked> {
    let mut ranked = Vec::new();
    for repo in repos::load_repos(roots) {
        if !repo.is_dir() {
            continue;
        }
        let name = repo
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(score) = rank_score(query, &name, &repo.to_string_lossy()) else {
            continue;
        };
        let Some(gitdir) = repos::git_dir(&repo) else {
            continue;
        };
        if !branch_matches(repos::head_branch(&gitdir).as_deref(), branch_filter) {
            continue;
        }
        ranked.push(Ranked {
            score,
            touched: reflog_touched(&gitdir),
            repo,
            gitdir,
        });
    }
    // `sort -k1rn -k2rn | head -12`: score first, then the freshest
    // reflog — the script's answer to "which of these was I in".
    ranked.sort_by_key(|r| (Reverse(r.score), Reverse(r.touched)));
    ranked.truncate(repos::MAX_ROWS);
    ranked
}

/// What one emitted row needs — gathered so `row_json` is a pure assembly
/// step the tests can drive without a filesystem.
struct RowSpec<'a> {
    id: &'a str,
    title: &'a str,
    display: &'a str,
    subtitle: &'a str,
    accessory: &'a str,
    open: &'a str,
    score: u64,
    branch: &'a str,
    upstream: &'a str,
    ahead: u64,
    behind: u64,
    dirty: u64,
    web: &'a str,
    slug: &'a str,
}

/// The jq object the script emits per row. The drift numbers, dirt count,
/// upstream, display path, age and slug are kept apart from the subtitle
/// string so the `repos` view can weight them — a layout cannot give the
/// branch and the ahead count different weights if they arrive already
/// glued into one string. The action list grows a GitHub entry only for a
/// repo with a web page, and a `gh:` query only for a github slug.
fn row_json(r: &RowSpec<'_>) -> Value {
    let mut actions = vec![
        json!({"title": "Open in Editor", "shortcut": "↵", "exec": r.open}),
        json!({"title": "Open Terminal Here",
               "exec": format!("setsid uwsm-app -- xdg-terminal-exec --dir={}", shq(r.id))}),
    ];
    if !r.web.is_empty() {
        // `@sh` single-quotes; `shq`'s %q backslashes would leave the url
        // escaped differently.
        actions.push(json!({"title": "Open on GitHub",
                            "exec": format!("omarchy-launch-browser {}", quote(r.web))}));
    }
    if !r.slug.is_empty() {
        actions.push(json!({"title": "Open Issues and Pull Requests",
                            "exec": "true", "query": format!("gh:{}", r.slug)}));
    }
    json!({
        "id": r.id,
        "title": r.title,
        "detail": r.display,
        "subtitle": r.subtitle,
        "accessory": r.accessory,
        "exec": r.open,
        "score": r.score,
        "branch": r.branch,
        "ahead": r.ahead,
        "behind": r.behind,
        "dirty": r.dirty,
        "upstream": r.upstream,
        "path": r.display,
        "age": r.accessory,
        "slug": r.slug,
        "actions": actions,
    })
}

impl NativeExt for Repo {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        let query = ctx.arg.trim().to_string();
        let branch_filter = ctx.filters.get("branch").cloned().unwrap_or_default();
        // `.repoOpen // empty` — a top-level oxy.json key, not one of the
        // extension's declared settings.
        let repo_open = ctx
            .settings
            .raw
            .get("repoOpen")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let visual = std::env::var("VISUAL").ok().filter(|v| !v.is_empty());
        let roots = repos::roots_setting(&ctx.settings);
        Box::pin(async move {
            // The manifest's `when` also names fd, but discovery here is
            // the in-process walk in `repos` — git is the only binary the
            // answer itself still needs.
            if !on_path("git") {
                return NativeOutcome::Fallback;
            }
            // Discovery and ranking are synchronous filesystem work —
            // keep them off the reactor thread.
            let ranked =
                tokio::task::spawn_blocking(move || rank(&query, &branch_filter, roots.as_deref()))
                    .await
                    .unwrap_or_default();
            // No repos, or nothing survived the needle and the `branch:`
            // filter — the script's bare `exit 0`.
            if ranked.is_empty() {
                return NativeOutcome::Empty;
            }
            // Once per keystroke, before the row loop — the script's
            // prune_state_cache.
            let cache_dir = repos::named_cache_dir("repo-state", 4);
            repos::prune_named_cache("repo-state", &cache_dir);

            let now = now_secs() as i64;
            let mut rows = Vec::with_capacity(ranked.len());
            for r in &ranked {
                let state = repos::repo_state(&r.repo, &r.gitdir).await;
                // `[[ -n $branch ]] || branch="?"` — even a cached line
                // that carried an empty branch still reads "?".
                let branch = if state.branch.is_empty() {
                    "?"
                } else {
                    state.branch.as_str()
                };
                let (ahead, behind) = repos::parse_ab(&state.ab);
                let path = r.repo.to_string_lossy().into_owned();
                let title = r
                    .repo
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let display = repos::display_path(&r.repo);
                let open = open_command(&repo_open, visual.as_deref(), &path);
                let subtitle = state_line_str(branch, state.dirty, ahead, behind);
                let accessory = repos::age(now - state.committed as i64);
                let (_remote, web, slug) = repos::remote_facts(&r.repo, &r.gitdir).await;
                rows.push(row_json(&RowSpec {
                    id: &path,
                    title: &title,
                    display: &display,
                    subtitle: &subtitle,
                    accessory: &accessory,
                    open: &open,
                    score: r.score,
                    branch,
                    upstream: &state.upstream,
                    ahead,
                    behind,
                    dirty: state.dirty,
                    web: &web,
                    slug: &slug,
                }));
            }
            NativeOutcome::Rows(rows)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn no_needle_lists_everything_at_the_base_score() {
        assert_eq!(rank_score("", "omarchy", "/home/u/omarchy"), Some(1000));
        assert_eq!(rank_score("", "gum", "/home/u/gum"), Some(1000));
    }

    #[test]
    fn a_needle_scores_the_scripts_three_tiers() {
        // name prefix > name contains > path contains; a miss is the
        // script's `continue`.
        assert_eq!(rank_score("omar", "omarchy", "/x/omarchy"), Some(40000));
        assert_eq!(rank_score("arch", "omarchy", "/x/omarchy"), Some(25000));
        assert_eq!(rank_score("work", "gum", "/x/work/gum"), Some(10000));
        assert_eq!(rank_score("zzz", "gum", "/x/gum"), None);
    }

    #[test]
    fn ranking_lowercases_both_sides() {
        // The script's `,,` — a shouted needle still matches, a mixed-case
        // directory still ranks.
        assert_eq!(rank_score("OMAR", "Omarchy", "/X/Omarchy"), Some(40000));
        assert_eq!(rank_score("OmAr", "OMARCHY", "/x/OMARCHY"), Some(40000));
    }

    #[test]
    fn the_branch_filter_is_a_caseless_substring_of_head() {
        assert!(branch_matches(Some("main"), "main"));
        assert!(branch_matches(Some("feature/LOGIN"), "login"));
        assert!(!branch_matches(Some("main"), "dev"));
        // A HEAD that cannot be read is the script's `|| continue`.
        assert!(!branch_matches(None, "main"));
        // No filter passes every repo without HEAD being read at all.
        assert!(branch_matches(None, ""));
        assert!(branch_matches(Some("main"), ""));
    }

    #[test]
    fn the_state_line_adds_each_mark_only_while_it_is_true() {
        assert_eq!(state_line_str("main", 0, 0, 0), "main");
        assert_eq!(state_line_str("main", 3, 0, 0), "main ●");
        assert_eq!(state_line_str("main", 0, 2, 0), "main ↑2");
        assert_eq!(state_line_str("main", 0, 0, 1), "main ↓1");
        assert_eq!(state_line_str("dev", 2, 4, 1), "dev ● ↑4 ↓1");
    }

    #[test]
    fn open_command_prefers_the_setting() {
        // repoOpen beats VISUAL, and {} is where the path lands — every
        // occurrence, not just the first.
        assert_eq!(
            open_command("zeditor {}", Some("code"), "/x/y"),
            "zeditor /x/y"
        );
        assert_eq!(open_command("run {} and {}", None, "/x"), "run /x and /x");
        // No {} — the path is appended, the way editors expect it.
        assert_eq!(open_command("zeditor", None, "/x/y"), "zeditor /x/y");
    }

    #[test]
    fn open_command_falls_back_to_visual_then_the_launcher() {
        // VISUAL is verbatim — it carries its own flags.
        assert_eq!(
            open_command("", Some("code --wait"), "/x/y"),
            "setsid code --wait /x/y"
        );
        assert_eq!(open_command("", None, "/x/y"), "omarchy-launch-editor /x/y");
        // An empty VISUAL is no VISUAL.
        assert_eq!(
            open_command("", Some(""), "/x/y"),
            "omarchy-launch-editor /x/y"
        );
    }

    #[test]
    fn open_command_quotes_paths_the_way_percent_q_does() {
        assert_eq!(
            open_command("", None, "/x/two words"),
            "omarchy-launch-editor /x/two\\ words"
        );
        assert_eq!(
            open_command("zeditor {}", None, "/x/two words"),
            "zeditor /x/two\\ words"
        );
    }

    /// A spec in the shape a clean github repo produces; tests mutate the
    /// fields they are about.
    fn spec() -> RowSpec<'static> {
        RowSpec {
            id: "/x/omarchy",
            title: "omarchy",
            display: "~/omarchy",
            subtitle: "main",
            accessory: "2m",
            open: "omarchy-launch-editor /x/omarchy",
            score: 40000,
            branch: "main",
            upstream: "origin/main",
            ahead: 0,
            behind: 0,
            dirty: 0,
            web: "",
            slug: "",
        }
    }

    #[test]
    fn a_row_keeps_numbers_as_numbers() {
        let row = row_json(&spec());
        // jq's --argjson: a zero is emitted, not omitted — the `repos`
        // view reads the drift even when it is nothing.
        assert_eq!(row["score"], json!(40000));
        assert_eq!(row["dirty"], json!(0));
        assert_eq!(row["ahead"], json!(0));
        assert_eq!(row["behind"], json!(0));
        assert_eq!(row["branch"], json!("main"));
        assert_eq!(row["upstream"], json!("origin/main"));
        // `path` is the display path and `age` the accessory — the same
        // strings under their own names, the way the script passed them.
        assert_eq!(row["path"], json!("~/omarchy"));
        assert_eq!(row["age"], json!("2m"));
        assert_eq!(row["exec"], json!("omarchy-launch-editor /x/omarchy"));
    }

    #[test]
    fn a_remote_adds_the_github_actions_in_order() {
        let mut s = spec();
        // No remote: just the two actions every repo gets.
        let row = row_json(&s);
        let titles: Vec<&str> = row["actions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|a| a["title"].as_str())
            .collect();
        assert_eq!(titles, ["Open in Editor", "Open Terminal Here"]);
        assert_eq!(
            row["actions"][1]["exec"],
            json!("setsid uwsm-app -- xdg-terminal-exec --dir=/x/omarchy")
        );

        // A github remote grows both, in the script's order — the web
        // page single-quoted the way @sh writes it.
        s.web = "https://github.com/a/b";
        s.slug = "a/b";
        let row = row_json(&s);
        assert_eq!(
            row["actions"][2],
            json!({"title": "Open on GitHub",
                   "exec": "omarchy-launch-browser 'https://github.com/a/b'"})
        );
        assert_eq!(
            row["actions"][3],
            json!({"title": "Open Issues and Pull Requests",
                   "exec": "true", "query": "gh:a/b"})
        );

        // A web page that is not github gets the browser action but no
        // `gh:` query.
        s.slug = "";
        let row = row_json(&s);
        assert_eq!(row["actions"].as_array().unwrap().len(), 3);
    }
}
