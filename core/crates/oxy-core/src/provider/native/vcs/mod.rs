//! Providers that read version-control state: the repo picker and the
//! three views over the current checkout (status, branches, stashes).
//! `run` is the one place a `git` invocation is assembled.

pub mod branch;
pub mod git;
pub mod repo;
// `dead_code`: the shared layer lands ahead of its four callers — the ports
// consume it together, and until then nothing calls it.
#[allow(dead_code)]
pub(crate) mod repos;
mod run;
pub mod stash;
