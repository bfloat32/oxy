//! The user's own destinations, from `quicklinks` in oxy.json — a port of
//! `plugin/Quicklinks.js` plus its Launcher.qml dispatch.
//!
//! Two ways in, because people reach for both: typing part of the title finds
//! the link among everything else, and typing its keyword addresses it
//! directly and hands the rest of the line over as the argument.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::model::row::{Action, Row};
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::settings::Quicklink;
use crate::support::quote::quote;
use crate::support::rank;
use crate::support::score::{Entry, fuzzy};

fn as_entry(link: &Quicklink, index: usize) -> Entry {
    let mut keywords: Vec<String> = link.tags.clone();
    if !link.keyword.is_empty() {
        keywords.push(link.keyword.clone());
    }
    Entry::new(
        format!(
            "ql.{}",
            if link.keyword.is_empty() {
                index.to_string()
            } else {
                link.keyword.clone()
            }
        ),
        link.title.clone(),
        link.subtitle.clone(),
        if link.url.is_empty() {
            link.open.clone()
        } else {
            link.url.clone()
        },
        keywords,
    )
}

/// Does a loaded extension answer for this keyword, by name or alias? The
/// quicklinks extension itself is skipped — it claims every link keyword as
/// an alias so the router can find it, and counting that claim would shadow
/// the very link being asked about.
fn extension_claims(ctx: &Ctx, keyword: &str) -> bool {
    ctx.registry.iter().any(|ext| {
        ext.id != "quicklinks"
            && (ext.keyword == keyword || ext.aliases.iter().any(|a| a == keyword))
    })
}

pub struct Quicklinks;

impl NativeExt for Quicklinks {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move {
            let links = &ctx.settings.quicklinks;
            if links.is_empty() {
                return NativeOutcome::Empty;
            }

            let mut out = Vec::new();
            for (i, link) in links.iter().enumerate() {
                if link.title.is_empty() {
                    continue;
                }
                let keyword = link.keyword.to_lowercase();

                // An extension that owns the keyword owns the answer: a
                // quicklink called `gh` sat above the GitHub extension's own
                // rows, and because the view comes from the first row the
                // repository panel never drew.
                if !keyword.is_empty() && extension_claims(&ctx, &keyword) {
                    continue;
                }

                let addressed = !keyword.is_empty() && ctx.query.scope == keyword;
                let argument;
                let fuzzy_score;

                if addressed {
                    argument = ctx.query.arg_for(&keyword, &[]);
                    fuzzy_score = -1;
                } else {
                    if !ctx.query.scope.is_empty() && ctx.query.scope != "quicklinks" {
                        continue;
                    }
                    if ctx.query.empty {
                        continue;
                    }
                    fuzzy_score = fuzzy(&as_entry(link, i), &ctx.query.text);
                    if fuzzy_score < 0 {
                        continue;
                    }
                    // Typing a name finds the link; it cannot also supply the
                    // argument, since the words that found it are not what
                    // goes in the placeholder.
                    argument = String::new();
                }

                let command = link.command(&argument);
                if command.is_empty() {
                    continue;
                }
                let needs_argument = link.takes_argument();
                let expanded = link.expand(&argument);

                let mut row = Row::new(
                    format!(
                        "ql:{}",
                        if keyword.is_empty() {
                            &link.title
                        } else {
                            &keyword
                        }
                    ),
                    "quicklinks",
                );
                row.group = "Quicklinks".into();
                row.title = link.title.clone();
                row.subtitle = link.tags.join(", ");
                row.detail = if addressed && !argument.is_empty() {
                    argument
                } else {
                    String::new()
                };
                row.accessory = if needs_argument && !keyword.is_empty() {
                    format!("{keyword}:")
                } else {
                    String::new()
                };
                row.icon_glyph = link.glyph.clone();
                // Addressed by keyword it is the answer, so it pins above apps.
                // Found by name it competes on match quality alone.
                if addressed {
                    row.tier = rank::TIER_FORCED;
                    row.local = 90000;
                    row.score = rank::score(rank::TIER_FORCED, 90000, 0);
                } else {
                    row.tier = rank::tier_for_fuzzy(fuzzy_score);
                    row.local = rank::local_for_fuzzy(fuzzy_score);
                    row.score = rank::score(row.tier, row.local, 2000);
                }
                row.exec = command.clone();
                let mut actions = vec![Action {
                    title: "Open".into(),
                    shortcut: "↵".into(),
                    exec: command,
                    ..Action::default()
                }];
                if !expanded.is_empty() {
                    actions.push(Action {
                        title: "Copy Link".into(),
                        exec: format!("printf %s {} | wl-copy", quote(&expanded)),
                        ..Action::default()
                    });
                }
                row.actions = Some(actions);
                out.push(row);
            }
            NativeOutcome::Built(out)
        })
    }
}
