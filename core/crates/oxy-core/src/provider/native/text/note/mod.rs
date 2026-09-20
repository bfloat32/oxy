//! `note` — the notes directory: one `.md` file per note, newest first,
//! from `~/Documents/Notes` (`NOTES_DIR`, or `notesDir` in oxy.json, moves
//! it). A port of `bin/oxy-note`'s read half.
//!
//!   note:               every note, newest first, with the count
//!   note:standup        notes whose title or text contains that
//!   note:remember this  the first row writes it down; the rest match
//!
//! The write legs stay in the script. `--save`, `--open`, `--edit` and
//! `--trash` are the script's own modes, and the rows built here still exec
//! `oxy-note --…` verbatim — reads are native, writes were never ported.
//!
//! The split: `parse` is the text work (slug, heading/body, word counts,
//! the tally), `editors` is which "Open in …" the box actually has,
//! `render` is the row objects and their action lists.

mod editors;
mod parse;
mod render;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::{Ctx, NativeExt, NativeOutcome, process};
use crate::settings::Settings;
use crate::settings::paths;
use crate::support::quote::quote;

use self::render::Editor;

pub struct Note;

/// Each subprocess probe gets its own small bound — a native run has no
/// worker deadline.
const CALL: Duration = Duration::from_secs(2);

/// How long a just-saved note keeps saying so — the script's
/// `FRESH_SECONDS`. The launcher re-asks a few times after a save; this
/// only has to outlast that.
const FRESH_SECONDS: i64 = 30;

/// `$NOTES_DIR`, then `notesDir` in oxy.json, then `~/Documents/Notes` —
/// the script's order, with a leading `~` expanded to `$HOME`. A `notesDir`
/// that is not a string reads as absent rather than as a JSON-shaped path.
fn notes_dir(settings: &Settings) -> String {
    let dir = std::env::var("NOTES_DIR")
        .ok()
        .filter(|d| !d.is_empty())
        .or_else(|| {
            settings
                .raw
                .get("notesDir")
                .and_then(|v| v.as_str())
                .filter(|d| !d.is_empty())
                .map(String::from)
        })
        .unwrap_or_else(|| "~/Documents/Notes".to_string());
    match dir.strip_prefix('~') {
        Some(rest) => format!("{}{}", paths::home().to_string_lossy(), rest),
        None => dir,
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The note `--save` wrote a moment ago, while it is still the answer to
/// "did that work". The stamp file's own mtime is the timestamp, so nothing
/// has to be parsed back; its contents are the path it wrote.
fn fresh_path(now: i64) -> String {
    let stamp = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("oxy-note-saved");
    let Some(stamped) = std::fs::metadata(&stamp)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
    else {
        return String::new();
    };
    if now - stamped >= 0 && now - stamped <= FRESH_SECONDS {
        // `$(<file)` stripped the trailing newline; a saved path never has
        // one, but a hand-made stamp might.
        std::fs::read_to_string(&stamp)
            .unwrap_or_default()
            .trim_end_matches('\n')
            .to_string()
    } else {
        String::new()
    }
}

/// `find dir -maxdepth 2 -name '*.md' -type f -printf '%T@\t%p' |
/// sort -z -rn`: every `.md` a level down included, newest first — the
/// fractional mtime is what sorts, the whole second is what the row shows.
fn list(dir: &Path, prefix: &str) -> Vec<(i64, String)> {
    let mut found: Vec<(SystemTime, i64, String)> = Vec::new();
    collect(dir, prefix, 2, &mut found);
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.2.cmp(&a.2)));
    found.into_iter().map(|(_, s, p)| (s, p)).collect()
}

fn collect(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<(SystemTime, i64, String)>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let shown = format!("{prefix}/{name}");
        // `-type f` without -L: a symlink is neither a file nor a descended
        // directory, the same exclusion find applies.
        if kind.is_dir() {
            collect(&entry.path(), &shown, depth - 1, out);
        } else if kind.is_file() && name.ends_with(".md") {
            let Some(mtime) = entry.metadata().ok().and_then(|m| m.modified().ok()) else {
                continue;
            };
            let secs = mtime
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            out.push((mtime, secs, shown));
        }
    }
}

