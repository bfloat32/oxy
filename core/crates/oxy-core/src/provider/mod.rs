//! A provider is whatever answers for an extension: a `search` command run
//! through the shell, a daemon on a unix socket, or a native implementation
//! compiled into the core.
//!
//! `worker` is the per-extension state machine — the port of
//! `ExtensionProvider.qml`: debounce, timeout, cache, refresh, availability,
//! and the epoch bookkeeping that keeps a slow answer from arriving after the
//! question changed.

pub mod process;
pub mod socket;
pub mod worker;

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;

use crate::extension::Extension;
use crate::query::Query;
use crate::settings::Settings;

/// Everything a provider needs to know about the question it was asked.
pub struct Ctx {
    pub query: Arc<Query>,
    /// The scoped argument (`arg_for`), already resolved.
    pub arg: String,
    /// Filters nobody claimed (`extras`).
    pub filters: Arc<BTreeMap<String, String>>,
    pub settings: Arc<Settings>,
    /// The loaded registry, for providers that list it (help).
    pub registry: Arc<Vec<Extension>>,
}

/// What a native provider decided.
pub enum NativeOutcome {
    /// Raw row objects; the worker runs them through `to_row` like a script's
    /// output, so both paths produce identical rows.
    Rows(Vec<Value>),
    /// Rows already built — providers that know their own tier and score.
    Built(Vec<crate::row::Row>),
    /// Declined: fall through to the socket or command the file declares.
    Fallback,
    /// A real "no answer" — not a fallback.
    Empty,
}

/// An extension compiled into the daemon.
///
/// `progress` is for answers that arrive in stages — the calculator's
/// "calculating" row, and later anything that streams. Each send replaces the
/// provider's bucket for the epoch without clearing its spinner.
pub trait NativeExt: Send + Sync {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        progress: tokio::sync::mpsc::UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>>;
}
