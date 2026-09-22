//! The offline fast path is what the cases pin, so that is what these pin
//! in the small: `parse_target`'s spellings, the skeleton rows, and the
//! emits over fixture JSON — the same answers `bin/oxy-gh` would have
//! printed, minus the processes.

use super::cache::{self, Session};
use super::{Kind, parse_target, render};
use serde_json::json;

/// 2027-01-15T08:00:00Z — fixture stamps of `2027-01-15T07:00:00Z` read
/// "1h", and `2026-*` ones read as days/months.
const NOW: i64 = 1_800_000_000;

fn t(q: &str) -> (String, String, Kind, String) {
    let t = parse_target(q);
    (t.slug, t.num, t.kind, t.text)
}

#[test]
fn a_bare_slug_is_a_repo() {
    let (slug, num, kind, text) = t("basecamp/omarchy");
    assert_eq!(slug, "basecamp/omarchy");
    assert_eq!(num, "");
    assert_eq!(kind, Kind::Repo);
    assert_eq!(text, "");
}

#[test]
fn a_pasted_pr_url_is_a_numbered_target() {
    let (slug, num, kind, _) = t("https://github.com/basecamp/omarchy/pull/7398");
    assert_eq!(slug, "basecamp/omarchy");
    assert_eq!(num, "7398");
    assert_eq!(kind, Kind::Pr);
}

#[test]
fn a_pasted_issue_url_is_the_issue_kind() {
    let (slug, num, kind, _) = t("https://github.com/basecamp/omarchy/issues/51");
    assert_eq!(slug, "basecamp/omarchy");
    assert_eq!(num, "51");
    assert_eq!(kind, Kind::Issue);
}

#[test]
fn hash_shorthand_is_the_number_kind() {
    let (slug, num, kind, _) = t("basecamp/omarchy#123");
    assert_eq!(slug, "basecamp/omarchy");
    assert_eq!(num, "123");
    assert_eq!(kind, Kind::Number);
}

#[test]
fn a_git_suffix_comes_off_anywhere() {
    assert_eq!(
        t("https://github.com/basecamp/omarchy.git").0,
        "basecamp/omarchy"
    );
    assert_eq!(t("basecamp/omarchy.git").0, "basecamp/omarchy");
    assert_eq!(t("basecamp/omarchy.git").2, Kind::Repo);
}

#[test]
fn url_dressings_all_strip() {
    assert_eq!(t("<https://github.com/a/b>").0, "a/b");
    assert_eq!(t("http://github.com/a/b").0, "a/b");
    assert_eq!(t("www.github.com/a/b").0, "a/b");
    assert_eq!(t("https://github.com/a/b?tab=readme").0, "a/b");
    assert_eq!(t("https://github.com/a/b#readme").0, "a/b");
    assert_eq!(t("https://github.com/a/b/").0, "a/b");
}

#[test]
fn a_deep_pull_link_degrades_to_the_repo() {
    // `/pull/1/files` — `${rest##*/}` reads "files", which is no number.
    let (slug, num, kind, _) = t("https://github.com/a/b/pull/12/files");
    assert_eq!(slug, "a/b");
    assert_eq!(num, "");
    assert_eq!(kind, Kind::Repo);
}

#[test]
fn a_number_after_a_space_is_the_number_kind() {
    let (slug, num, kind, text) = t("basecamp/omarchy 123");
    assert_eq!(slug, "basecamp/omarchy");
    assert_eq!(num, "123");
    assert_eq!(kind, Kind::Number);
    assert_eq!(text, "");
}

#[test]
fn words_after_a_slug_are_search_text() {
    // `repo theme` — KIND stays repo and T_TEXT carries the words; the
    // script's space branch returns before the digit check.
    let (slug, num, kind, text) = t("basecamp/omarchy theme dark");
    assert_eq!(slug, "basecamp/omarchy");
    assert_eq!(num, "");
    assert_eq!(kind, Kind::Repo);
    assert_eq!(text, "theme dark");
}

