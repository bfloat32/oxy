//! `note` — the notes file: one row per `## ` heading with the body
//! between it and the next heading, newest first, from
//! `~/.config/omarchy/oxy-notes.md`. The write legs (`--save`, `--open`,
//! `--edit`, `--trash`) stay in `oxy-note`, which its own rows still exec.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::{Ctx, NativeExt, NativeOutcome};

#[derive(Default)]
pub struct Note;

impl NativeExt for Note {
    fn query<'a>(
        &'a mut self,
        _ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move { NativeOutcome::Fallback })
    }
}
