//! The list emitters — own/found repos, repo and inbox PRs, repo and
//! inbox issues, workflow runs — plus the `gh search` reshapes that
//! turn flat search output into the GraphQL nesting they read.

use super::jq::*;
use super::rows::prrow;

// ---------------------------------------------------------------- lists

/// `.[]` — the array's elements, or an object's values; a scalar is the
/// death the whole emit dies on.
fn each(v: &Value) -> Option<Vec<&Value>> {
    match v {
        Value::Array(a) => Some(a.iter().collect()),
        Value::Object(m) => Some(m.values().collect()),
        _ => None,
    }
}

/// `gh:`'s own-repos emit — `gh repo list` filtered locally, newest push
/// first, twelve rows of "Your Repos".
pub(crate) fn own_repos_rows(body: &str, q: &str, now: i64) -> Vec<Value> {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    own_repos(&v, q, now).unwrap_or_default()
}

fn own_repos(v: &Value, q: &str, now: i64) -> Option<Vec<Value>> {
    let mut kept: Vec<&Value> = Vec::new();
    for item in each(v)? {
        // `select` short-circuits on an empty q, so the haystack's type
        // errors only kill a narrowed query — but `"own:" + nameWithOwner`
        // below dies either way.
        if !q.is_empty() {
            let hay = format!(
                "{} {}",
                sc(at(item, "nameWithOwner"))?,
                so(at(item, "description"))?
            )
            .to_lowercase();
            if !hay.contains(q) {
                continue;
            }
        }
        kept.push(item);
    }
    // `sort_by(.pushedAt) | reverse` — jq's order puts null first
    // ascending, so the never-pushed sort last here.
    kept.sort_by_key(|i| match at(i, "pushedAt").unwrap_or(&NULL) {
        Value::Null => (0, String::new()),
        other => (1, tostring(other)),
    });
    kept.reverse();
    kept.truncate(12);

    let mut out = Vec::with_capacity(kept.len());
    for (i, item) in kept.iter().enumerate() {
        let nwo = sc(at(item, "nameWithOwner"))?;
        let url = at(item, "url")?;
        let subtitle = join_ne(
            vec![
                emit_or_empty(at(at(item, "primaryLanguage")?, "name")),
                Value::String(if truthy(at(item, "isPrivate")?) {
                    "private".to_string()
                } else {
                    String::new()
                }),
                Value::String(if truthy(at(item, "isArchived")?) {
                    "archived".to_string()
                } else {
                    String::new()
                }),
            ],
            "  ",
        )?;
        out.push(json!({
            "id": format!("own:{nwo}"),
            "title": at(item, "nameWithOwner")?.clone(),
            "detail": emit_or_empty(at(item, "description")),
            "subtitle": subtitle,
            "accessory": format!("★ {}  {}", tostring(at(item, "stargazerCount")?),
                                 ago(at(item, "pushedAt")?, now)),
            "group": "Your Repos",
            "exec": openurl(url)?,
            "score": 90000 - i as i64 * 100,
            "actions": [
                {"title": "Open in Browser", "shortcut": "↵", "exec": openurl(url)?},
                {"title": "This Repository", "exec": "true", "query": format!("gh:{nwo}")},
                {"title": "Pull Requests", "exec": "true", "query": format!("pr:{nwo}")},
                {"title": "Clone", "exec": format!("'{TUI}' gh repo clone {}", quote(&nwo))},
                {"title": "Copy URL", "exec": copyurl(url)?}
            ]
        }));
    }
    Some(out)
}

/// `gh:`'s "On GitHub" emit — `gh search repos`, minus the names already
/// in the own list, six rows.
pub(crate) fn found_repos_rows(body: &str, seen: &[Value], now: i64) -> Vec<Value> {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    found_repos(&v, seen, now).unwrap_or_default()
}