#[test]
fn a_numberish_word_is_search_text_not_a_number() {
    let (slug, num, kind, text) = t("basecamp/omarchy 12x");
    assert_eq!(slug, "basecamp/omarchy");
    assert_eq!(num, "");
    assert_eq!(kind, Kind::Repo);
    assert_eq!(text, "12x");
}

#[test]
fn a_hash_word_after_a_space_is_number_kind_unvalidated() {
    // `o/r #abc` — the space branch returns early, T_NUM unvalidated. The
    // skeleton's `--argjson` is what dies on it downstream.
    let (slug, num, kind, _) = t("basecamp/omarchy #abc");
    assert_eq!(slug, "basecamp/omarchy");
    assert_eq!(num, "abc");
    assert_eq!(kind, Kind::Number);
}

#[test]
fn plain_words_are_text() {
    let (slug, num, kind, text) = t("some words that are not a repo");
    assert_eq!(slug, "");
    assert_eq!(num, "");
    assert_eq!(kind, Kind::Text);
    assert_eq!(text, "some words that are not a repo");
}

#[test]
fn nothing_is_empty() {
    let (_, _, kind, text) = t("");
    assert_eq!(kind, Kind::Empty);
    assert_eq!(text, "");
}

#[test]
fn one_leading_space_strips() {
    let (slug, _, kind, _) = t(" basecamp/omarchy");
    assert_eq!(slug, "basecamp/omarchy");
    assert_eq!(kind, Kind::Repo);
}

#[test]
fn a_host_looking_pair_is_still_a_slug() {
    // The script's slug check is `[A-Za-z0-9._-]+` on both sides — a
    // non-github URL degrades to a two-level slug, quirks and all.
    let (slug, _, kind, _) = t("https://example.com/x");
    assert_eq!(slug, "example.com/x");
    assert_eq!(kind, Kind::Repo);
}

#[test]
fn the_pr_skeleton_is_the_offline_row() {
    let row = render::pr_skeleton("basecamp/omarchy", 7398);
    assert_eq!(row["view"], "ghpr");
    assert_eq!(row["id"], "pr:basecamp/omarchy#7398");
    assert_eq!(row["title"], "#7398");
    assert_eq!(row["subtitle"], "basecamp/omarchy #7398");
    assert_eq!(row["waiting"], true);
    let exec = row["exec"].as_str().unwrap();
    assert!(exec.contains("/pull/7398"));
    assert!(exec.starts_with("omarchy-launch-browser "));
    assert_eq!(row["pr"]["number"], 7398);
    assert_eq!(row["pr"]["repo"], "basecamp/omarchy");
    assert_eq!(row["checks"], json!([]));
    assert_eq!(row["reviews"], json!([]));
    let actions = row["actions"].as_array().unwrap();
    assert_eq!(actions[0]["title"], "Open in Browser");
    assert_eq!(actions[0]["shortcut"], "↵");
    assert_eq!(actions[1]["title"], "Copy URL");
    assert!(actions[1]["exec"].as_str().unwrap().contains("wl-copy"));
}

#[test]
fn the_repo_skeleton_is_the_offline_row() {
    let row = render::repo_skeleton("basecamp/omarchy");
    assert_eq!(row["view"], "ghrepo");
    assert_eq!(row["id"], "gh:basecamp/omarchy");
    assert_eq!(row["title"], "basecamp/omarchy");
    assert_eq!(row["waiting"], true);
    assert_eq!(row["repo"]["slug"], "basecamp/omarchy");
    let exec = row["exec"].as_str().unwrap();
    assert!(exec.contains("https://github.com/basecamp/omarchy"));
    let actions = row["actions"].as_array().unwrap();
    assert_eq!(actions[1]["query"], "pr:basecamp/omarchy");
    assert!(
        actions[2]["exec"]
            .as_str()
            .unwrap()
            .contains("gh repo clone")
    );
}

