//! The family's single `git` call: `git --no-optional-locks` first, so a
//! read never takes a lock a commit is holding — a launcher that blocks a
//! `git commit` is worse than one that answers slowly. `-C <repo>` scopes
//! it when a repo is known. Everything still goes through `process::run`,
//! so the login-env PATH and the deadline behave like every other
//! provider's shell call.

use std::time::Duration;

use crate::provider::process::{Finished, run};
use crate::support::quote::quote;

// `dead_code`: the runner is scaffolded ahead of its callers — the vcs
// ports land on it together, and until then nothing calls it.

/// `git --no-optional-locks <args>` with the family's deadline.
#[allow(dead_code)]
pub(crate) async fn git(args: &str, timeout: Duration) -> Option<Finished> {
    run(&format!("git --no-optional-locks {args}"), timeout).await
}

/// The same, inside `repo`.
#[allow(dead_code)]
pub(crate) async fn git_in(repo: &str, args: &str, timeout: Duration) -> Option<Finished> {
    git(&format!("-C {} {args}", quote(repo)), timeout).await
}

/// Scoped by gitdir instead of worktree — `git --git-dir=<dir>`, which the
/// stash picker uses to ask about a repo without a checkout around it.
#[allow(dead_code)]
pub(crate) async fn git_at(gitdir: &str, args: &str, timeout: Duration) -> Option<Finished> {
    git(&format!("--git-dir={} {args}", quote(gitdir)), timeout).await
}
