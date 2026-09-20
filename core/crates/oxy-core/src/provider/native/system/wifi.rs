//! `wifi — networks, the radio and saved profiles`. A port of `bin/oxy-wifi`.
//!
//! Stub for now: it declines every question, so the manifest's `search`
//! script answers until the port lands.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::{Ctx, NativeExt, NativeOutcome};

pub struct Wifi;

impl NativeExt for Wifi {
    fn query<'a>(
        &'a mut self,
        _ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move { NativeOutcome::Fallback })
    }
}
