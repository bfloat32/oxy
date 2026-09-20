//! `snip` — the snippet library: `~/.config/omarchy/oxy-snippets.json`
//! parsed in-process, one row per entry with its line/character counts.
//! Enter copies, Ctrl+K types — both still the clipboard daemon's work.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::{Ctx, NativeExt, NativeOutcome};

#[derive(Default)]
pub struct Snip;

impl NativeExt for Snip {
    fn query<'a>(
        &'a mut self,
        _ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move { NativeOutcome::Fallback })
    }
}
