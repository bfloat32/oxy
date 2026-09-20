//! `pass` — the vault: names and folders from `pass` or 1Password's `op`,
//! whichever is installed. A row never carries a secret — copy/type/clear
//! go back through `oxy-pass`'s own legs, whose pipes were written not to
//! echo. A zero-count first row is how the card explains an empty store.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::{Ctx, NativeExt, NativeOutcome};

#[derive(Default)]
pub struct Pass;

impl NativeExt for Pass {
    fn query<'a>(
        &'a mut self,
        _ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move { NativeOutcome::Fallback })
    }
}