#[test]
fn the_issue_row_never_needed_a_request() {
    let row = render::issue_row("basecamp/omarchy", 5);
    assert_eq!(row["id"], "issue:basecamp/omarchy#5");
    assert_eq!(row["title"], "basecamp/omarchy #5");
    assert_eq!(row["subtitle"], "open on GitHub");
    assert!(row["exec"].as_str().unwrap().contains("/issues/5"));
}

#[test]
fn the_ci_hint_asks_for_a_slug() {
    let row = render::ci_hint();
    assert_eq!(row["title"], "Name a repository");
    assert_eq!(row["subtitle"], "ci:owner/repo");
}

#[test]
fn a_pr_panel_row_draws_the_view() {
    let body = json!({
        "data": {"repository": {"issueOrPullRequest": {
            "__typename": "PullRequest",
            "number": 111, "title": "fix the thing", "state": "OPEN",
            "url": "https://github.com/trophos/trophos/pull/111",
            "isDraft": false, "headRefName": "fix", "baseRefName": "main",
            "additions": 10, "deletions": 2, "changedFiles": 3,
            "mergeable": "MERGEABLE",
            "author": {"login": "troph"},
            "reviewDecision": "APPROVED",
            "comments": {"totalCount": 2},
            "createdAt": "2027-01-01T00:00:00Z",
            "updatedAt": "2027-01-15T07:00:00Z",
            "reviews": {"nodes": [
                {"state": "APPROVED", "submittedAt": "2027-01-15T07:00:00Z",
                 "author": {"login": "rev"}},
                {"state": "COMMENTED", "submittedAt": "2027-01-15T07:00:00Z",
                 "author": {"login": ""}}
            ]},
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {
                "state": "FAILURE",
                "contexts": {"nodes": [
                    {"__typename": "CheckRun", "name": "build",
                     "conclusion": "FAILURE", "status": "COMPLETED",
                     "startedAt": "2027-01-15T07:00:00Z",
                     "completedAt": "2027-01-15T07:01:30Z",
                     "detailsUrl": "https://x/y"},
                    {"__typename": "StatusContext", "context": "lint",
                     "state": "SUCCESS", "targetUrl": "https://x/z"}
                ]}
            }}}]}
        }}}
    });
    let rows = render::pr_panel(&body.to_string(), "trophos/trophos", NOW);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["view"], "ghpr");
    assert_eq!(row["id"], "pr:trophos/trophos#111");
    assert_eq!(row["title"], "fix the thing");
    assert_eq!(row["subtitle"], "trophos/trophos #111");
    assert_eq!(row["pr"]["mark"], "✗");
    assert_eq!(row["pr"]["failing"], 1);
    assert_eq!(row["pr"]["total"], 2);
    assert_eq!(row["pr"]["review"], "approved");
    assert_eq!(row["pr"]["rollup"], "FAILURE");
    // Sorted: the red check first, and its took string is minutes.
    assert_eq!(row["checks"][0]["name"], "build");
    assert_eq!(row["checks"][0]["mark"], "✗");
    assert_eq!(row["checks"][0]["state"], "failure");
    assert_eq!(row["checks"][0]["took"], "1m");
    assert_eq!(row["checks"][1]["name"], "lint");
    assert_eq!(row["checks"][1]["mark"], "✓");
    // The nameless review is filtered by `select(.who != "")`.
    let reviews = row["reviews"].as_array().unwrap();
    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0]["who"], "rev");
    assert_eq!(reviews[0]["mark"], "✓");
    // The actions are the panel's own order.
    let titles: Vec<&str> = row["actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["title"].as_str())
        .collect();
    assert_eq!(
        titles,
        [
            "Open in Browser",
            "Show Diff",
            "Watch Checks",
            "Check Out Locally",
            "Workflow Runs",
            "Copy URL"
        ]
    );
}