fn found_repos(v: &Value, seen: &[Value], now: i64) -> Option<Vec<Value>> {
    let mut out = Vec::new();
    for item in each(v)? {
        // `($seen | index(.fullName)) == null` — a name already yours is
        // not a discovery.
        let name = at(item, "fullName")?;
        if seen.iter().any(|s| s == name) {
            continue;
        }
        if out.len() == 6 {
            break;
        }
        let name = sc(Some(name))?;
        let url = at(item, "url")?;
        out.push(json!({
            "id": format!("found:{name}"),
            "title": at(item, "fullName")?.clone(),
            "detail": emit_or_empty(at(item, "description")),
            "subtitle": emit_or_empty(at(item, "language")),
            "accessory": format!("★ {}  {}", tostring(at(item, "stargazersCount")?),
                                 ago(at(item, "updatedAt")?, now)),
            "group": "On GitHub",
            "exec": openurl(url)?,
            "score": 80000 - out.len() as i64 * 100,
            "actions": [
                {"title": "Open in Browser", "shortcut": "↵", "exec": openurl(url)?},
                {"title": "This Repository", "exec": "true", "query": format!("gh:{name}")},
                {"title": "Clone", "exec": format!("'{TUI}' gh repo clone {}", quote(&name))},
                {"title": "Copy URL", "exec": copyurl(url)?}
            ]
        }));
    }
    Some(out)
}

/// The `keep` the repo-PR emit filters with — title, author, branch and
/// number against the lowered text.
fn keep_pr(p: &Value, q: &str) -> Option<bool> {
    if q.is_empty() {
        return Some(true);
    }
    let hay = format!(
        "{} {} {} #{}",
        so(at(p, "title"))?,
        so(at(at(p, "author")?, "login"))?,
        so(at(p, "headRefName"))?,
        tostring(at(p, "number")?)
    )
    .to_lowercase();
    Some(hay.contains(q))
}

/// `pr:` on a repository — the open-PRs emit (over graphql nodes or the
/// `gh search` reshape; both arrive nested the same way).
pub(crate) fn repo_prs_rows(body: &str, slug: &str, q: &str, now: i64) -> Vec<Value> {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    repo_prs(&v, slug, q, now).unwrap_or_default()
}

fn repo_prs(v: &Value, slug: &str, q: &str, now: i64) -> Option<Vec<Value>> {
    let nodes = list(at(
        at(at(at(v, "data")?, "repository")?, "pullRequests")?,
        "nodes",
    )?)?;
    let group = format!("Open Pull Requests  {slug}");
    let mut out = Vec::new();
    for p in nodes {
        if !keep_pr(p, q)? {
            continue;
        }
        if out.len() == 14 {
            break;
        }
        out.push(prrow(p, &group, 90000 - out.len() as i64 * 100, slug, now)?);
    }
    Some(out)
}

/// `pr:`'s inbox — "Waiting On You" ahead of "Your Pull Requests".
pub(crate) fn mine_prs_rows(body: &str, q: &str, now: i64) -> Vec<Value> {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    mine_prs(&v, q, now).unwrap_or_default()
}

fn mine_prs(v: &Value, q: &str, now: i64) -> Option<Vec<Value>> {
    // `keep` here adds the repository name — the inbox needs it.
    let keep = |p: &Value| -> Option<bool> {
        if q.is_empty() {
            return Some(true);
        }
        let hay = format!(
            "{} {} {} {} #{}",
            so(at(p, "title"))?,
            so(at(at(p, "repository")?, "nameWithOwner"))?,
            so(at(at(p, "author")?, "login"))?,
            so(at(p, "headRefName"))?,
            tostring(at(p, "number")?)
        )
        .to_lowercase();
        Some(hay.contains(q))
    };
    let data = at(v, "data")?;
    let review = list(at(at(data, "review")?, "nodes")?)?;
    let mine = list(at(at(data, "mine")?, "nodes")?)?;
    let mut all: Vec<(&Value, &str, i64)> = Vec::new();
    for p in review {
        if keep(p)? {
            all.push((p, "Waiting On You", 95000));
        }
    }
    for p in mine {
        if keep(p)? {
            all.push((p, "Your Pull Requests", 90000));
        }
    }
    let mut out = Vec::new();
    for (i, (p, g, b)) in all.iter().enumerate() {
        out.push(prrow(p, g, b - i as i64 * 100, "", now)?);
    }
    Some(out)
}

/// The issue-list emit `pr:`'s repo mode and `issue:`'s share — the label
/// join, the comment-count badge, the "This Repository" action.
fn issue_list_row(n: &Value, repo: &str, group: &str, score: i64, now: i64) -> Option<Value> {
    let num = tostring(at(n, "number")?);
    let labels = join(&names(at(at(n, "labels")?, "nodes"))?, ", ")?;
    let comments = alt(at(at(n, "comments")?, "totalCount"), &NULL);
    let acc = format!(
        "{}{}",
        if over_zero(comments) {
            format!("{}󰆉  ", tostring(comments))
        } else {
            String::new()
        },
        ago(at(n, "updatedAt")?, now)
    );
    let url = at(n, "url")?;
    Some(json!({
        "id": format!("issue:{repo}#{num}"),
        "title": at(n, "title")?.clone(),
        "detail": format!("{repo}  #{num}  {}", so(at(at(n, "author")?, "login"))?),
        "subtitle": labels,
        "accessory": acc,
        "group": group,
        "exec": openurl(url)?,
        "score": score,
        "actions": [
            {"title": "Open in Browser", "shortcut": "↵", "exec": openurl(url)?},
            {"title": "This Repository", "exec": "true", "query": format!("gh:{repo}")},
            {"title": "Copy URL", "exec": copyurl(url)?}
        ]
    }))
}

