//! `ssh:` — every host you have written down, read from `~/.ssh/config` and
//! its `Include`s, with the `known_hosts` mark the script carried. A port of
//! `bin/oxy-ssh`: nothing here touches the network, so the 60ms debounce the
//! keyword runs on stays honest.
//!
//! Row fields are the ones the `hosts` view reads: `alias`, `hostName`,
//! `user`, `port`, `sourceFile`, `identity`, `proxyJump`, `known`.

use std::collections::HashSet;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use super::ssh_config::{Host, expand_includes, known_path, parse_hosts, read_known, tilde};
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::support::quote::quote;

/// The parsed host set, behind a lock so the query can run on
/// `spawn_blocking` — the file walk is synchronous work.
///
/// Keyed on the mtimes of the whole read set: the config, every file its
/// `Include`s resolve to, and `known_hosts`. Any of them changing re-reads;
/// a quiet ~/.ssh keeps the parse.
#[derive(Default)]
pub struct Ssh {
    cache: Arc<Mutex<Option<SshCache>>>,
}

struct SshCache {
    stamps: Vec<(PathBuf, Option<std::time::SystemTime>)>,
    hosts: Arc<Vec<Host>>,
    known: Option<Arc<HashSet<String>>>,
}

impl NativeExt for Ssh {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        let cache = self.cache.clone();
        let arg = ctx.arg.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || query_blocking(cache, &arg))
                .await
                .unwrap_or(NativeOutcome::Empty)
        })
    }
}

fn query_blocking(cache: Arc<Mutex<Option<SshCache>>>, arg: &str) -> NativeOutcome {
    let config = crate::settings::paths::home().join(".ssh/config");
    if !config.is_file() {
        return NativeOutcome::Empty;
    }
    let needle = arg.trim().to_lowercase();
    // Resolving `Include` reads each file — that walk is the price of
    // knowing what to stat. The parse itself is what the cache saves.
    let mut files = Vec::new();
    expand_includes(&config, 0, &mut files);
    files.push(known_path());
    let stamps: Vec<(PathBuf, Option<std::time::SystemTime>)> = files
        .iter()
        .map(|f| {
            (
                f.clone(),
                std::fs::metadata(f).and_then(|m| m.modified()).ok(),
            )
        })
        .collect();
    let (hosts, known) = {
        let mut c = cache.lock().unwrap();
        match c.as_ref() {
            Some(c) if c.stamps == stamps => (c.hosts.clone(), c.known.clone()),
            _ => {
                // `files` still names the known_hosts entry; the host
                // parse reads only the config set.
                let hosts = Arc::new(parse_hosts(&files[..files.len() - 1]));
                let known = read_known(&known_path()).map(Arc::new);
                *c = Some(SshCache {
                    stamps,
                    hosts: hosts.clone(),
                    known: known.clone(),
                });
                (hosts, known)
            }
        }
    };
    let hosts = &*hosts;
    let known = known.as_deref();

    let mut rows = Vec::new();
    let mut emitted = 0usize;
    for h in hosts {
        for alias in &h.aliases {
            if emitted >= 30 {
                break;
            }
            let target = if h.hostname.is_empty() {
                alias.clone()
            } else {
                h.hostname.clone()
            };
            let port = if h.port.is_empty() {
                "22".to_string()
            } else {
                h.port.clone()
            };
            // The plain list still has to read: an unregistered view
            // falls back to it, and blank rows are worse than ugly.
            let mut sub = if h.user.is_empty() {
                target.clone()
            } else {
                format!("{}@{}", h.user, target)
            };
            if port != "22" {
                sub = format!("{sub}:{port}");
            }
            if !needle.is_empty()
                && !format!("{} {}", alias.to_lowercase(), sub.to_lowercase()).contains(&needle)
            {
                continue;
            }
            // ssh keys a host by name alone on the default port and
            // by [name]:port off it — the lookup has to match that
            // exactly.
            let seen = known.as_ref().map(|set| {
                let key = if port == "22" {
                    target.to_lowercase()
                } else {
                    format!("[{}]:{}", target.to_lowercase(), port)
                };
                set.contains(&key)
            });
            let exec = format!(
                "omarchy-launch-tui --app-id=org.omarchy.ssh ssh {}",
                quote(alias)
            );
            let file_short = tilde(&h.file);
            rows.push(json!({
                "id": alias,
                "title": alias,
                "detail": sub,
                "subtitle": file_short,
                "exec": exec,
                "score": 90000 - (emitted as i64) * 100,
                // On every row, not just the first: ranking can put
                // any row first and only the first row's view is read.
                "view": "hosts",
                "alias": alias,
                "hostName": target,
                "user": h.user,
                "port": port.parse::<i64>().unwrap_or(22),
                "sourceFile": file_short,
                "identity": tilde(&h.identity),
                "proxyJump": h.proxy,
                // true, false, or absent when known_hosts is hashed
                // and cannot be read — a field that is wrong is worse
                // than a field that is missing.
                "known": seen,
                "actions": [
                    { "title": "Connect", "shortcut": "↵", "exec": exec },
                    { "title": "Open SFTP", "exec": format!(
                        "setsid uwsm-app -- nautilus {}",
                        quote(&format!("sftp://{alias}"))) },
                    { "title": "Edit Config", "exec": format!(
                        "omarchy-launch-editor {}", quote(&h.file)) },
                ]
            }));
            emitted += 1;
        }
    }
    NativeOutcome::Rows(rows)
}
