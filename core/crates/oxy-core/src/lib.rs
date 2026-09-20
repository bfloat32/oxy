//! The Rust core of Oxy.
//!
//! Everything the launcher knows how to do lives here: parsing the box,
//! routing to providers, merging answers into one ranked list, and the state
//! (frecency, pins, recents, cache) that makes the second opening instant.
//!
//! The QML frontend is a thin client: it sends the raw text of the box and
//! draws whatever `Results` the engine pushes back.

#![forbid(unsafe_code)]

pub mod availability;
pub mod cache;
pub mod dirs;
pub mod engine;
pub mod extension;
pub mod native;
pub mod provider;
pub mod query;
pub mod rank;
pub mod row;
pub mod score;
pub mod settings;
pub mod shellquote;
pub mod state;

pub use engine::{Engine, EngineCmd, EngineEvent};
pub use extension::Extension;
pub use query::Query;
pub use row::{Action, Row};

/// A field that could be arbitrarily long (a query, a command, a title) is
/// truncated before it is logged — the log explains behavior, and a
/// ten-thousand-character paste explains nothing more than its head does.
pub(crate) fn clip(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    } else {
        s.to_string()
    }
}