/// `basename "$file" .md` — the last component minus the suffix. GNU keeps
/// the name when stripping would empty it, so `.md` is its own basename.
fn basename_stem(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.strip_suffix(".md")
        .filter(|s| !s.is_empty())
        .unwrap_or(name)
        .to_string()
}

/// One `date` for every stamp on screen: `date -f` reads a line each, so
/// the whole list's `+%-d %b %Y` is a single fork where the script spent
/// one per row — and the month names stay the locale's own. A `date` that
/// answers the wrong number of lines, or none, falls back to the UTC civil
/// day rather than misaligning the rows.
async fn date_batch(stamps: &[i64]) -> Vec<String> {
    if stamps.is_empty() {
        return Vec::new();
    }
    let args = stamps
        .iter()
        .map(|t| format!("@{t}"))
        .collect::<Vec<_>>()
        .join(" ");
    let cmd = format!("date -f <(printf '%s\\n' {args}) '+%-d %b %Y'");
    if let Some(fin) = process::run(&cmd, CALL).await {
        let lines: Vec<String> = fin.stdout.lines().map(|l| l.to_string()).collect();
        if lines.len() == stamps.len() {
            return lines;
        }
    }
    stamps.iter().map(|t| parse::stamp_date_utc(*t)).collect()
}

/// One note that survived the query, ready to become a row.
struct NoteItem {
    file: String,
    modified: i64,
    title: String,
    body: String,
    words: usize,
    base_score: i64,
    /// The running `matched` count at the moment the row was built — the
    /// score's `- matched` half, which orders the list.
    rank: usize,
}

/// Everything the filesystem had to say, gathered in one blocking pass.
struct Scan {
    /// The query's slug — "" when there is nothing to name a note after.
    name: String,
    /// `$dir/$name.md` already exists: the write row opens, it does not save.
    target_exists: bool,
    /// The just-saved note's path, while it still counts.
    fresh_path: String,
    /// Every `.md` the find saw — the tally's denominator.
    total: usize,
    /// Every one the query kept, past the row limit included — the tally's
    /// numerator and each row's `rank`.
    matched: usize,
    /// The first `ROW_LIMIT` matches, newest first.
    notes: Vec<NoteItem>,
}

/// The read half of the script's main loop: list, parse, match, hold.
fn scan(dir: &str, arg: &str, now: i64) -> Scan {
    let name = if arg.is_empty() {
        String::new()
    } else {
        parse::slug(arg)
    };
    // `-f "$dir/$name.md"` — the script's own untrimmed join.
    let target_exists = !name.is_empty() && Path::new(&format!("{dir}/{name}.md")).is_file();
    let mut scan = Scan {
        name,
        target_exists,
        fresh_path: fresh_path(now),
        total: 0,
        matched: 0,
        notes: Vec::new(),
    };

    for (modified, file) in list(Path::new(dir), dir.trim_end_matches('/')) {
        scan.total += 1;
        let base = basename_stem(&file);
        // grep, sed and wc each read the file in the script — a note that
        // cannot be read still got its row, named after the file.
        let text = std::fs::read(&file)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let p = parse::parse(&text, &base);
        let Some(base_score) = parse::match_score(arg, &p.title, &base, &p.body) else {
            continue;
        };
        scan.matched += 1;
        // Past what the card could ever draw, a match is only worth
        // counting — the tally still knows it is there.
        if scan.matched > parse::ROW_LIMIT {
            continue;
        }
        scan.notes.push(NoteItem {
            file,
            modified,
            title: p.title,
            body: p.body,
            words: p.words,
            base_score,
            rank: scan.matched,
        });
    }
    scan
}

