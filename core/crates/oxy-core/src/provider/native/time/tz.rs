//! `tz` — the timezone answer: clocks across configured zones, a named-zone
//! lookup over the full IANA list, and the `timegrid` cells. Ported from
//! `bin/oxy-timezone` (+ `oxy-timezone-plan`) onto `jiff`, which reads the
//! same `/usr/share/zoneinfo` on Linux and bundles tzdb on Windows.
//! Stub: declines to the script until the port lands.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::{Ctx, NativeExt, NativeOutcome};

#[derive(Default)]
pub struct Tz;

impl NativeExt for Tz {
    fn query<'a>(
        &'a mut self,
        _ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move { NativeOutcome::Fallback })
    }
}
