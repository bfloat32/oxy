//! The always-last row: take this question to the web — a port of the
//! `queryWeb` builtin in `Launcher.qml`.
//!
//! Tier `web` is below everything real, and the merger drops the row entirely
//! when anything else answered — except when the query is scoped to it, which
//! is how `?dogs` still searches.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::model::row::{Action, Row};
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::settings::url_encode;
use crate::support::quote::quote;
use crate::support::rank;

pub struct Web;

fn engine_url(settings: &crate::settings::Settings, id: &str, query: &str) -> String {
    settings
        .engine(id)
        .map(|e| e.url.replace("{}", &url_encode(query)))
        .unwrap_or_default()
}

impl NativeExt for Web {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move {
            let text = ctx.arg.trim().to_string();
            if text.is_empty() {
                return NativeOutcome::Empty;
            }
            let settings = &ctx.settings;
            let engine = settings
                .engine(&settings.default_engine)
                .map(|e| e.title.clone())
                .unwrap_or_else(|| settings.default_engine.clone());
            let default_url = engine_url(settings, &settings.default_engine, &text);

            let mut row = Row::new(format!("web:{text}"), "web");
            row.group = "Web".into();
            row.title = format!("Search the web for \u{201C}{text}\u{201D}");
            row.subtitle = engine;
            row.tier = rank::TIER_WEB;
            row.local = 0;
            row.score = rank::score(rank::TIER_WEB, 0, 0);
            row.exec = format!("omarchy-launch-browser {}", quote(&default_url));

            // The default engine first, then whatever else is configured:
            // Ctrl+K is how "search Google" becomes "ask ChatGPT" without
            // touching a config file.
            let mut seen = std::collections::HashSet::new();
            let mut actions = Vec::new();
            let add = |id: &str,
                       primary: bool,
                       actions: &mut Vec<Action>,
                       seen: &mut std::collections::HashSet<String>| {
                if !seen.insert(id.to_string()) {
                    return;
                }
                let Some(engine) = settings.engine(id) else {
                    return;
                };
                let url = engine_url(settings, id, &text);
                if url.is_empty() {
                    return;
                }
                actions.push(Action {
                    title: engine.title.clone(),
                    shortcut: if primary { "↵".into() } else { String::new() },
                    exec: format!("omarchy-launch-browser {}", quote(&url)),
                    ..Action::default()
                });
            };
            add(
                &settings.default_engine.clone(),
                true,
                &mut actions,
                &mut seen,
            );
            for id in settings.engine_actions.clone() {
                add(&id, false, &mut actions, &mut seen);
            }
            row.actions = Some(actions);
            NativeOutcome::Built(vec![row])
        })
    }
}
