//! Extensions compiled into the daemon. Each names itself in its JSON file
//! with `"native": "<name>"` and may still carry a `search` command, which the
//! worker runs when the provider returns `Fallback`.
//!
//! The set below is the reference implementation's built-ins and the first
//! ported extensions. The rest answer through their `search` scripts while
//! they wait their turn.

pub mod calc;
pub mod desktop;
pub mod system;
pub mod text;
pub mod time;
pub mod util;

use crate::provider::NativeExt;

/// The name a `"native"` field knows. Unknown names load as if the field were
/// absent, so a JSON file naming a build that does not have it falls back to
/// its `search` rather than going silent.
pub fn construct(name: &str) -> Option<Box<dyn NativeExt>> {
    match name {
        "alarm" => Some(Box::new(time::alarm::Alarm)),
        "apps" => Some(Box::new(desktop::apps::Apps::new())),
        "bri" | "brightness" => Some(Box::new(system::bri::Bri)),
        "bt" | "bluetooth" => Some(Box::new(system::bt::Bt)),
        "calc" => Some(Box::new(calc::Calc::new())),
        "cal" | "calendar" => Some(Box::new(time::calendar::Cal::new())),
        "calchist" => Some(Box::new(text::calchist::CalcHist::default())),
        "ch" | "clipboard" => Some(Box::new(system::clipboard::Clip::default())),
        "commands" | "run" => Some(Box::new(desktop::commands::Commands::new())),
        "date" => Some(Box::new(time::date::Date)),
        "emoji" => Some(Box::new(desktop::emoji::Emoji::default())),
        "file" | "files" => Some(Box::new(system::file::Files)),
        "kill" | "ps" => Some(Box::new(system::kill::Kill::new())),
        "quicklinks" => Some(Box::new(desktop::quicklinks::Quicklinks)),
        "recent" => Some(Box::new(system::recent::Recent::default())),
        "ssh" => Some(Box::new(system::ssh::Ssh::default())),
        "sys" | "system" => Some(Box::new(system::sys::Sys::new())),
        "theme" => Some(Box::new(desktop::theme::Theme)),
        "vol" | "volume" => Some(Box::new(system::vol::Vol)),
        "web" => Some(Box::new(desktop::web::Web)),
        "wifi" => Some(Box::new(system::wifi::Wifi)),
        "win" | "windows" => Some(Box::new(system::win::Win)),
        _ => None,
    }
}