/// `issue:` on a repository — the open-issues emit.
pub(crate) fn repo_issues_rows(body: &str, slug: &str, q: &str, now: i64) -> Vec<Value> {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    repo_issues(&v, slug, q, now).unwrap_or_default()
}

fn repo_issues(v: &Value, slug: &str, q: &str, now: i64) -> Option<Vec<Value>> {
    let nodes = list(at(
        at(at(at(v, "data")?, "repository")?, "issues")?,
        "nodes",
    )?)?;
    let group = format!("Open Issues  {slug}");
    let mut out = Vec::new();
    for n in nodes {
        if !q.is_empty() {
            // `keep` reads the label names too — `issue:repo bug` finds the
            // issue whose title never said "bug".
            let hay = format!(
                "{} {} {} #{}",
                so(at(n, "title"))?,
                so(at(at(n, "author")?, "login"))?,
                join(&names(at(at(n, "labels")?, "nodes"))?, " ")?,
                tostring(at(n, "number")?)
            )
            .to_lowercase();
            if !hay.contains(q) {
                continue;
            }
        }
        if out.len() == 14 {
            break;
        }
        out.push(issue_list_row(
            n,
            slug,
            &group,
            90000 - out.len() as i64 * 100,
            now,
        )?);
    }
    Some(out)
}

/// `issue:`'s inbox — assigned ahead of mentioning, deduped by url.
pub(crate) fn my_issues_rows(body: &str, q: &str, now: i64) -> Vec<Value> {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    my_issues(&v, q, now).unwrap_or_default()
}

fn my_issues(v: &Value, q: &str, now: i64) -> Option<Vec<Value>> {
    let keep = |n: &Value| -> Option<bool> {
        if q.is_empty() {
            return Some(true);
        }
        let hay = format!(
            "{} {} #{}",
            so(at(n, "title"))?,
            so(at(at(n, "repository")?, "nameWithOwner"))?,
            tostring(at(n, "number")?)
        )
        .to_lowercase();
        Some(hay.contains(q))
    };
    let data = at(v, "data")?;
    let mut all: Vec<(&Value, &str, i64)> = Vec::new();
    for n in list(at(at(data, "assigned")?, "nodes")?)? {
        if keep(n)? {
            all.push((n, "Assigned To You", 95000));
        }
    }
    for n in list(at(at(data, "mentioned")?, "nodes")?)? {
        if keep(n)? {
            all.push((n, "Mentioning You", 88000));
        }
    }
    // `unique_by(.url)` — sorted by it, first of each run kept. jq's order
    // puts null before everything, so the sort key does too.
    let key = |v: &Value| -> (u8, String) {
        match v {
            Value::Null => (0, String::new()),
            Value::Bool(_) | Value::Number(_) | Value::String(_) => (1, tostring(v)),
            _ => (2, String::new()),
        }
    };
    all.sort_by(|a, b| {
        key(at(a.0, "url").unwrap_or(&NULL)).cmp(&key(at(b.0, "url").unwrap_or(&NULL)))
    });
    all.dedup_by(|a, b| at(a.0, "url") == at(b.0, "url"));
    let mut out = Vec::new();
    for (i, (n, g, b)) in all.iter().enumerate() {
        let repo = so(at(at(n, "repository")?, "nameWithOwner"))?;
        out.push(issue_list_row(n, &repo, g, b - i as i64 * 100, now)?);
    }
    Some(out)
}

/// `ci:`'s runs — failures first, then the ones still running.
pub(crate) fn run_rows(body: &str, slug: &str, now: i64) -> Vec<Value> {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    runs(&v, slug, now).unwrap_or_default()
}

