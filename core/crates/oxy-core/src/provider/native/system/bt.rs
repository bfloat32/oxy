//! `bt — bluetooth devices and the radio`. A port of `bin/oxy-bluetooth`.
//!
//! Stub for now: it declines every question, so the manifest's `search`
//! script answers until the port lands.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::{Ctx, NativeExt, NativeOutcome};

pub struct Bt;

impl NativeExt for Bt {
    fn query<'a>(
        &'a mut self,
        _ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move { NativeOutcome::Fallback })
    }
}
