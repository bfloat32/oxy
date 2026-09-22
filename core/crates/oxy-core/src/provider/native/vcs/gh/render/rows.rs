//! The small rows — skeletons, hints, the empty-state answers, and
//! `prrow`, the pull-request row the repo list, search reshape and
//! inbox all share.

use super::jq::*;

// ---------------------------------------------------------------- tiny rows

/// `row()` — the bash helper's `jq -nc` object, field order and all.
pub(crate) fn row(
    title: &str,
    subtitle: &str,
    detail: &str,
    exec: &str,
    id: &str,
    score: i64,
) -> Value {
    json!({"id": id, "title": title, "subtitle": subtitle, "detail": detail,
           "exec": exec, "score": score})
}

/// `signin_row` — the two failures, told apart by `gh auth token` (the
/// caller ran it: keyring-local, no request).
pub(crate) fn signin_row(authed: bool) -> Value {
    if authed {
        row(
            "GitHub is not answering",
            "signed in, no reply",
            "the network, or GitHub itself",
            "",
            "gh-unreachable",
            99000,
        )
    } else {
        row(
            "Not signed in to GitHub",
            "gh auth login",
            "everything under gh: needs an account",
            &format!("{TUI} gh auth login"),
            "gh-signin",
            99000,
        )
    }
}

/// `pr_skeleton` — a pull request drawn from the slug and number alone.
/// `num` already parsed: `--argjson` is where the script died on a
/// non-number, and the caller returns `Empty` for it instead.
pub(crate) fn pr_skeleton(slug: &str, num: i64) -> Value {
    let url = format!("https://github.com/{slug}/pull/{num}");
    let open = format!("{BROWSER} {}", quote(&url));
    json!({
        "id": format!("pr:{slug}#{num}"),
        "view": "ghpr",
        "title": format!("#{num}"),
        "subtitle": format!("{slug} #{num}"),
        "exec": open,
        "score": 90000,
        "waiting": true,
        "pr": {"repo": slug, "number": num, "title": "", "url": url},
        "checks": [],
        "reviews": [],
        "actions": [
            {"title": "Open in Browser", "shortcut": "↵", "exec": open},
            {"title": "Copy URL",
             "exec": format!("printf %s {} | wl-copy", quote(&url))}
        ]
    })
}

/// `repo_skeleton` — a repository drawn from its slug alone.
pub(crate) fn repo_skeleton(slug: &str) -> Value {
    let url = format!("https://github.com/{slug}");
    let open = format!("{BROWSER} {}", quote(&url));
    json!({
        "id": format!("gh:{slug}"),
        "view": "ghrepo",
        "title": slug,
        "subtitle": "",
        "exec": open,
        "score": 90000,
        "waiting": true,
        "repo": {"slug": slug, "url": url, "description": ""},
        "prs": [],
        "actions": [
            {"title": "Open in Browser", "shortcut": "↵", "exec": open},
            {"title": "Pull Requests", "exec": "true", "query": format!("pr:{slug}")},
            {"title": "Clone", "exec": format!("{TUI} gh repo clone {}", quote(slug))},
            {"title": "Copy URL",
             "exec": format!("printf %s {} | wl-copy", quote(&url))}
        ]
    })
}

/// `mode_issues`'s numbered row — the one answer that never needed a
/// request: GitHub redirects /issues/N to the pull request when it is one.
pub(crate) fn issue_row(slug: &str, num: i64) -> Value {
    let url = format!("https://github.com/{slug}/issues/{num}");
    let open = format!("{BROWSER} {}", quote(&url));
    json!({
        "id": format!("issue:{slug}#{num}"),
        "title": format!("{slug} #{num}"),
        "subtitle": "open on GitHub",
        "exec": open,
        "score": 90000,
        "actions": [
            {"title": "Open in Browser", "shortcut": "↵", "exec": open},
            {"title": "Copy URL",
             "exec": format!("printf %s {} | wl-copy", quote(&url))}
        ]
    })
}

/// `mode_runs`'s hint for a slug-less query.
pub(crate) fn ci_hint() -> Value {
    row(
        "Name a repository",
        "ci:owner/repo",
        "workflow runs belong to a repository",
        "",
        "ci-hint",
        90000,
    )
}

/// The bare-`pr:` empty inbox — an answer, not a failed search.
pub(crate) fn prs_empty() -> Value {
    row(
        "Nothing open, nothing waiting",
        "no pull requests of yours, none for you to review",
        "",
        "",
        "pr-empty",
        90000,
    )
}

/// The bare-`issue:` equivalent.
pub(crate) fn issues_empty() -> Value {
    row(
        "Nothing assigned, nothing mentioning you",
        "an empty inbox is an answer",
        "",
        "",
        "issue-empty",
        90000,
    )
}

// ---------------------------------------------------------------- prrow

/// `prrow($group; $base)` — one pull-request row, shared by the repo list,
/// the search reshape and the inbox.
pub(crate) fn prrow(p: &Value, group: &str, base: i64, slug_fb: &str, now: i64) -> Option<Value> {
    // `$p.repository.nameWithOwner // $slug` — a scalar `repository` is
    // the index error, and a surviving non-string is the `+` death.
    let repo = match at(at(p, "repository")?, "nameWithOwner")? {
        Value::Null | Value::Bool(false) => slug_fb.to_string(),
        Value::String(s) => s.clone(),
        _ => return None,
    };

    let num = tostring(at(p, "number")?);
    let st = prstate(p)?;
    let m = mark(&st)?;

    let title = format!(
        "{}{}",
        if truthy(at(p, "isDraft")?) {
            "draft  "
        } else {
            ""
        },
        sc(at(p, "title"))?
    );

    // `[ $repo, "#n", (.author.login // ""), (.headRefName // "") ]
    //  | map(select(. != "")) | join("  ")` — the `// ""` reads emit raw,
    //  so a number still stringifies through `join`; an object is the
    //  death, which `join` supplies.
    let detail = join_ne(
        vec![
            Value::String(repo.clone()),
            Value::String(format!("#{num}")),
            emit_or_empty(at(at(p, "author")?, "login")),
            emit_or_empty(at(p, "headRefName")),
        ],
        "  ",
    )?;

    let size = if over_zero(alt(at(p, "changedFiles"), &NULL)) {
        format!(
            "+{} −{}  {} files",
            tostring(at(p, "additions")?),
            tostring(at(p, "deletions")?),
            tostring(at(p, "changedFiles")?)
        )
    } else {
        String::new()
    };
    let subtitle = join_ne(
        vec![
            Value::String(revword(at(p, "reviewDecision")?).to_string()),
            Value::String(size),
        ],
        "   ",
    )?;

    let url = at(p, "url")?;
    let open = openurl(url)?;
    Some(json!({
        "id": format!("pr:{repo}#{num}"),
        "title": title,
        "detail": detail,
        "subtitle": subtitle,
        "accessory": accessory(m, ago(at(p, "updatedAt")?, now)),
        "group": group,
        "exec": open,
        "score": base,
        "actions": [
            {"title": "Open in Browser", "shortcut": "↵", "exec": open},
            {"title": "This Pull Request", "exec": "true",
             "query": format!("pr:{repo}#{num}")},
            {"title": "Show Diff",
             "exec": format!("'{TUI}' gh pr diff --repo {} {num}", quote(&repo))},
            {"title": "Check Out Locally",
             "exec": format!("'{TUI}' gh pr checkout --repo {} {num}", quote(&repo))},
            {"title": "Copy URL", "exec": copyurl(url)?}
        ]
    }))
}