#[test]
fn an_issue_typename_draws_the_issue_row() {
    let body = json!({
        "data": {"repository": {"issueOrPullRequest": {
            "__typename": "Issue",
            "number": 5, "title": "it broke", "state": "OPEN",
            "url": "https://github.com/a/b/issues/5",
            "author": {"login": "who"},
            "comments": {"totalCount": 3},
            "labels": {"nodes": [{"name": "bug"}]},
            "updatedAt": "2027-01-15T07:00:00Z"
        }}}
    });
    let rows = render::pr_panel(&body.to_string(), "a/b", NOW);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "issue:a/b#5");
    assert_eq!(rows[0]["group"], "Issue");
    assert_eq!(rows[0]["subtitle"], "bug");
    assert!(rows[0]["exec"].as_str().unwrap().contains("/issues/5"));
    assert!(rows[0]["accessory"].as_str().unwrap().contains('󰆉'));
}

#[test]
fn a_null_target_is_no_such_number() {
    let body = json!({"data": {"repository": {"issueOrPullRequest": null}}});
    let rows = render::pr_panel(&body.to_string(), "a/b", NOW);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["title"], "a/b has no such number");
}

#[test]
fn a_repo_panel_row_draws_ghrepo() {
    let body = json!({
        "data": {"repository": {
            "nameWithOwner": "basecamp/omarchy",
            "description": "an opinionated setup",
            "url": "https://github.com/basecamp/omarchy",
            "isPrivate": false, "isArchived": false, "isFork": false,
            "stargazerCount": 100, "forkCount": 4, "pushedAt": "2027-01-15T07:00:00Z",
            "primaryLanguage": {"name": "Shell"},
            "defaultBranchRef": {"name": "main", "target": {
                "abbreviatedOid": "abc1234", "messageHeadline": "a commit",
                "committedDate": "2027-01-15T07:00:00Z",
                "statusCheckRollup": {"state": "SUCCESS"}}},
            "pullRequests": {"totalCount": 3},
            "issues": {"totalCount": 45},
            "latestRelease": {"tagName": "v1.0", "publishedAt": "2027-01-01T00:00:00Z"},
            "openPrs": {"nodes": [{
                "number": 9, "title": "open pr", "url": "https://x/pr/9",
                "isDraft": true, "updatedAt": "2027-01-15T07:00:00Z",
                "author": {"login": "troph"}, "reviewDecision": "REVIEW_REQUIRED",
                "commits": {"nodes": []}
            }]}
        }}
    });
    let rows = render::repo_panel(&body.to_string(), "basecamp/omarchy", NOW);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["view"], "ghrepo");
    assert_eq!(row["title"], "basecamp/omarchy");
    assert_eq!(row["subtitle"], "an opinionated setup");
    assert_eq!(row["repo"]["stars"], 100);
    assert_eq!(row["repo"]["prs"], 3);
    assert_eq!(row["repo"]["branch"], "main");
    assert_eq!(row["repo"]["headMark"], "✓");
    assert_eq!(row["repo"]["release"], "v1.0");
    assert_eq!(row["prs"][0]["draft"], true);
    assert_eq!(row["prs"][0]["review"], "review needed");
    let titles: Vec<&str> = row["actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["title"].as_str())
        .collect();
    assert_eq!(
        titles,
        [
            "Open in Browser",
            "Pull Requests",
            "Workflow Runs",
            "Issues",
            "Clone",
            "Copy URL"
        ]
    );
    assert_eq!(row["actions"][1]["query"], "pr:basecamp/omarchy");
    assert_eq!(row["actions"][2]["query"], "ci:basecamp/omarchy");
}

#[test]
fn a_null_repository_is_not_found() {
    let body = json!({"data": {"repository": null}});
    let rows = render::repo_panel(&body.to_string(), "a/b", NOW);
    assert_eq!(rows[0]["title"], "a/b");
    assert_eq!(rows[0]["subtitle"], "not found, or no access");
}

