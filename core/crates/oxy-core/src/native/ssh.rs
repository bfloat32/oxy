//! `ssh:` — every host you have written down, read from `~/.ssh/config` and
//! its `Include`s, with the `known_hosts` mark the script carried. A port of
//! `bin/oxy-ssh`: nothing here touches the network, so the 60ms debounce the
//! keyword runs on stays honest.
//!
//! Row fields are the ones the `hosts` view reads: `alias`, `hostName`,
//! `user`, `port`, `sourceFile`, `identity`, `proxyJump`, `known`.

use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::shellquote::quote;

pub struct Ssh;

/// One block's worth of settings, emitted once per non-wildcard alias.
struct Host {
    aliases: Vec<String>,
    hostname: String,
    user: String,
    port: String,
    identity: String,
    proxy: String,
    file: String,
}

/// The files `Include` expands to, in the order ssh itself would read them.
/// Depth is bounded because an Include cycle is otherwise an infinite loop
/// rather than an error.
fn expand_includes(file: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    if depth > 8 {
        return;
    }
    let Ok(text) = std::fs::read_to_string(file) else {
        return;
    };
    out.push(file.to_path_buf());
    let home = crate::dirs::home();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.splitn(2, char::is_whitespace);
        if !words.next().unwrap_or("").eq_ignore_ascii_case("include") {
            continue;
        }
        for target in words.next().unwrap_or("").split_whitespace() {
            let target = if let Some(rest) = target.strip_prefix('~') {
                home.join(rest.trim_start_matches('/'))
            } else {
                let p = PathBuf::from(target);
                // A relative Include is relative to ~/.ssh, not the cwd.
                if p.is_absolute() {
                    p
                } else {
                    home.join(".ssh").join(p)
                }
            };
            for m in expand_glob(&target) {
                if m.is_file() {
                    expand_includes(&m, depth + 1, out);
                }
            }
        }
    }
}

