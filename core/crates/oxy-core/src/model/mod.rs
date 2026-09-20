//! The shapes on the wire — what a row, an action, a query and an event are.
//!
//! Nothing in `model` may open a file or spawn a process; it is the
//! vocabulary every other layer speaks.

pub mod action;
pub mod event;
pub mod query;
pub mod row;