fn runs(v: &Value, slug: &str, now: i64) -> Option<Vec<Value>> {
    let group = format!("Workflow Runs  {slug}");
    let mut out = Vec::new();
    for r in each(v)? {
        if out.len() == 14 {
            break;
        }
        // `if .status != "completed" then "●" else (.conclusion | mark)`
        let m = if at(r, "status")?.as_str() != Some("completed") {
            "●"
        } else {
            mark(at(r, "conclusion")?)?
        };
        let id = tostring(at(r, "databaseId")?);
        let url = at(r, "url")?;
        // `if (.conclusion // "") == "" then .status else .conclusion end`
        // — the else emits the raw conclusion, not the normalized one.
        let conc_raw = at(r, "conclusion")?;
        let subtitle = if matches!(conc_raw, Value::Null | Value::Bool(false))
            || conc_raw.as_str() == Some("")
        {
            at(r, "status")?.clone()
        } else {
            conc_raw.clone()
        };
        // Bare `+` fields: null vanishes, a scalar that is neither dies.
        let detail = format!(
            "{}  {}  {}",
            sc(at(r, "workflowName"))?,
            sc(at(r, "headBranch"))?,
            sc(at(r, "event"))?
        );
        out.push(json!({
            "id": format!("run:{slug}:{id}"),
            "title": at(r, "displayTitle")?.clone(),
            "detail": detail,
            "subtitle": subtitle,
            "accessory": accessory(m, ago(at(r, "createdAt")?, now)),
            "group": group,
            "exec": openurl(url)?,
            "score": (match m { "✗" => 95000, "●" => 92000, _ => 90000 }) - out.len() as i64 * 100,
            "actions": [
                {"title": "Open in Browser", "shortcut": "↵", "exec": openurl(url)?},
                {"title": "Failed Logs",
                 "exec": format!("'{TUI}' gh run view --repo {} {id} --log-failed", quote(slug))},
                {"title": "Watch",
                 "exec": format!("'{TUI}' gh run watch --repo {} {id}", quote(slug))},
                {"title": "This Repository", "exec": "true", "query": format!("gh:{slug}")},
                {"title": "Copy URL", "exec": copyurl(url)?}
            ]
        }));
    }
    Some(out)
}

// ---------------------------------------------------------------- reshapes

/// The `gh search prs` flat array → the `pullRequests.nodes` shape the
/// emit reads. `None` is the script's `|| return` on a non-array.
pub(crate) fn reshape_search_prs(body: &str, slug: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    let arr = v.as_array()?;
    let mut nodes = Vec::with_capacity(arr.len());
    for p in arr {
        // `.author.login // ""` dies on a scalar author, like jq.
        let login = alt(at(at(p, "author")?, "login"), &NULL).clone();
        nodes.push(json!({
            "number": at(p, "number")?.clone(),
            "title": at(p, "title")?.clone(),
            "url": at(p, "url")?.clone(),
            "updatedAt": at(p, "updatedAt")?.clone(),
            "isDraft": alt(at(p, "isDraft"), &json!(false)).clone(),
            "additions": 0, "deletions": 0, "changedFiles": 0,
            "repository": {"nameWithOwner": slug},
            "headRefName": "",
            "reviewDecision": Value::Null,
            "author": {"login": login},
            "commits": {"nodes": []}
        }));
    }
    serde_json::to_string(&json!({
        "data": {"repository": {"nameWithOwner": slug,
                                "pullRequests": {"nodes": nodes}}}
    }))
    .ok()
}

/// The `gh search issues` equivalent, into `issues.nodes`.
pub(crate) fn reshape_search_issues(body: &str, slug: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    let arr = v.as_array()?;
    let mut nodes = Vec::with_capacity(arr.len());
    for n in arr {
        let login = alt(at(at(n, "author")?, "login"), &NULL).clone();
        let mut labels = Vec::new();
        for l in list(at(n, "labels")?)? {
            labels.push(json!({"name": at(l, "name")?.clone()}));
        }
        nodes.push(json!({
            "number": at(n, "number")?.clone(),
            "title": at(n, "title")?.clone(),
            "url": at(n, "url")?.clone(),
            "updatedAt": at(n, "updatedAt")?.clone(),
            "repository": {"nameWithOwner": slug},
            "author": {"login": login},
            "comments": {"totalCount": alt(at(n, "commentsCount"), &json!(0)).clone()},
            "labels": {"nodes": labels}
        }));
    }
    serde_json::to_string(&json!({
        "data": {"repository": {"nameWithOwner": slug,
                                "issues": {"nodes": nodes}}}
    }))
    .ok()
}