/// The shell's `for match in $target` — a literal path, or `*`, `?` and
/// `[...]` over the parent directory's listing.
fn expand_glob(target: &Path) -> Vec<PathBuf> {
    let text = target.to_string_lossy();
    if !text.contains(['*', '?', '[']) {
        return vec![target.to_path_buf()];
    }
    let Some(parent) = target.parent() else {
        return Vec::new();
    };
    let Some(name) = target.file_name().and_then(|n| n.to_str()) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = std::fs::read_dir(parent)
        .map(|read| {
            read.flatten()
                .filter(|e| e.file_name().to_str().is_some_and(|n| glob_match(name, n)))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// `*` any run, `?` one char, `[abc]`/`[a-z]` one of the set, `[!..]` its
/// complement — the subset of fnmatch ssh configs actually use.
fn glob_match(pat: &str, text: &str) -> bool {
    fn inner(p: &[u8], t: &[u8]) -> bool {
        if p.is_empty() {
            return t.is_empty();
        }
        match p[0] {
            b'*' => (0..=t.len()).any(|i| inner(&p[1..], &t[i..])),
            b'?' => !t.is_empty() && inner(&p[1..], &t[1..]),
            b'[' => {
                if t.is_empty() {
                    return false;
                }
                let (set, rest) = match p.iter().position(|&c| c == b']') {
                    // A ']' as the first member is itself: [!] matches ']'.
                    Some(0) | None => return inner(&p[1..], t),
                    Some(i) => (&p[1..i], &p[i + 1..]),
                };
                let (neg, set) = if set.first() == Some(&b'!') {
                    (true, &set[1..])
                } else {
                    (false, set)
                };
                let c = t[0];
                let mut hit = false;
                let mut i = 0;
                while i < set.len() {
                    if i + 2 < set.len() && set[i + 1] == b'-' {
                        if (set[i]..=set[i + 2]).contains(&c) {
                            hit = true;
                        }
                        i += 3;
                    } else {
                        if set[i] == c {
                            hit = true;
                        }
                        i += 1;
                    }
                }
                if hit != neg {
                    inner(rest, &t[1..])
                } else {
                    false
                }
            }
            c => !t.is_empty() && t[0] == c && inner(&p[1..], &t[1..]),
        }
    }
    inner(pat.as_bytes(), text.as_bytes())
}

/// The names known_hosts can answer for. A `|` hostfield is hashed, and a
/// hashed file cannot be matched without reimplementing the hash — so the
/// whole signal drops to `null` rather than reading "never connected" for
/// every host. `@cert-authority` markers land where a hostname would, so
/// those lines are skipped.
fn known_hosts() -> Option<HashSet<String>> {
    let path = crate::dirs::home().join(".ssh/known_hosts");
    let text = std::fs::read_to_string(path).ok()?;
    let mut set = HashSet::new();
    for line in text.lines() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') || line.starts_with('@') {
            continue;
        }
        let Some(hostfield) = line.split_whitespace().next() else {
            continue;
        };
        if hostfield.starts_with('|') {
            return None;
        }
        for name in hostfield.split(',') {
            if !name.is_empty() {
                set.insert(name.to_lowercase());
            }
        }
    }
    Some(set)
}

/// ssh accepts `Key value` and `Key=value` alike; only a leading `Key=` is
/// rewritten, so an `=` inside a ProxyCommand is not eaten.
fn split_key_value(line: &str) -> Option<(String, String)> {
    let line = line.trim_start();
    let mut buf = line.to_string();
    if let Some(eq) = line.find('=')
        && line[..eq].chars().all(|c| c.is_ascii_alphabetic())
        && !line[..eq].is_empty()
    {
        buf.replace_range(eq..=eq, " ");
    }
    let mut parts = buf.splitn(2, char::is_whitespace);
    let key = parts.next()?;
    let value = parts.next()?.trim_start();
    if value.is_empty() {
        return None;
    }
    Some((key.to_lowercase(), value.to_string()))
}

fn parse_hosts(files: &[PathBuf]) -> Vec<Host> {
    let mut hosts: Vec<Host> = Vec::new();
    let mut cur: Option<Host> = None;
    for file in files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let fname = file.to_string_lossy().into_owned();
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            let line = line.trim_start();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = split_key_value(line) else {
                continue;
            };
            match key.as_str() {
                "host" => {
                    if let Some(h) = cur.take() {
                        hosts.push(h);
                    }
                    let mut h = Host {
                        aliases: Vec::new(),
                        hostname: String::new(),
                        user: String::new(),
                        port: String::new(),
                        identity: String::new(),
                        proxy: String::new(),
                        file: fname.clone(),
                    };
                    // `Host prod production` is two aliases for one block; a
                    // pattern with a wildcard is a default, not a machine.
                    for w in value.split_whitespace() {
                        if !w.contains(['*', '?', '!']) {
                            h.aliases.push(w.to_string());
                        }
                    }
                    cur = Some(h);
                }
                "hostname" => {
                    if let Some(h) = cur.as_mut() {
                        h.hostname = value;
                    }
                }
                "user" => {
                    if let Some(h) = cur.as_mut() {
                        h.user = value;
                    }
                }
                "port" => {
                    if let Some(h) = cur.as_mut() {
                        h.port = value;
                    }
                }
                // Only the first is kept: the row answers "is a key pinned
                // here", not the keyring.
                "identityfile" => {
                    if let Some(h) = cur.as_mut()
                        && h.identity.is_empty()
                    {
                        h.identity = value;
                    }
                }
                "proxyjump" => {
                    if let Some(h) = cur.as_mut() {
                        h.proxy = value;
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(h) = cur.take() {
        hosts.push(h);
    }
    hosts
}

fn tilde(path: &str) -> String {
    let home = crate::dirs::home();
    let home = home.to_string_lossy();
    if let Some(rest) = path.strip_prefix(home.as_ref()) {
        format!("~{rest}")
    } else {
        path.to_string()
    }
}

impl NativeExt for Ssh {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move {
            let config = crate::dirs::home().join(".ssh/config");
            if !config.is_file() {
                return NativeOutcome::Empty;
            }
            let needle = ctx.arg.trim().to_lowercase();
            let mut files = Vec::new();
            expand_includes(&config, 0, &mut files);
            let hosts = parse_hosts(&files);
            let known = known_hosts();

            let mut rows = Vec::new();
            let mut emitted = 0usize;
            for h in &hosts {
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
                        && !format!("{} {}", alias.to_lowercase(), sub.to_lowercase())
                            .contains(&needle)
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
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("*.conf", "a.conf"));
        assert!(!glob_match("*.conf", "a.cfg"));
        assert!(glob_match("a?c", "abc"));
        assert!(glob_match("[ab]c", "bc"));
        assert!(!glob_match("[!ab]c", "bc"));
        assert!(glob_match("[!ab]c", "dc"));
        assert!(glob_match("config.d/*", "config.d/x"));
    }

    #[test]
    fn key_value_forms() {
        assert_eq!(
            split_key_value("HostName=example.com"),
            Some(("hostname".into(), "example.com".into()))
        );
        assert_eq!(
            split_key_value("  ProxyCommand ssh -W %h:%p jump"),
            Some(("proxycommand".into(), "ssh -W %h:%p jump".into()))
        );
        // A `Key=` rewrite must not eat an `=` inside the value.
        assert_eq!(
            split_key_value("ProxyCommand=ssh -W x=y j"),
            Some(("proxycommand".into(), "ssh -W x=y j".into()))
        );
        assert_eq!(split_key_value("Host"), None);
    }

    #[test]
    fn host_blocks() {
        let dir = std::env::temp_dir().join(format!("oxy-ssh-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let inc = dir.join("extra.conf");
        let main = dir.join("config");
        std::fs::write(&inc, "Host jump\n  HostName jump.example\n  Port 2222\n").unwrap();
        std::fs::write(
            &main,
            format!(
                "# comment\nHost prod production\n  HostName 10.0.0.1\n  User deploy\n\
                 Host *\n  User root\nInclude {}\n",
                inc.display()
            ),
        )
        .unwrap();
        let mut files = Vec::new();
        expand_includes(&main, 0, &mut files);
        let hosts = parse_hosts(&files);
        let aliases: Vec<&str> = hosts
            .iter()
            .flat_map(|h| h.aliases.iter().map(String::as_str))
            .collect();
        assert_eq!(aliases, ["prod", "production", "jump"]);
        assert_eq!(hosts[0].hostname, "10.0.0.1");
        assert_eq!(hosts[0].user, "deploy");
        assert_eq!(hosts[2].port, "2222");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