#[test]
fn own_repos_filter_sort_and_cap() {
    let mut repos = Vec::new();
    for i in 0..15 {
        repos.push(json!({
            "nameWithOwner": format!("me/repo{i:02}"),
            "description": "",
            "primaryLanguage": {"name": "Rust"},
            "stargazerCount": i,
            "pushedAt": format!("2026-01-{:02}T00:00:00Z", i + 1),
            "isPrivate": false, "isArchived": false,
            "url": format!("https://github.com/me/repo{i:02}")
        }));
    }
    let rows = render::own_repos_rows(&json!(repos).to_string(), "", NOW);
    assert_eq!(rows.len(), 12);
    // Newest push first, and the score walks down by 100 a row.
    assert_eq!(rows[0]["id"], "own:me/repo14");
    assert_eq!(rows[0]["score"], 90000);
    assert_eq!(rows[1]["score"], 89900);
    assert_eq!(rows[0]["group"], "Your Repos");

    // A text query narrows on slug + description.
    let rows = render::own_repos_rows(&json!(repos).to_string(), "repo05", NOW);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "own:me/repo05");
}

#[test]
fn found_repos_skip_the_ones_already_yours() {
    let found = json!([
        {"fullName": "me/repo14", "description": "", "language": "Rust",
         "stargazersCount": 1, "updatedAt": "2027-01-15T07:00:00Z",
         "url": "https://github.com/me/repo14"},
        {"fullName": "other/cool", "description": "neat", "language": "Go",
         "stargazersCount": 9, "updatedAt": "2027-01-15T07:00:00Z",
         "url": "https://github.com/other/cool"}
    ]);
    let seen = vec![json!("me/repo14")];
    let rows = render::found_repos_rows(&found.to_string(), &seen, NOW);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "found:other/cool");
    assert_eq!(rows[0]["group"], "On GitHub");
    assert_eq!(rows[0]["score"], 80000);
}

#[test]
fn repo_prs_draw_rows_from_graphql_nodes() {
    let body = json!({
        "data": {"repository": {"nameWithOwner": "a/b", "pullRequests": {"nodes": [
            {"number": 7, "title": "one", "url": "https://x/7",
             "isDraft": false, "updatedAt": "2027-01-15T07:00:00Z",
             "additions": 1, "deletions": 0, "changedFiles": 1,
             "repository": {"nameWithOwner": "a/b"}, "headRefName": "br",
             "reviewDecision": null, "author": {"login": "me"},
             "commits": {"nodes": [{"commit": {"statusCheckRollup":
                {"state": "SUCCESS"}}}]}}
        ]}}}
    });
    let rows = render::repo_prs_rows(&body.to_string(), "a/b", "", NOW);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "pr:a/b#7");
    assert_eq!(rows[0]["group"], "Open Pull Requests  a/b");
    assert_eq!(rows[0]["accessory"], "✓ 1h");

    // The keep filter reads title/author/branch/number.
    let rows = render::repo_prs_rows(&body.to_string(), "a/b", "nope", NOW);
    assert!(rows.is_empty());
    let rows = render::repo_prs_rows(&body.to_string(), "a/b", "#7", NOW);
    assert_eq!(rows.len(), 1);
}

#[test]
fn the_pr_inbox_groups_waiting_then_mine() {
    let pr = |n: i64, repo: &str| {
        json!({"number": n, "title": "t", "url": "https://x/p",
               "isDraft": false, "updatedAt": "2027-01-15T07:00:00Z",
               "repository": {"nameWithOwner": repo},
               "author": {"login": "who"}, "headRefName": "br",
               "commits": {"nodes": []}})
    };
    let body = json!({
        "data": {
            "review": {"nodes": [pr(1, "a/b")]},
            "mine": {"nodes": [pr(2, "c/d"), pr(3, "e/f")]}
        }
    });
    let rows = render::mine_prs_rows(&body.to_string(), "", NOW);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["group"], "Waiting On You");
    assert_eq!(rows[0]["score"], 95000);
    assert_eq!(rows[1]["group"], "Your Pull Requests");
    assert_eq!(rows[1]["score"], 89900); // b - i*100 over the concat
    assert_eq!(rows[2]["score"], 89800);
}

