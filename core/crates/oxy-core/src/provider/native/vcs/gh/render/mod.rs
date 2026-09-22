//! The script's jq emits, decomposed into functions. `jq` holds the
//! shared defs (`ago`, `mark`, `revword`, `prstate`, `openurl`, `copyurl`);
//! `rows` the skeletons and the shared `prrow`; `panels` the
//! single-target `gh:o/r` and `gh:o/r#N` rows; `lists` the many-row
//! emits and the `gh search` reshapes. Each function is one of the
//! script's emit sites, in the order the script lists them.
//!
//! jq is total in a way Rust is not: `.a` on a scalar, `"x" + 5`, a
//! `join` over an object — each kills the emit, and the script's answer to
//! that was silence. `None` in `jq.rs` is that silence: the emitters
//! return `Option`/`Vec` where `None`/empty is "jq died mid-pipe".

mod jq;
mod lists;
mod panels;
mod rows;

pub(crate) use lists::*;
pub(crate) use panels::*;
pub(crate) use rows::*;