/// Rows from what the scan found — the script's `out` array in order: the
/// write row first, then the notes, then the tally rides on the first.
fn assemble(
    dir: &str,
    arg: &str,
    now: i64,
    scan: Scan,
    editors: &[Editor],
    details: Vec<String>,
) -> NativeOutcome {
    let mut out: Vec<Value> = Vec::new();
    let mut fresh_seen = false;

    // The row that writes — first whenever there is anything to name a
    // note after, and honest about which of the two things it will do:
    // a name that already exists opens, it does not "save" over.
    if !scan.name.is_empty() {
        let target = format!("{dir}/{}.md", scan.name);
        if scan.target_exists {
            let acts = render::note_actions(&target, editors);
            out.push(render::row(
                &format!("new:{}", scan.name),
                &format!("Open \u{201c}{arg}\u{201d}"),
                "This note already exists",
                &format!("{}.md", scan.name),
                "",
                0,
                "open",
                &render::first_exec(&acts),
                99000,
                acts,
                false,
            ));
        } else {
            let acts = render::new_actions(arg, editors);
            out.push(render::row(
                &format!("new:{}", scan.name),
                arg,
                "Save as a new note",
                &format!("{}.md", scan.name),
                "",
                0,
                "new",
                &format!("oxy-note --save {}", quote(arg)),
                99000,
                acts,
                false,
            ));
        }
    }

    for (i, note) in scan.notes.iter().enumerate() {
        let detail = details
            .get(i)
            .cloned()
            .unwrap_or_else(|| parse::stamp_date_utc(note.modified));
        // The one that was just saved says so where the date goes, and the
        // view gives it the accent.
        let fresh = !scan.fresh_path.is_empty() && note.file == scan.fresh_path;
        let stamp = if fresh {
            "Saved just now".to_string()
        } else {
            parse::ago(now, note.modified, &detail)
        };
        fresh_seen |= fresh;
        let acts = render::note_actions(&note.file, editors);
        out.push(render::row(
            &note.file,
            &note.title,
            &stamp,
            &detail,
            &parse::excerpt(&note.body),
            note.words,
            "note",
            &render::first_exec(&acts),
            note.base_score - note.rank as i64,
            acts,
            fresh,
        ));
    }

    // Nothing at all — no notes on disk and nothing typed to make one
    // from. A mode that answers with a blank card reads as broken, so it
    // says what it is and offers the only thing there is to do here.
    // With notes on disk and no rows to show, the script printed nothing.
    if out.is_empty() {
        if scan.total > 0 {
            return NativeOutcome::Empty;
        }
        let qd = quote(dir);
        return NativeOutcome::Rows(vec![render::row(
            "empty",
            "No notes yet",
            "Type to write one",
            dir,
            "",
            0,
            "empty",
            &format!("nautilus {qd}"),
            99000,
            vec![json!({
                "title": "Open Notes Folder",
                "shortcut": "↵",
                "exec": format!("nautilus {qd}"),
            })],
            false,
        )]);
    }

    // The count rides on the first row — the only one the view reads it
    // from.
    let tally = parse::tally(scan.total, scan.matched, !arg.is_empty(), fresh_seen);
    if let Some(first) = out.first_mut().and_then(|r| r.as_object_mut()) {
        first.insert("tally".to_string(), json!(tally));
    }
    NativeOutcome::Rows(out)
}

impl NativeExt for Note {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move {
            // The manifest has no `when` — the script answered
            // unconditionally, so the port does too.
            let dir = notes_dir(&ctx.settings);
            // `note:` arguments arrive trimmed of surrounding space, the
            // way the script trimmed `"${1:-}"`.
            let arg = ctx.arg.trim().to_string();
            let now = now_secs();
            let omarchy = editors::omarchy_editor();
            let mime = editors::mime_editor().await;

            // The disk part — the walk and the per-file reads — is
            // synchronous work; it runs off the async threads. A scan that
            // somehow panicked is no answer, not an empty folder.
            let (d2, a2) = (dir.clone(), arg.clone());
            let scan = match tokio::task::spawn_blocking(move || scan(&d2, &a2, now)).await {
                Ok(s) => s,
                Err(_) => return NativeOutcome::Empty,
            };

            let stamps: Vec<i64> = scan.notes.iter().map(|n| n.modified).collect();
            let details = date_batch(&stamps).await;
            let editors = editors::available(&omarchy, &mime);
            assemble(&dir, &arg, now, scan, &editors, details)
        })
    }
}