#[test]
fn the_issue_inbox_dedupes_by_url() {
    let issue = |n: i64| {
        json!({"number": n, "title": "t", "url": format!("https://x/i{n}"),
               "updatedAt": "2027-01-15T07:00:00Z",
               "repository": {"nameWithOwner": "a/b"},
               "author": {"login": "who"}, "comments": {"totalCount": 0},
               "labels": {"nodes": []}})
    };
    let body = json!({
        "data": {
            "assigned": {"nodes": [issue(1)]},
            "mentioned": {"nodes": [issue(1), issue(2)]}
        }
    });
    let rows = render::my_issues_rows(&body.to_string(), "", NOW);
    assert_eq!(rows.len(), 2);
    // The dupe keeps the first — "Assigned To You".
    assert_eq!(rows[0]["group"], "Assigned To You");
    assert_eq!(rows[0]["id"], "issue:a/b#1");
    assert_eq!(rows[1]["id"], "issue:a/b#2");
}

#[test]
fn repo_issues_draw_rows() {
    let body = json!({
        "data": {"repository": {"issues": {"nodes": [
            {"number": 4, "title": "clipboard thing", "url": "https://x/4",
             "updatedAt": "2027-01-15T07:00:00Z",
             "author": {"login": "who"}, "comments": {"totalCount": 2},
             "labels": {"nodes": [{"name": "bug"}]}}
        ]}}}
    });
    let rows = render::repo_issues_rows(&body.to_string(), "a/b", "", NOW);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "issue:a/b#4");
    assert_eq!(rows[0]["group"], "Open Issues  a/b");
    assert_eq!(rows[0]["subtitle"], "bug");
    assert!(rows[0]["accessory"].as_str().unwrap().contains('󰆉'));
    assert_eq!(rows[0]["actions"][1]["query"], "gh:a/b");

    // `keep` searches the label names too.
    let rows = render::repo_issues_rows(&body.to_string(), "a/b", "bug", NOW);
    assert_eq!(rows.len(), 1);
    let rows = render::repo_issues_rows(&body.to_string(), "a/b", "nope", NOW);
    assert!(rows.is_empty());
}

#[test]
fn run_rows_put_failures_first_by_score() {
    let runs = json!([
        {"databaseId": 1, "displayTitle": "ok", "workflowName": "ci",
         "status": "completed", "conclusion": "success", "headBranch": "main",
         "event": "push", "createdAt": "2027-01-15T07:00:00Z",
         "url": "https://x/1"},
        {"databaseId": 2, "displayTitle": "bad", "workflowName": "ci",
         "status": "completed", "conclusion": "failure", "headBranch": "main",
         "event": "push", "createdAt": "2027-01-15T07:00:00Z",
         "url": "https://x/2"},
        {"databaseId": 3, "displayTitle": "wip", "workflowName": "ci",
         "status": "in_progress", "conclusion": null, "headBranch": "dev",
         "event": "push", "createdAt": "2027-01-15T07:00:00Z",
         "url": "https://x/3"}
    ]);
    let rows = render::run_rows(&runs.to_string(), "a/b", NOW);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["score"], 90000); // success is the 90000 tier
    assert_eq!(rows[0]["accessory"], "✓ 1h");
    assert_eq!(rows[1]["score"], 94900); // the ✗ tier minus a slot
    assert_eq!(rows[2]["score"], 91800); // ● tier — in_progress is "●"
    assert_eq!(rows[2]["subtitle"], "in_progress"); // status when no conclusion
    assert_eq!(rows[0]["detail"], "ci  main  push");
    assert_eq!(rows[0]["id"], "run:a/b:1");
    assert!(
        rows[1]["actions"][1]["exec"]
            .as_str()
            .unwrap()
            .contains("gh run view --repo 'a/b' 2 --log-failed")
    );
}

