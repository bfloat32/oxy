//! `herdr` — the agent list: running sessions, answered ones, offline
//! answers, with a refresh while the card is open. Reads `herdr api
//! snapshot` for each *running* session only, and nothing here marks a
//! session seen — those reads stay read-only.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::{Ctx, NativeExt, NativeOutcome};

#[derive(Default)]
pub struct Herdr;

impl NativeExt for Herdr {
    fn query<'a>(
        &'a mut self,
        _ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move { NativeOutcome::Fallback })
    }
}
