//! `def` — the dictionary and thesaurus behind `def:` (aliases `define`,
//! `dict`, `syn`, `word`), a port of `bin/oxy-define`.
//!
//! api.dictionaryapi.dev answers keyless over curl — there is no offline
//! dictionary on an Omarchy box — and answers land in
//! `~/.cache/oxy/define`, because a word means the same thing tomorrow and
//! typing is one request per keystroke otherwise. A miss is not the end of
//! the lookup: the word is stemmed, the phrase is tried whole then by its
//! head word, and a spelling no stem rescues gets one Datamuse guess,
//! shown only after the guess itself has been looked up and found real.
//! Whatever answers, the row says which spelling answered it.
//!
//! `curl` stays the transport — there is no HTTP client in the workspace —
//! through `process::run` like every other provider's shell call; jq's half
//! is serde_json. The split: `word` is the pure text work (term cleanup,
//! stems, edit distance), `rows` is the jq program and the standalone rows.

mod rows;
mod word;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Map, Value};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::native::util::on_path;
use crate::provider::{Ctx, NativeExt, NativeOutcome, process};
use crate::support::quote::quote;

#[derive(Default)]
pub struct Def;

/// The script's own deadlines: curl's `--max-time` is 6 on the dictionary
/// and 3 on Datamuse; the run wrapper gets a second of slack over each so
/// curl's own limit is the one that fires.
const DICT_TIMEOUT: Duration = Duration::from_secs(7);
const DATAMUSE_TIMEOUT: Duration = Duration::from_secs(4);

/// `budget=4` — every miss is a request, and a word with three stems and a
/// spelling guess behind it would be five on one keystroke. The cache
/// means it is only ever paid once per word.
const BUDGET: u32 = 4;
/// `cache_max=500` — expiry does the real work; the cap exists so a bad
/// afternoon cannot fill a disk.
const CACHE_MAX: usize = 500;
/// `${OXY_CACHEDAYS:-30}` — thirty days when nobody has said otherwise: a
/// definition does not change.
const DEFAULT_CACHE_DAYS: u64 = 30;
const DAY_SECS: u64 = 86400;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn mtime(path: &Path) -> u64 {
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `${XDG_CACHE_HOME:-$HOME/.cache}/oxy/define`.
fn cache_dir() -> PathBuf {
    std::env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::settings::paths::home().join(".cache"))
        .join("oxy")
        .join("define")
}

/// `${OXY_CACHEDAYS:-30}` with the `^[0-9]+$` guard — the script leg gets
/// the value as an `OXY_…` environment prefix from
/// `extensionSettings.def.cacheDays`, so that key wins here too and the
/// raw environment variable is the fallback.
fn cache_days(settings: Option<&Map<String, Value>>) -> u64 {
    let raw = settings
        .and_then(|m| m.get("cacheDays"))
        .filter(|v| !v.is_null())
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .or_else(|| std::env::var("OXY_CACHEDAYS").ok())
        .unwrap_or_default();
    if !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()) {
        raw.parse().unwrap_or(DEFAULT_CACHE_DAYS)
    } else {
        DEFAULT_CACHE_DAYS
    }
}

/// A cache file is servable while `find "$f" -mtime +days -print -quit`
/// prints nothing — "modified more than `days` days ago" is the stale side
/// of that test.
fn fresh(file: &Path, days: u64) -> bool {
    let Ok(mtime) = file.metadata().and_then(|m| m.modified()) else {
        return false;
    };
    let age = std::time::SystemTime::now()
        .duration_since(mtime)
        .unwrap_or_default()
        .as_secs();
    age / DAY_SECS <= days
}

/// `find -delete` removes an empty directory too; `rm -f` (the cap's tool)
/// does not. The two call sites differ the same way the script's do.
fn remove_entry(path: &Path, also_dirs: bool) {
    if also_dirs && path.is_dir() {
        let _ = std::fs::remove_dir(path);
    } else {
        let _ = std::fs::remove_file(path);
    }
}