#[test]
fn search_results_reshape_into_the_graphql_nesting() {
    let flat = json!([
        {"number": 9, "title": "found", "url": "https://x/9",
         "updatedAt": "2027-01-15T07:00:00Z", "repository": {"nameWithOwner": "a/b"},
         "author": {"login": "who"}, "isDraft": false}
    ]);
    let body = render::reshape_search_prs(&flat.to_string(), "a/b").unwrap();
    let rows = render::repo_prs_rows(&body, "a/b", "", NOW);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "pr:a/b#9");
    // The fields `gh search` cannot know sit at their absent values.
    assert_eq!(rows[0]["subtitle"], "");

    let flat = json!([
        {"number": 3, "title": "found", "url": "https://x/3",
         "updatedAt": "2027-01-15T07:00:00Z", "repository": {"nameWithOwner": "a/b"},
         "author": {"login": "who"}, "labels": [{"name": "ui"}],
         "commentsCount": 2}
    ]);
    let body = render::reshape_search_issues(&flat.to_string(), "a/b").unwrap();
    let rows = render::repo_issues_rows(&body, "a/b", "", NOW);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "issue:a/b#3");
    assert_eq!(rows[0]["subtitle"], "ui");
    assert!(rows[0]["accessory"].as_str().unwrap().contains('󰆉'));
}

#[test]
fn dead_bodies_are_no_rows() {
    // jq died silent on all of these, so the emits answer nothing.
    assert!(render::pr_panel("not json", "a/b", NOW).is_empty());
    // A scalar where an object was indexed is the same death.
    assert!(render::pr_panel("{\"data\":{\"repository\":5}}", "a/b", NOW).is_empty());
    assert!(render::repo_panel("{", "a/b", NOW).is_empty());
    assert!(render::own_repos_rows("not json", "", NOW).is_empty());
    assert!(render::run_rows("42", "a/b", NOW).is_empty()); // .[] on a number
    assert!(render::reshape_search_prs("{}", "a/b").is_none());
}

#[test]
fn a_missing_target_is_still_a_row() {
    // `.data.repository` absent reads null, and null draws "no such
    // number" — a GraphQL answer with nothing in it is still an answer.
    let rows = render::pr_panel("{}", "a/b", NOW);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["title"], "a/b has no such number");
}

#[test]
fn graphql_commands_quote_their_user_text() {
    let cmd = super::gql::search_prs_cmd("a/b", "it's a search");
    assert!(cmd.contains("'it'\\''s a search'"));
    assert!(cmd.contains("--repo 'a/b'"));
    let cmd = super::gql::pr_cmd("o/r", "5");
    assert!(cmd.contains("-F number='5'"));
    assert!(cmd.contains("-F owner='o'"));
}

// ------------------------------------------------------------------- cache

/// A private cache dir under the system temp, swept on drop — tests run
/// parallel, so each gets its own counter suffix.
struct Tmp(std::path::PathBuf);

