//! `bin/oxy-gh` — the GitHub drawer — in Rust. The script's commentary
//! about the offline fast path is the whole reason this exists: a typed
//! `owner/repo`, a pasted GitHub URL or an `owner/repo#123` is already a
//! complete answer, so those rows come out before any request is even
//! considered.
//!
//! Everything else still goes through `gh` — the CLI is the source of
//! truth — but where the script spent one process per `jq` emit, the jq
//! programs are reimplemented in `render.rs` against serde_json, so a
//! warm cache answers with zero subprocesses. The cache itself is the
//! script's, byte for byte: `$XDG_STATE_HOME/omarchy/oxy-gh/<fp>-<md5>.json`,
//! fingerprinted by the gh credentials, capped at 200 entries, refreshed
//! behind the lock files the script's `tried_recently` reads. Either leg
//! answers the other.
//!
//! Four manifests land here through `native`: `gh`, `gh-pr`, `gh-issue`,
//! `gh-ci` — the mode the script read from `OXY_GH_MODE`, carried in the
//! name because `Ctx` has no extension id.

mod cache;
mod gql;
mod md5;
mod render;
#[cfg(test)]
mod tests;

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::native::util::on_path;
use crate::provider::process;
use crate::provider::{Ctx, NativeExt, NativeOutcome};

use cache::{Session, key_for};

/// `OXY_GH_MODE`, as a constructor argument.
#[derive(Clone, Copy)]
enum Mode {
    Repo,
    Prs,
    Issues,
    Runs,
}

/// The provider — one instance per manifest name.
pub struct Gh {
    mode: Mode,
}

impl Gh {
    /// `"gh"` — repositories and one numbered pull request/issue.
    pub fn repos() -> Self {
        Self { mode: Mode::Repo }
    }
    /// `"gh-pr"` — the PR inbox, a repo's open PRs, one numbered PR.
    pub fn prs() -> Self {
        Self { mode: Mode::Prs }
    }
    /// `"gh-issue"` — the issue inbox, a repo's open issues, one number.
    pub fn issues() -> Self {
        Self { mode: Mode::Issues }
    }
    /// `"gh-ci"` — workflow runs for a repository.
    pub fn runs() -> Self {
        Self { mode: Mode::Runs }
    }
}

/// `T_KIND` — what `parse_target` decided the text is.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
enum Kind {
    Empty,
    #[default]
    Text,
    Repo,
    Pr,
    Issue,
    /// `owner/repo N` or `owner/repo #N` — the space-separated spellings.
    Number,
}

/// `T_SLUG`/`T_NUM`/`T_KIND`/`T_TEXT` — the fields the script's
/// `parse_target` exports. `text` is the whole query under `text`, the
/// words after a space under `repo` (the search-terms slot), "" elsewhere.
#[derive(Default)]
struct Target {
    slug: String,
    num: String,
    kind: Kind,
    text: String,
}

