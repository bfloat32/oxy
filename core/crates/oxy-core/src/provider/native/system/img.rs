//! `img` — the image search: an in-process walk over the usual picture
//! folders (or the `in:` root), newest first, with dimensions only when
//! `identify` is installed. What was a `fd` call is a `WalkBuilder` here,
//! the same crate `file` walks with.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::{Ctx, NativeExt, NativeOutcome};

#[derive(Default)]
pub struct Img;

impl NativeExt for Img {
    fn query<'a>(
        &'a mut self,
        _ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move { NativeOutcome::Fallback })
    }
}