impl Tmp {
    fn new() -> Self {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "oxy-gh-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tmp() -> (Tmp, Session) {
    let dir = Tmp::new();
    let s = Session {
        offline: false,
        cold: false,
        fp: "testfp".to_string(),
        dir: dir.0.clone(),
        now: cache::now_secs(),
    };
    (dir, s)
}

#[test]
fn the_cache_key_is_the_scripts_md5() {
    // `printf '%s' "abc" | md5sum | cut -c1-16`
    assert_eq!(cache::key_for("abc"), "900150983cd24fb0");
    assert_eq!(cache::key_for(""), "d41d8cd98f00b204");
    // The fingerprint is twelve hex chars of credential state.
    let fp = cache::auth_fingerprint();
    assert_eq!(fp.len(), 12);
    assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
}

#[tokio::test]
async fn pull_fetches_caches_and_serves_stale() {
    let (_d, s) = tmp();
    // Callers pre-hash like the script's `pull "$(key_for …)"`.
    let key = cache::key_for("k");
    let got = cache::pull(&s, &key, 60, "printf '{\"a\":1}'").await;
    assert_eq!(got.as_deref(), Some("{\"a\":1}"));

    // The file is named like the script's.
    let file = s.dir.join(format!("testfp-{key}.json"));
    assert!(file.exists());

    // A second pull inside the ttl does not re-run the command: this one
    // would fail if it ran.
    let got = cache::pull(&s, &key, 60, "exit 1").await;
    assert_eq!(got.as_deref(), Some("{\"a\":1}"));
}

#[tokio::test]
async fn peek_serves_stale_and_arms_a_warm() {
    let (_d, s) = tmp();
    let key = cache::key_for("k");
    // Age a file past the ttl — peek serves it and spawns the refresh.
    let file = s.dir.join(format!("testfp-{key}.json"));
    std::fs::write(&file, "{\"old\":1}").unwrap();
    let mtime = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    let got = cache::peek(&s, &key, 60, "printf '{\"new\":1}'").await;
    assert_eq!(got.as_deref(), Some("{\"old\":1}"));
    assert!(s.dir.join(format!("testfp-{key}.json.lock")).exists());

    // The warm lands on its own time.
    for _ in 0..60 {
        if std::fs::read_to_string(&file).ok().as_deref() == Some("{\"new\":1}") {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("the warm never wrote");
}

#[tokio::test]
async fn cold_reads_nothing_and_offline_serves_stale() {
    let (_d, mut s) = tmp();
    let key = cache::key_for("k");
    let file = s.dir.join(format!("testfp-{key}.json"));
    std::fs::write(&file, "{\"old\":1}").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
        .unwrap(); // stale

    s.cold = true;
    assert!(cache::peek(&s, &key, 60, "printf x").await.is_none());
    assert!(cache::pull(&s, &key, 60, "printf x").await.is_none());

    s.cold = false;
    s.offline = true;
    assert_eq!(
        cache::peek(&s, &key, 60, "printf x").await.as_deref(),
        Some("{\"old\":1}")
    );
    assert_eq!(
        cache::pull(&s, &key, 60, "printf x").await.as_deref(),
        Some("{\"old\":1}")
    );
}

#[test]
fn housekeeping_sweeps_other_fingerprints_and_orphans() {
    let (d, s) = tmp();
    let dir = d.path();
    std::fs::write(dir.join("testfp-a.json"), "{}").unwrap();
    std::fs::write(dir.join("otherfp-b.json"), "{}").unwrap();
    std::fs::write(dir.join("otherfp-b.json.lock"), "").unwrap();
    std::fs::write(dir.join("testfp-c.json.lock"), "").unwrap(); // orphan
    std::fs::write(dir.join("testfp-d.json"), "{}").unwrap();
    std::fs::write(dir.join("testfp-d.json.lock"), "").unwrap(); // not an orphan
    cache::housekeeping(dir, &s.fp);
    assert!(dir.join("testfp-a.json").exists());
    assert!(!dir.join("otherfp-b.json").exists());
    assert!(!dir.join("otherfp-b.json.lock").exists());
    assert!(!dir.join("testfp-c.json.lock").exists());
    assert!(dir.join("testfp-d.json.lock").exists());
}

#[test]
fn housekeeping_evicts_past_two_hundred() {
    let (d, s) = tmp();
    let dir = d.path();
    // 202 entries, oldest first — the two oldest go.
    for i in 0..202 {
        let f = dir.join(format!("testfp-e{i:03}.json"));
        std::fs::write(&f, "{}").unwrap();
        // Backdate: mtime = now - (202 - i) seconds, oldest first.
        let age = std::time::Duration::from_secs(202 - i);
        std::fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - age)
            .unwrap();
    }
    cache::housekeeping(dir, &s.fp);
    let left = std::fs::read_dir(dir)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".json")
        })
        .count();
    assert_eq!(left, 200);
    assert!(!dir.join("testfp-e000.json").exists());
    assert!(!dir.join("testfp-e001.json").exists());
    assert!(dir.join("testfp-e201.json").exists());
}

#[test]
fn the_first_json_line_is_the_answer() {
    // The mise-chatter case the sed line exists for.
    assert_eq!(
        cache::json_only("mise tools:\n  gh@2.97\n{\"a\":1}\n"),
        "{\"a\":1}\n"
    );
    assert_eq!(cache::json_only("nothing json\n"), "");
}