/// `[A-Za-z0-9._-]+` — both sides of the slug use the script's one loose
/// pattern: it is `[[ $owner =~ ... && $name =~ ... ]]`, not GitHub's login
/// rules, so `a.b_c` is a slug here whether GitHub would allow it or not.
fn slug_chars(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `parse_target`. The script reads the query in stages — a leading space,
/// a URL's dressings, then `owner/name` plus whatever followed — and this
/// is those stages in order, `${var#pat}`/`${var%%pat}` for `${var#pat}`.
/// The important part is what it does *not* do: it never invents a slug.
/// Text that is not a slug stays text.
fn parse_target(q_in: &str) -> Target {
    let mut t = Target::default();
    let q = q_in.strip_prefix(' ').unwrap_or(q_in); // "${1# }" — one space
    t.text = q.to_string();
    if q.is_empty() {
        t.kind = Kind::Empty;
        t.text.clear();
        return t;
    }

    // A URL is a slug wearing a hostname: `<…>` and the scheme come off,
    // `www.` does, `github.com/` does with its `?`/`#` tail, and one
    // trailing `/` does. Case-sensitive, like the script's `==`.
    let mut u = q.strip_prefix('<').unwrap_or(q);
    u = u.strip_suffix('>').unwrap_or(u);
    u = u.strip_prefix("http://").unwrap_or(u);
    u = u.strip_prefix("https://").unwrap_or(u);
    u = u.strip_prefix("www.").unwrap_or(u);
    if let Some(rest) = u.strip_prefix("github.com/") {
        u = rest.split('?').next().unwrap_or_default();
        u = u.split('#').next().unwrap_or_default();
    }
    let u = u.strip_suffix('/').unwrap_or(u);

    // owner/name, and whatever came after it.
    let Some((owner, rest)) = u.split_once('/') else {
        return t; // no slash: text stays text
    };
    let name_end = rest.find(['/', ' ', '#']).unwrap_or(rest.len());
    let name_raw = &rest[..name_end];
    let name = name_raw.strip_suffix(".git").unwrap_or(name_raw);
    if owner.is_empty() || name.is_empty() {
        return t;
    }
    if !slug_chars(owner) || !slug_chars(name) {
        return t;
    }

    t.slug = format!("{owner}/{name}");
    t.kind = Kind::Repo;
    // `rest` re-reads: the name went, a `.git` left behind goes, and what
    // remains decides the kind.
    let rest = &rest[name_raw.len()..];
    let rest = rest.strip_prefix(".git").unwrap_or(rest);

    if rest.starts_with("/pull/") || rest.starts_with("/pulls/") {
        t.num = rest.rsplit('/').next().unwrap_or_default().to_string();
        t.kind = Kind::Pr;
    } else if rest.starts_with("/issues/") {
        t.num = rest.rsplit('/').next().unwrap_or_default().to_string();
        t.kind = Kind::Issue;
    } else if let Some(r) = rest.strip_prefix('#') {
        t.num = r.split(' ').next().unwrap_or_default().to_string();
        t.kind = Kind::Number;
    } else if let Some(r) = rest.strip_prefix(' ') {
        // `owner/repo words`: the first word names a number when it is
        // one, and the rest is search text when it is not.
        let word = r.split(' ').next().unwrap_or_default();
        if let Some(w) = word.strip_prefix('#') {
            t.num = w.to_string();
            t.kind = Kind::Number;
        } else if !word.is_empty() && word.bytes().all(|b| b.is_ascii_digit()) {
            t.num = word.to_string();
            t.kind = Kind::Number;
        }
        t.text = r.to_string();
        if !t.num.is_empty() {
            t.text.clear();
        }
        return t; // the script returns here: no digit validation below
    }

    // `T_NUM="${T_NUM%%[!0-9]*}"` — a number ends at its first non-digit,
    // and one that was never digits is no number at all.
    if let Some(i) = t.num.bytes().position(|b| !b.is_ascii_digit()) {
        t.num.truncate(i);
    }
    if t.num.is_empty() && t.kind != Kind::Repo {
        t.kind = Kind::Repo;
    }
    t.text.clear();
    t
}

/// The script's `timeout 3 gh auth token >/dev/null 2>&1` — keyring-local,
/// never a request, so asking it on every keystroke is free.
async fn authed(s: &Session) -> bool {
    if s.offline {
        return false;
    }
    process::run(
        "timeout 3 gh auth token >/dev/null 2>&1",
        Duration::from_secs(5),
    )
    .await
    .is_some_and(|f| f.code == Some(0))
}

/// `authed || { signin_row; return; }` — signin_row re-runs the token read
/// itself, so the row knows which of the two failures it is.
async fn signin(s: &Session) -> Value {
    if s.offline {
        render::signin_row(false)
    } else {
        render::signin_row(authed(s).await)
    }
}

/// `emit` produced rows or jq died silent — `Empty` is both.
fn out(rows: Vec<Value>) -> NativeOutcome {
    if rows.is_empty() {
        NativeOutcome::Empty
    } else {
        NativeOutcome::Rows(rows)
    }
}

/// `pr:`/`gh:`/`ci:`'s numbered path — peek the panel, skeleton while it
/// warms. `--argjson` is where the script dies on a non-number; here that
/// is `Empty` (and no doomed request is fired, which is the one place the
/// port is *less* eager than the script).
async fn one_pr(s: &Session, t: &Target) -> NativeOutcome {
    let Ok(num) = t.num.parse::<i64>() else {
        return NativeOutcome::Empty;
    };
    let key = format!("pr-{}", key_for(&format!("{}#{}", t.slug, t.num)));
    match cache::peek(s, &key, 30, &gql::pr_cmd(&t.slug, &t.num)).await {
        Some(body) => out(render::pr_panel(&body, &t.slug, s.now)),
        None => NativeOutcome::Rows(vec![render::pr_skeleton(&t.slug, num)]),
    }
}

/// `gh:` — the repository panel, or the own/search lists.
async fn mode_repo(s: &Session, t: &Target) -> NativeOutcome {
    match t.kind {
        Kind::Pr | Kind::Issue | Kind::Number => one_pr(s, t).await,
        Kind::Repo => {
            let key = format!("repo-{}", key_for(&t.slug));
            match cache::peek(s, &key, 60, &gql::repo_cmd(&t.slug)).await {
                Some(body) => out(render::repo_panel(&body, &t.slug, s.now)),
                None => NativeOutcome::Rows(vec![render::repo_skeleton(&t.slug)]),
            }
        }
        _ => {
            if !authed(s).await {
                return NativeOutcome::Rows(vec![signin(s).await]);
            }
            let own = cache::pull(s, "own-repos", 120, &gql::own_repos_cmd())
                .await
                .unwrap_or_else(|| "[]".to_string());
            let mut rows = render::own_repos_rows(&own, &t.text.to_lowercase(), s.now);
            if t.text.chars().count() >= 3 {
                // `${#T_TEXT} -ge 3`: below that a search term is not a
                // search term, and every keystroke is answered from the
                // list above for no request at all.
                let seen = seen_list(&own);
                let key = format!("search-{}", key_for(&t.text));
                if let Some(found) =
                    cache::peek(s, &key, 300, &gql::search_repos_cmd(&t.text)).await
                {
                    rows.extend(render::found_repos_rows(&found, &seen, s.now));
                }
            }
            out(rows)
        }
    }
}

/// `pr:` — numbered PR, a repo's open PRs (or a search of them), or the
/// inbox.
async fn mode_prs(s: &Session, t: &Target) -> NativeOutcome {
    if matches!(t.kind, Kind::Pr | Kind::Number | Kind::Issue) {
        return one_pr(s, t).await;
    }
    if !authed(s).await {
        return NativeOutcome::Rows(vec![signin(s).await]);
    }
    if t.kind == Kind::Repo {
        let body = if !t.text.is_empty() {
            // A word after the repository searches the repository, rather
            // than filtering the thirty most recently updated.
            // `gh search prs` — not `search issues`, which adds `is:issue`
            // and would return every pull request never.
            let key = format!(
                "reposearchprs-{}",
                key_for(&format!("{} {}", t.slug, t.text))
            );
            match cache::pull(s, &key, 45, &gql::search_prs_cmd(&t.slug, &t.text)).await {
                // The flat array is reshaped into the graphql nesting so
                // one emit serves both paths.
                Some(raw) => render::reshape_search_prs(&raw, &t.slug),
                None => None,
            }
        } else {
            cache::pull(
                s,
                &format!("repoprs-{}", key_for(&t.slug)),
                45,
                &gql::repo_prs_cmd(&t.slug),
            )
            .await
        };
        let Some(body) = body else {
            return NativeOutcome::Empty;
        };
        return out(render::repo_prs_rows(
            &body,
            &t.slug,
            &t.text.to_lowercase(),
            s.now,
        ));
    }

    // A bare `pr:`, or words: the inbox, filtered.
    let Some(body) = cache::pull(s, "mine-prs", 45, &gql::mine_prs_cmd()).await else {
        return NativeOutcome::Empty;
    };
    let rows = render::mine_prs_rows(&body, &t.text.to_lowercase(), s.now);
    if rows.is_empty() && t.text.is_empty() {
        // An empty inbox is an answer, and a good one. The launcher's own
        // "Nothing matches" is not: it reads as a search that failed.
        return NativeOutcome::Rows(vec![render::prs_empty()]);
    }
    out(rows)
}

/// `issue:` — numbered issue, a repo's open issues (or a search of them),
/// or the assigned/mentioned inbox.
async fn mode_issues(s: &Session, t: &Target) -> NativeOutcome {
    if !t.num.is_empty() {
        // A repository and a number is a URL. GitHub sends /issues/N to
        // the pull request when it turns out to be one, so this is right
        // without asking.
        let Ok(num) = t.num.parse::<i64>() else {
            return NativeOutcome::Empty;
        };
        return NativeOutcome::Rows(vec![render::issue_row(&t.slug, num)]);
    }
    if !authed(s).await {
        return NativeOutcome::Rows(vec![signin(s).await]);
    }
    if t.kind == Kind::Repo {
        let body = if !t.text.is_empty() {
            let key = format!(
                "repoissuesearch-{}",
                key_for(&format!("{} {}", t.slug, t.text))
            );
            match cache::pull(s, &key, 45, &gql::search_issues_cmd(&t.slug, &t.text)).await {
                Some(raw) => render::reshape_search_issues(&raw, &t.slug),
                None => None,
            }
        } else {
            cache::pull(
                s,
                &format!("repoissues-{}", key_for(&t.slug)),
                45,
                &gql::repo_issues_cmd(&t.slug),
            )
            .await
        };
        let Some(body) = body else {
            return NativeOutcome::Empty;
        };
        return out(render::repo_issues_rows(
            &body,
            &t.slug,
            &t.text.to_lowercase(),
            s.now,
        ));
    }
    let Some(body) = cache::pull(s, "my-issues", 60, &gql::my_issues_cmd()).await else {
        return NativeOutcome::Empty;
    };
    let rows = render::my_issues_rows(&body, &t.text.to_lowercase(), s.now);
    if rows.is_empty() && t.text.is_empty() {
        return NativeOutcome::Rows(vec![render::issues_empty()]);
    }
    out(rows)
}

/// `ci:` — workflow runs; a slug-less query draws the hint row before any
/// auth check, same as the script.
async fn mode_runs(s: &Session, t: &Target) -> NativeOutcome {
    if t.slug.is_empty() {
        return NativeOutcome::Rows(vec![render::ci_hint()]);
    }
    if !authed(s).await {
        return NativeOutcome::Rows(vec![signin(s).await]);
    }
    let Some(body) = cache::pull(
        s,
        &format!("runs-{}", key_for(&t.slug)),
        30,
        &gql::run_list_cmd(&t.slug),
    )
    .await
    else {
        return NativeOutcome::Empty;
    };
    out(render::run_rows(&body, &t.slug, s.now))
}

/// `[ .[].nameWithOwner ]` over the own-repos body — the `seen` list the
/// found-emit subtracts. A scalar member is the jq error the script's
/// `|| seen="[]"` catches.
fn seen_list(own: &str) -> Vec<Value> {
    let Ok(v) = serde_json::from_str::<Value>(own) else {
        return Vec::new();
    };
    let items: Vec<&Value> = match &v {
        Value::Array(a) => a.iter().collect(),
        Value::Object(m) => m.values().collect(),
        _ => return Vec::new(),
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item.get("nameWithOwner") {
            Some(v) => out.push(v.clone()),
            None if item.is_object() || item.is_null() => out.push(Value::Null),
            None => return Vec::new(),
        }
    }
    out
}

impl NativeExt for Gh {
    /// The whole script, async. Everything that can be answered without a
    /// request is answered before the first `gh` is even spawned; the only
    /// blocking call in the file is `pull`, and only for the questions the
    /// text cannot answer.
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        let mode = self.mode;
        let mut query = ctx.arg.clone();
        Box::pin(async move {
            // The manifest's `when`, re-checked like every native: no `gh`
            // and the script leg owns the query outright.
            if !on_path("gh") || !on_path("jq") {
                return NativeOutcome::Fallback;
            }

            // `!offline` in the text, `OXY_GH_OFFLINE` in the env — the
            // two ways to pin the drawer to what it already knows. `cold`
            // is the prefix's second half: read nothing a request left.
            let mut offline = std::env::var_os("OXY_GH_OFFLINE").is_some_and(|v| !v.is_empty());
            let mut cold = false;
            if query == "!offline" || query.starts_with("!offline ") {
                offline = true;
                cold = true;
                let rest = &query["!offline".len()..];
                query = rest.strip_prefix(' ').unwrap_or(rest).to_string();
            }

            let session = Session {
                offline,
                cold,
                fp: cache::auth_fingerprint(),
                dir: cache::cache_dir(),
                now: cache::now_secs(),
            };
            cache::housekeeping(&session.dir, &session.fp);
            let t = parse_target(&query);
            match mode {
                Mode::Repo => mode_repo(&session, &t).await,
                Mode::Prs => mode_prs(&session, &t).await,
                Mode::Issues => mode_issues(&session, &t).await,
                Mode::Runs => mode_runs(&session, &t).await,
            }
        })
    }
}
