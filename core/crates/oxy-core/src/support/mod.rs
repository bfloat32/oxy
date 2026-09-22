//! Small, dependency-free helpers the other layers share: the answer cache,
//! the availability store, ranking, the fuzzy scorer and shell quoting.

pub mod availability;
pub mod cache;
pub mod lines;
pub mod net;
pub mod quote;
pub mod rank;
pub mod score;
pub mod store;
