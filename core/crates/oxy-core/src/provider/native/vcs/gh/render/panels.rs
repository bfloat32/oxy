//! The single-target panels — `pr_panel` for `gh:o/r#N` and
//! `repo_panel` for `gh:o/r`. Each is one GraphQL document drawn once.

use super::jq::*;

// ---------------------------------------------------------------- panels

/// `render_pr_panel` — one row from the `GQL_PR` answer: a "no such
/// number" row on null, an issue row for an Issue, the `ghpr` panel for a
/// PullRequest.
pub(crate) fn pr_panel(body: &str, slug: &str, now: i64) -> Vec<Value> {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    pr_panel_row(&v, slug, now).into_iter().collect()
}

fn pr_panel_row(v: &Value, slug: &str, now: i64) -> Option<Value> {
    // `.data.repository.issueOrPullRequest` — `// null` does not catch
    // the index error a scalar `repository` is; the `?` keeps that death.
    let t = at(at(at(v, "data")?, "repository")?, "issueOrPullRequest")?;
    if matches!(t, Value::Null | Value::Bool(false)) {
        let url = format!("https://github.com/{slug}");
        return Some(json!({
            "id": format!("gh:{slug}"),
            "title": format!("{slug} has no such number"),
            "subtitle": "not found, or no access",
            "score": 90000,
            "exec": openurl(&Value::String(url))?
        }));
    }
    if at(t, "__typename")?.as_str() == Some("Issue") {
        let num = tostring(at(t, "number")?);
        let labels = join(&names(at(at(t, "labels")?, "nodes"))?, ", ")?;
        let url = at(t, "url")?;
        let comments = alt(at(at(t, "comments")?, "totalCount"), &NULL);
        let acc = format!(
            "{}{}",
            if over_zero(comments) {
                format!("{} 󰆉  ", tostring(comments))
            } else {
                String::new()
            },
            ago(at(t, "updatedAt")?, now)
        );
        return Some(json!({
            "id": format!("issue:{slug}#{num}"),
            "title": at(t, "title")?.clone(),
            "subtitle": labels,
            "detail": format!("{slug}  #{num}  {}", so(at(at(t, "author")?, "login"))?),
            "accessory": acc,
            "group": "Issue",
            "exec": openurl(url)?,
            "score": 90000,
            "actions": [
                {"title": "Open in Browser", "shortcut": "↵", "exec": openurl(url)?},
                {"title": "Copy URL", "exec": copyurl(url)?}
            ]
        }));
    }

    // One check per context: CheckRun carries conclusion/status and the
    // duration, StatusContext its state and target. Sorted failures, then
    // running, then the rest — stable, like `sort_by`.
    let roll = alt(
        at(
            at(ati(at(at(t, "commits")?, "nodes")?, 0)?, "commit")?,
            "statusCheckRollup",
        ),
        &NULL,
    );
    let mut checks: Vec<Map<String, Value>> = Vec::new();
    for c in list(at(at(roll, "contexts")?, "nodes")?)? {
        let mut e = Map::new();
        if at(c, "__typename")?.as_str() == Some("CheckRun") {
            e.insert("name".into(), at(c, "name")?.clone());
            e.insert(
                "state".into(),
                alt(
                    Some(alt(at(c, "conclusion"), at(c, "status").unwrap_or(&NULL))),
                    &NULL,
                )
                .clone(),
            );
            e.insert("url".into(), alt(at(c, "detailsUrl"), &NULL).clone());
            let took = if !at(c, "completedAt")?.is_null() && !at(c, "startedAt")?.is_null() {
                let end = at(c, "completedAt")?
                    .as_str()
                    .and_then(parse_iso)
                    .unwrap_or(0);
                let start = at(c, "startedAt")?
                    .as_str()
                    .and_then(parse_iso)
                    .unwrap_or(0);
                (end - start) as f64
            } else {
                0.0
            };
            e.insert("took".into(), json!(took));
        } else {
            e.insert("name".into(), at(c, "context")?.clone());
            e.insert("state".into(), alt(at(c, "state"), &NULL).clone());
            e.insert("url".into(), alt(at(c, "targetUrl"), &NULL).clone());
            e.insert("took".into(), json!(0));
        }
        let m = mark(&e["state"])?.to_string();
        e.insert("mark".into(), json!(m));
        checks.push(e);
    }
    let failing = checks.iter().filter(|c| c["mark"] == "✗").count();
    let running = checks.iter().filter(|c| c["mark"] == "●").count();
    checks.sort_by_key(|c| match c["mark"].as_str() {
        Some("✗") => 0,
        Some("●") => 1,
        _ => 2,
    });
    let checks_out: Vec<Value> = checks
        .iter()
        .map(|c| {
            let took = c["took"].as_f64().unwrap_or(0.0);
            let took_s = if took > 0.0 {
                if took < 60.0 {
                    format!("{}s", took.floor() as i64)
                } else {
                    format!("{}m", (took / 60.0).floor() as i64)
                }
            } else {
                String::new()
            };
            Some(json!({
                "name": c["name"],
                "mark": c["mark"],
                "state": so(Some(&c["state"]))?.to_lowercase(),
                "took": took_s
            }))
        })
        .collect::<Option<Vec<_>>>()?;

    let mut reviews = Vec::new();
    for r in list(at(at(t, "reviews")?, "nodes")?)? {
        // `{who: (.author.login // "")}` then `select(.who != "")` — the
        // normalized value is what is emitted and compared.
        let who = emit_or_empty(at(at(r, "author")?, "login"));
        let state = emit_or_empty(at(r, "state"));
        if who == Value::String(String::new()) {
            continue;
        }
        reviews.push(json!({
            "who": who,
            "state": state,
            "mark": match at(r, "state")?.as_str() {
                Some("APPROVED") => "✓",
                Some("CHANGES_REQUESTED") => "✗",
                _ => ""
            },
            "age": ago(at(r, "submittedAt")?, now)
        }));
    }

    let num = tostring(at(t, "number")?);
    let url = at(t, "url")?;
    let roll_state = emit_or_empty(at(roll, "state"));
    let repo_q = quote(slug);
    Some(json!({
        "id": format!("pr:{slug}#{num}"),
        "view": "ghpr",
        "title": at(t, "title")?.clone(),
        "subtitle": format!("{slug} #{num}"),
        "exec": openurl(url)?,
        "score": 90000,
        "pr": {
            "repo": slug,
            "number": at(t, "number")?.clone(),
            "title": at(t, "title")?.clone(),
            "author": emit_or_empty(at(at(t, "author")?, "login")),
            "head": emit_or_empty(at(t, "headRefName")),
            "base": emit_or_empty(at(t, "baseRefName")),
            "draft": at(t, "isDraft")? == &Value::Bool(true),
            "state": emit_or_empty(at(t, "state")),
            "mergeable": emit_or_empty(at(t, "mergeable")),
            "review": revword(at(t, "reviewDecision")?),
            "additions": alt(at(t, "additions"), &json!(0)).clone(),
            "deletions": alt(at(t, "deletions"), &json!(0)).clone(),
            "files": alt(at(t, "changedFiles"), &json!(0)).clone(),
            "comments": alt(at(at(t, "comments")?, "totalCount"), &json!(0)).clone(),
            "age": ago(at(t, "updatedAt")?, now),
            "opened": ago(at(t, "createdAt")?, now),
            "rollup": roll_state,
            "mark": mark(&roll_state)?,
            "failing": failing as i64,
            "running": running as i64,
            "total": checks.len() as i64,
            "url": at(t, "url")?.clone()
        },
        "checks": checks_out,
        "reviews": reviews,
        "actions": [
            {"title": "Open in Browser", "shortcut": "↵", "exec": openurl(url)?},
            {"title": "Show Diff",
             "exec": format!("'{TUI}' gh pr diff --repo {repo_q} {num}")},
            {"title": "Watch Checks",
             "exec": format!("'{TUI}' gh pr checks --repo {repo_q} {num} --watch")},
            {"title": "Check Out Locally",
             "exec": format!("'{TUI}' gh pr checkout --repo {repo_q} {num}")},
            {"title": "Workflow Runs", "exec": "true", "query": format!("ci:{slug}")},
            {"title": "Copy URL", "exec": copyurl(url)?}
        ]
    }))
}