/// `prune_cache` — called after a word is written, and only then: a run
/// that answers from the cache should cost no more than reading one file.
/// Expired entries go first (the same rule the read side already applies);
/// if the directory is still over the ceiling after that, the oldest go
/// until it is not.
fn prune_cache(dir: &Path, days: u64) {
    let now = now_secs();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<(u64, PathBuf)> = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        // `find -name '*.json'` — every entry type counts, not only files.
        let is_json = path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().ends_with(".json"));
        if !is_json {
            continue;
        }
        let m = mtime(&path);
        if now.saturating_sub(m) / DAY_SECS > days {
            remove_entry(&path, true);
        } else {
            entries.push((m, path));
        }
    }
    if entries.len() <= CACHE_MAX {
        return;
    }
    entries.sort_by_key(|(m, _)| *m);
    for (_, path) in entries.iter().take(entries.len() - CACHE_MAX) {
        remove_entry(path, false);
    }
}

/// `jq -e 'type == "array" and length > 0'` — a real entry is a non-empty
/// array; the API's "no definitions found" is an object, and a timeout is
/// nothing at all.
fn is_entry(body: &str) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .map(|v| v.as_array().is_some_and(|a| !a.is_empty()))
        .unwrap_or(false)
}

/// One lookup, through the cache. `lookup` is true only when the answer is
/// a real entry; `reached` tells a miss apart from an outage — a word with
/// no entry is an answer worth saying out loud, a dictionary that could
/// not be reached is not, and the two must not print the same row.
struct Lookup {
    dir: PathBuf,
    days: u64,
    /// Request ceiling per query — cache hits do not spend it.
    budget: u32,
    /// A curl that came back empty is the network, not the word — trying
    /// the next stem would be another six seconds finding out the same
    /// thing.
    dead: bool,
    reached: bool,
    /// The body the last lookup left — the winning array when it matters.
    body: String,
}

impl Lookup {
    async fn lookup(&mut self, word: &str) -> bool {
        let w = word.to_lowercase();
        let cache = self.dir.join(format!("{}.json", w.replace(' ', "_")));

        if cache.is_file() && fresh(&cache, self.days) {
            self.body = std::fs::read_to_string(&cache).unwrap_or_default();
            self.reached = true;
            return is_entry(&self.body);
        }

        if self.dead || self.budget == 0 {
            return false;
        }
        self.budget -= 1;
        let url = format!(
            "https://api.dictionaryapi.dev/api/v2/entries/en/{}",
            w.replace(' ', "%20")
        );
        let fin = process::run(
            &format!("curl -sS --max-time 6 --compressed {}", quote(&url)),
            DICT_TIMEOUT,
        )
        .await;
        // `body=$(curl …)` — command substitution strips trailing newlines.
        let body = fin
            .map(|f| f.stdout.trim_end_matches('\n').to_string())
            .unwrap_or_default();
        if body.is_empty() {
            self.dead = true;
            return false;
        }
        self.reached = true;
        self.body = body;
        if is_entry(&self.body) {
            // Only a real answer is worth keeping — a timeout cached for a
            // month is worse than no cache at all.
            if std::fs::create_dir_all(&self.dir).is_ok()
                && std::fs::write(&cache, &self.body).is_ok()
            {
                prune_cache(&self.dir, self.days);
            }
            return true;
        }
        false
    }
}

