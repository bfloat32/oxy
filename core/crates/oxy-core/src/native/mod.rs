//! Extensions compiled into the daemon. Each names itself in its JSON file
//! with `"native": "<name>"` and may still carry a `search` command, which the
//! worker runs when the provider returns `Fallback`.
//!
//! The set below is the reference implementation's built-ins and the first
//! ported extensions. The rest answer through their `search` scripts while
//! they wait their turn.

pub mod apps;
pub mod calc;
pub mod calendar;
pub mod commands;
pub mod file;
pub mod kill;
pub mod quicklinks;
pub mod sys;
pub mod web;

use crate::provider::NativeExt;

/// The name a `"native"` field knows. Unknown names load as if the field were
/// absent, so a JSON file naming a build that does not have it falls back to
/// its `search` rather than going silent.
pub fn construct(name: &str) -> Option<Box<dyn NativeExt>> {
    match name {
        "apps" => Some(Box::new(apps::Apps::new())),
        "calc" => Some(Box::new(calc::Calc::new())),
        "cal" | "calendar" => Some(Box::new(calendar::Cal::new())),
        "commands" | "run" => Some(Box::new(commands::Commands)),
        "file" | "files" => Some(Box::new(file::Files)),
        "kill" | "ps" => Some(Box::new(kill::Kill::new())),
        "quicklinks" => Some(Box::new(quicklinks::Quicklinks)),
        "sys" | "system" => Some(Box::new(sys::Sys::new())),
        "web" => Some(Box::new(web::Web)),
        _ => None,
    }
}