/// `render_repo_panel` — the `ghrepo` view, or the "not found" row.
pub(crate) fn repo_panel(body: &str, slug: &str, now: i64) -> Vec<Value> {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    repo_panel_row(&v, slug, now).into_iter().collect()
}

fn repo_panel_row(v: &Value, slug: &str, now: i64) -> Option<Value> {
    // Same rule as the pull panel: a scalar `repository` kills the emit.
    let r = at(at(v, "data")?, "repository")?;
    if matches!(r, Value::Null | Value::Bool(false)) {
        let url = format!("https://github.com/{slug}");
        return Some(json!({
            "id": format!("gh:{slug}"),
            "title": slug,
            "subtitle": "not found, or no access",
            "detail": "a private repository needs the repo scope",
            "score": 90000,
            "exec": openurl(&Value::String(url))?
        }));
    }
    let empty_obj = json!({});
    let br = alt(at(r, "defaultBranchRef"), &empty_obj);
    let head = alt(at(br, "target"), &empty_obj);
    let nwo = sc(at(r, "nameWithOwner"))?;
    let url = at(r, "url")?;
    // `$r.url + "/issues"` — the `+` still dies on a non-string, so the
    // Issues action keeps its own read.
    let issues_url = format!("{}/issues", sc(Some(url))?);

    let mut prs = Vec::new();
    for p in list(at(at(r, "openPrs")?, "nodes")?)? {
        prs.push(json!({
            "number": at(p, "number")?.clone(),
            "title": at(p, "title")?.clone(),
            "author": emit_or_empty(at(at(p, "author")?, "login")),
            "draft": at(p, "isDraft")? == &Value::Bool(true),
            "mark": mark(&prstate(p)?)?,
            "review": revword(at(p, "reviewDecision")?),
            "age": ago(at(p, "updatedAt")?, now)
        }));
    }

    Some(json!({
        "id": format!("gh:{nwo}"),
        "view": "ghrepo",
        "title": nwo,
        "subtitle": emit_or_empty(at(r, "description")),
        "exec": openurl(url)?,
        "score": 90000,
        "repo": {
            "slug": nwo,
            "description": emit_or_empty(at(r, "description")),
            "language": emit_or_empty(at(at(r, "primaryLanguage")?, "name")),
            "stars": alt(at(r, "stargazerCount"), &json!(0)).clone(),
            "forks": alt(at(r, "forkCount"), &json!(0)).clone(),
            "private": at(r, "isPrivate")? == &Value::Bool(true),
            "archived": at(r, "isArchived")? == &Value::Bool(true),
            "fork": at(r, "isFork")? == &Value::Bool(true),
            "branch": emit_or_empty(at(br, "name")),
            "headline": emit_or_empty(at(head, "messageHeadline")),
            "headHash": emit_or_empty(at(head, "abbreviatedOid")),
            "headAge": ago(at(head, "committedDate")?, now),
            "headMark": mark(at(at(head, "statusCheckRollup")?, "state")?)?,
            "prs": alt(at(at(r, "pullRequests")?, "totalCount"), &json!(0)).clone(),
            "issues": alt(at(at(r, "issues")?, "totalCount"), &json!(0)).clone(),
            "release": emit_or_empty(at(at(r, "latestRelease")?, "tagName")),
            "releaseAge": ago(at(at(r, "latestRelease")?, "publishedAt")?, now),
            "pushed": ago(at(r, "pushedAt")?, now),
            "url": at(r, "url")?.clone()
        },
        "prs": prs,
        "actions": [
            {"title": "Open in Browser", "shortcut": "↵", "exec": openurl(url)?},
            {"title": "Pull Requests", "exec": "true", "query": format!("pr:{nwo}")},
            {"title": "Workflow Runs", "exec": "true", "query": format!("ci:{nwo}")},
            {"title": "Issues", "exec": openurl(&Value::String(issues_url))?},
            {"title": "Clone", "exec": format!("'{TUI}' gh repo clone {}", quote(&nwo))},
            {"title": "Copy URL", "exec": copyurl(url)?}
        ]
    }))
}