/// `curl -sS --max-time 3 … | jq -r '.[]?.word // empty'` — the candidate
/// spellings Datamuse offers, as the lines jq would print. `.[]?` walks an
/// array (or an object's values); `.word` on something that cannot be
/// indexed is where jq dies, which is where the list ends.
async fn datamuse(lower: &str) -> Vec<String> {
    let url = format!(
        "https://api.datamuse.com/words?sl={}&max=4",
        lower.replace(' ', "%20")
    );
    let Some(fin) = process::run(
        &format!("curl -sS --max-time 3 {}", quote(&url)),
        DATAMUSE_TIMEOUT,
    )
    .await
    else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&fin.stdout) else {
        return Vec::new();
    };
    let items: Vec<&Value> = match &v {
        Value::Array(a) => a.iter().collect(),
        Value::Object(o) => o.values().collect(),
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for item in items {
        let Value::Object(o) = item else { break };
        match o.get("word") {
            None | Some(Value::Null) | Some(Value::Bool(false)) => {}
            Some(Value::String(s)) => out.push(s.clone()),
            // `jq -r` renders what is there — a number prints as its digits.
            Some(other) => out.push(other.to_string()),
        }
    }
    out
}

async fn answer(arg: &str, settings: &crate::settings::Settings) -> NativeOutcome {
    let Some(term) = word::clean_term(arg) else {
        return NativeOutcome::Empty;
    };
    let lower = term.to_lowercase();
    let mut lk = Lookup {
        dir: cache_dir(),
        days: cache_days(settings.settings_for("def")),
        budget: BUDGET,
        dead: false,
        reached: false,
        body: String::new(),
    };

    // The script also carried `shown` — which spelling answered — but only
    // `note` ever reaches a row, so the note is what is kept here.
    let mut note = String::new();
    let mut found = lk.lookup(&lower).await;
    if !found {
        // A hyphenated phrase and a spaced one are the same phrase to a
        // reader.
        if lower.contains('-') {
            let spaced = lower.replace('-', " ");
            if lk.lookup(&spaced).await {
                found = true;
                note = format!("No entry for “{term}” — showing “{spaced}”");
            }
        }
        if !found {
            for s in word::stems(&lower) {
                if s.is_empty() || s == lower {
                    continue;
                }
                if lk.lookup(&s).await {
                    found = true;
                    note = format!("No entry for “{term}” — showing “{s}”");
                    break;
                }
            }
        }
        // A phrase the dictionary does not carry is still worth answering
        // by its head word, and the row says that is what happened.
        if !found && lower.contains(' ') {
            let head = lower.split(' ').next().unwrap_or_default();
            if head.chars().count() >= 2 && lk.lookup(head).await {
                found = true;
                note = format!("No entry for “{term}” — showing “{head}”");
            }
        }
    }

    // One guess at a spelling, and only after the guess has been looked up
    // and found real. Datamuse is keyless and its first candidate is
    // usually the word itself, which is why the query is skipped over
    // rather than trusted.
    if !found && lk.reached && !lower.contains(' ') && lower.chars().count() >= 4 {
        for cand in datamuse(&lower).await.into_iter().take(3) {
            if cand.is_empty() || cand == lower {
                continue;
            }
            if !word::near(&lower, &cand) {
                continue;
            }
            if lk.lookup(&cand).await {
                found = true;
                note = format!("No entry for “{term}” — did you mean “{cand}”?");
                break;
            }
        }
    }

    if !found && !lk.reached {
        return NativeOutcome::Rows(vec![rows::offline_row(&term)]);
    }
    if !found {
        return NativeOutcome::Rows(vec![rows::no_def_row(&term, &lower)]);
    }
    let Ok(body) = serde_json::from_str::<Value>(&lk.body) else {
        return NativeOutcome::Empty;
    };
    match rows::define_rows(&body, &note) {
        Some(rows) => NativeOutcome::Rows(rows),
        // jq dying on a malformed shape is the script printing nothing.
        None => NativeOutcome::Empty,
    }
}

impl NativeExt for Def {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        let arg = ctx.arg.clone();
        let settings = ctx.settings.clone();
        Box::pin(async move {
            if arg.is_empty() {
                return NativeOutcome::Empty;
            }
            // The manifest's `when`, re-checked — a worker asks a native
            // even when it fails, and here that is a decline, not a list.
            if !on_path("curl") {
                return NativeOutcome::Fallback;
            }
            answer(&arg, &settings).await
        })
    }
}

#[cfg(test)]
mod tests;
