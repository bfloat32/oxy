//! The GraphQL documents `bin/oxy-gh` fires, verbatim — one request per
//! panel was the script's whole cost model (`gh api graphql` is 1 point of
//! the 5000/hour rate limit, where REST-per-fact would burn a dozen), so
//! they are kept byte-for-byte rather than reshaped. Each `*_cmd` builds
//! the shell line the cache layer runs; user text goes through `quote`.

use crate::support::quote::quote;

const GQL_REPO: &str = r#"
query($owner:String!, $name:String!) {
  repository(owner:$owner, name:$name) {
    nameWithOwner description url isPrivate isArchived isFork
    stargazerCount forkCount pushedAt
    primaryLanguage { name }
    defaultBranchRef { name target { ... on Commit {
      abbreviatedOid messageHeadline committedDate statusCheckRollup { state } } } }
    pullRequests(states: OPEN) { totalCount }
    issues(states: OPEN) { totalCount }
    latestRelease { tagName publishedAt }
    openPrs: pullRequests(states: OPEN, first: 8, orderBy: {field: UPDATED_AT, direction: DESC}) {
      nodes { number title url isDraft updatedAt author { login } reviewDecision
        commits(last:1){ nodes { commit { statusCheckRollup { state } } } } } }
  }
}"#;

const GQL_PR: &str = r#"
query($owner:String!, $name:String!, $number:Int!) {
  repository(owner:$owner, name:$name) {
    nameWithOwner
    issueOrPullRequest(number:$number) {
      __typename
      ... on Issue { number title url state createdAt updatedAt author { login }
        comments { totalCount } labels(first:8){ nodes { name } } }
      ... on PullRequest {
        number title url state isDraft createdAt updatedAt
        additions deletions changedFiles headRefName baseRefName mergeable
        author { login } reviewDecision comments { totalCount }
        reviews(last: 10) { nodes { state submittedAt author { login } } }
        commits(last: 1) { nodes { commit { statusCheckRollup {
          state contexts(first: 30) { nodes {
            __typename
            ... on CheckRun { name conclusion status startedAt completedAt detailsUrl }
            ... on StatusContext { context state targetUrl createdAt } } } } } } }
      }
    }
  }
}"#;

const GQL_MINE: &str = r#"
query {
  mine: search(query: "is:pr is:open author:@me archived:false", type: ISSUE, first: 20) {
    nodes { ... on PullRequest {
      number title url isDraft updatedAt additions deletions changedFiles
      repository { nameWithOwner } headRefName reviewDecision author { login }
      commits(last:1){ nodes { commit { statusCheckRollup { state } } } } } } }
  review: search(query: "is:pr is:open review-requested:@me archived:false", type: ISSUE, first: 15) {
    nodes { ... on PullRequest {
      number title url isDraft updatedAt additions deletions changedFiles
      repository { nameWithOwner } headRefName reviewDecision author { login }
      commits(last:1){ nodes { commit { statusCheckRollup { state } } } } } } }
}"#;

const GQL_REPO_PRS: &str = r#"
query($owner:String!, $name:String!) {
  repository(owner:$owner, name:$name) {
    nameWithOwner
    pullRequests(states: OPEN, first: 30, orderBy: {field: UPDATED_AT, direction: DESC}) {
      nodes { number title url isDraft updatedAt additions deletions changedFiles
        repository { nameWithOwner } headRefName reviewDecision author { login }
        commits(last:1){ nodes { commit { statusCheckRollup { state } } } } } }
  }
}"#;

const GQL_REPO_ISSUES: &str = r#"
query($owner:String!, $name:String!) {
  repository(owner:$owner, name:$name) {
    nameWithOwner
    issues(states: OPEN, first: 30, orderBy: {field: UPDATED_AT, direction: DESC}) {
      nodes { number title url updatedAt repository { nameWithOwner }
        author { login } comments { totalCount } labels(first:5){ nodes { name } } } }
  }
}"#;

const GQL_ISSUES: &str = r#"
query {
  assigned: search(query: "is:issue is:open assignee:@me archived:false", type: ISSUE, first: 20) {
    nodes { ... on Issue { number title url updatedAt repository { nameWithOwner }
      author { login } comments { totalCount } labels(first:5){ nodes { name } } } } }
  mentioned: search(query: "is:issue is:open mentions:@me archived:false", type: ISSUE, first: 15) {
    nodes { ... on Issue { number title url updatedAt repository { nameWithOwner }
      author { login } comments { totalCount } labels(first:5){ nodes { name } } } } }
}"#;

/// `owner/name` into the `-F` pair the documents take.
fn owner_name(slug: &str) -> (String, String) {
    let (owner, name) = slug.split_once('/').unwrap_or((slug, ""));
    (quote(owner), quote(name))
}

/// `gh api graphql -f query=<doc> -F owner -F name` — the two-variable call
/// every repository panel shares.
fn graphql(query: &str, slug: &str) -> String {
    let (owner, name) = owner_name(slug);
    format!(
        "gh api graphql -f {} -F owner={owner} -F name={name}",
        quote(&format!("query={query}"))
    )
}

/// `gh api graphql -f query=$GQL_PR -F owner -F name -F number`.
pub(crate) fn pr_cmd(slug: &str, num: &str) -> String {
    format!("{} -F number={}", graphql(GQL_PR, slug), quote(num))
}

/// `gh api graphql -f query=$GQL_REPO -F owner -F name`.
pub(crate) fn repo_cmd(slug: &str) -> String {
    graphql(GQL_REPO, slug)
}

/// `gh api graphql -f query=$GQL_REPO_PRS -F owner -F name`.
pub(crate) fn repo_prs_cmd(slug: &str) -> String {
    graphql(GQL_REPO_PRS, slug)
}

/// `gh api graphql -f query=$GQL_REPO_ISSUES -F owner -F name`.
pub(crate) fn repo_issues_cmd(slug: &str) -> String {
    graphql(GQL_REPO_ISSUES, slug)
}

/// `gh api graphql -f query=$GQL_MINE`.
pub(crate) fn mine_prs_cmd() -> String {
    format!("gh api graphql -f {}", quote(&format!("query={GQL_MINE}")))
}

/// `gh api graphql -f query=$GQL_ISSUES`.
pub(crate) fn my_issues_cmd() -> String {
    format!(
        "gh api graphql -f {}",
        quote(&format!("query={GQL_ISSUES}"))
    )
}

/// `gh repo list --limit 100 --json …`.
pub(crate) fn own_repos_cmd() -> String {
    "gh repo list --limit 100 \
     --json nameWithOwner,description,primaryLanguage,stargazerCount,pushedAt,isPrivate,isArchived,url"
        .to_string()
}

/// `gh search repos "<text>" --limit 12 --json …`.
pub(crate) fn search_repos_cmd(text: &str) -> String {
    format!(
        "gh search repos {} --limit 12 \
         --json fullName,description,language,stargazersCount,updatedAt,url",
        quote(text)
    )
}

/// `gh search prs --repo <slug> --state open --limit 30 --json … -- <text>`.
/// `search prs`, not `search issues` — the latter adds `is:issue` and a
/// pull request never matches it.
pub(crate) fn search_prs_cmd(slug: &str, text: &str) -> String {
    format!(
        "gh search prs --repo {} --state open --limit 30 \
         --json number,title,url,updatedAt,repository,author,isDraft -- {}",
        quote(slug),
        quote(text)
    )
}

/// `gh search issues --repo <slug> --state open --limit 30 --json … -- <text>`.
pub(crate) fn search_issues_cmd(slug: &str, text: &str) -> String {
    format!(
        "gh search issues --repo {} --state open --limit 30 \
         --json number,title,url,updatedAt,repository,author,labels,commentsCount -- {}",
        quote(slug),
        quote(text)
    )
}

/// `gh run list --repo <slug> --limit 20 --json …`.
pub(crate) fn run_list_cmd(slug: &str) -> String {
    format!(
        "gh run list --repo {} --limit 20 \
         --json databaseId,displayTitle,workflowName,status,conclusion,headBranch,event,createdAt,url",
        quote(slug)
    )
}
