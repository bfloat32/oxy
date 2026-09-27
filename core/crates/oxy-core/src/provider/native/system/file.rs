//! `file:` — anything called this, under here — a port of `oxy-search-files`.
//!
//! The original walked with `fd` and ranked in awk; here the `ignore` crate
//! walks the tree directly, which is the same crate `fd` is built on. The
//! exclusions, the scoring, the kind-by-extension table and the row shape are
//! the same, so the `files` view cannot tell the difference.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::native::util::{human_size, short_age};
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::support::quote::quote;

/// The dirs fd was told to skip. `.git` and `node_modules` are where files go
/// to be found by the thousand and wanted by nobody.
const EXCLUDE: &[&str] = &[
    ".git",
    "node_modules",
    ".cache",
    ".cargo",
    ".npm",
    ".rustup",
    ".local/share/Trash",
    ".venv",
    "target",
];

/// The cap on the walk: the first N hits are traversal order, not relevance,
/// so the slice is wide and the ranking happens here.
const MAX_WALK: usize = 400;
const MAX_ROWS: usize = 20;
/// Matches cap the result list, but a needle that hits nothing would still
/// read every dirent under ~. Bounding *visited* entries keeps a keystroke's
/// worst case proportional to a large-but-finite tree scan, not the whole
/// home directory.
const MAX_VISITED: usize = 100_000;

/// What kind of thing this is, from the extension alone. Reading magic bytes
/// would be more honest and would cost a syscall per row; the extension is
/// what the user typed and what every file manager goes by anyway.
fn kind_of(ext: &str) -> &'static str {
    match ext.to_lowercase().as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "bmp" | "svg" | "tiff" | "tif"
        | "heic" | "ico" => "image",
        "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" | "wmv" | "flv" => "video",
        "mp3" | "flac" | "ogg" | "wav" | "m4a" | "opus" | "aac" | "wma" => "audio",
        "pdf" | "epub" | "djvu" | "doc" | "docx" | "odt" | "rtf" | "txt" | "md" | "org" | "tex" => {
            "doc"
        }
        "csv" | "tsv" | "xlsx" | "xls" | "ods" => "sheet",
        "zip" | "tar" | "gz" | "xz" | "zst" | "7z" | "rar" | "bz2" | "tgz" => "archive",
        "sh" | "bash" | "zsh" | "fish" | "py" | "js" | "mjs" | "ts" | "tsx" | "jsx" | "rs"
        | "go" | "c" | "h" | "cpp" | "hpp" | "java" | "rb" | "lua" | "qml" | "vim" | "pl"
        | "php" | "swift" | "kt" => "code",
        "json" | "yaml" | "yml" | "toml" | "ini" | "conf" | "xml" | "html" | "css" | "scss"
        | "sql" => "data",
        _ => "file",
    }
}

/// A path as a file:// URL, canonical on both platforms: separators flip on
/// Windows so `C:\icons\x.svg` reads `file:///C:/icons/x.svg`, and the
/// characters that make a URL mean something else are percent-encoded, so
/// `report #2 (final)?.txt` draws its thumbnail.
pub(crate) fn file_url(path: &str) -> String {
    let path: std::borrow::Cow<'_, str> = if cfg!(windows) {
        path.replace('\\', "/").into()
    } else {
        path.into()
    };
    let mut out = String::with_capacity(path.len() + 9);
    out.push_str(if path.starts_with('/') {
        "file://"
    } else {
        "file:///"
    });
    for c in path.chars() {
        match c {
            '%' => out.push_str("%25"),
            '#' => out.push_str("%23"),
            '?' => out.push_str("%3F"),
            ' ' => out.push_str("%20"),
            '"' => out.push_str("%22"),
            '\t' => out.push_str("%09"),
            _ => out.push(c),
        }
    }
    out
}

fn is_excluded(path: &Path, root: &Path) -> bool {
    let rel = match path.strip_prefix(root) {
        Ok(r) => r,
        Err(_) => return false,
    };
    rel.components().any(|c| {
        let name = c.as_os_str().to_string_lossy();
        EXCLUDE.contains(&name.as_ref())
    }) || rel.to_string_lossy().contains(".local/share/Trash")
}

pub struct Files;

impl NativeExt for Files {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move {
            let query = ctx.arg.trim().to_string();
            if query.is_empty() {
                return NativeOutcome::Empty;
            }

            let format = ctx
                .filters
                .get("format")
                .map(|f| f.trim_start_matches('.').to_string())
                .unwrap_or_default();
            let root = ctx
                .filters
                .get("in")
                .map(|r| {
                    // `${root/#\~/…}` — an anchored tilde only: a `~`
                    // mid-path is a character, not a home.
                    let home = crate::settings::paths::home()
                        .to_string_lossy()
                        .into_owned();
                    let r = if let Some(rest) = r.strip_prefix("~/") {
                        format!("{home}/{rest}")
                    } else if r == "~" {
                        home
                    } else {
                        r.clone()
                    };
                    Path::new(&r).to_path_buf()
                })
                .filter(|r| r.is_dir())
                .unwrap_or_else(crate::settings::paths::home);

            // The walk is blocking IO; keep it off the reactor thread.
            let query_lower = query.to_lowercase();
            let home = crate::settings::paths::home();
            let Ok(hits) =
                tokio::task::spawn_blocking(move || walk(&root, &query_lower, &format, &home))
                    .await
            else {
                // A panic inside the walk is not "no files" — the script leg
                // gets its own bounded try.
                return NativeOutcome::Fallback;
            };

            if hits.is_empty() {
                return NativeOutcome::Empty;
            }

            let rows: Vec<Value> = hits
                .into_iter()
                .map(|hit| {
                    let exec = format!("xdg-open {}", quote(&hit.path));
                    json!({
                        "id": hit.path,
                        "title": hit.base,
                        "dir": hit.dir,
                        "ext": hit.ext,
                        "kind": hit.kind,
                        "size": hit.size,
                        "age": hit.age,
                        "art": hit.art,
                        "copyText": hit.path,
                        "subtitle": hit.dir,
                        "accessory": hit.size,
                        "exec": exec,
                        "score": hit.score,
                        "actions": [
                            { "title": "Open", "shortcut": "↵", "exec": exec },
                            { "title": "Copy Path", "exec": format!(
                                "printf %s {} | wl-copy", quote(&hit.path)) },
                            { "title": "Open Folder", "exec": format!(
                                "xdg-open {}", quote(&hit.parent)) },
                            { "title": "Reveal in Files", "exec": format!(
                                "nautilus --select {}", quote(&hit.path)) },
                        ],
                    })
                })
                .collect();
            NativeOutcome::Rows(rows)
        })
    }
}

struct Hit {
    path: String,
    base: String,
    dir: String,
    ext: String,
    kind: String,
    size: String,
    age: String,
    art: String,
    parent: String,
    score: i64,
}

fn walk(root: &Path, query_lower: &str, format: &str, home: &Path) -> Vec<Hit> {
    use ignore::WalkBuilder;

    let mut builder = WalkBuilder::new(root);
    builder.hidden(false).follow_links(true).git_ignore(true);
    // fd's `--exclude` prunes, it does not filter: a `node_modules` tree
    // never gets descended into at all. Filtering only the files would burn
    // the whole visited budget walking their contents.
    let prune_root = root.to_path_buf();
    builder.filter_entry(move |e| {
        if e.file_type().is_some_and(|t| t.is_dir()) {
            !is_excluded(e.path(), &prune_root)
        } else {
            true
        }
    });

    let mut scored: Vec<(i64, String, String)> = Vec::new();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut visited = 0usize;
    for entry in builder.build().flatten() {
        visited += 1;
        if visited > MAX_VISITED {
            break;
        }
        let path = entry.path();
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if is_excluded(path, root) {
            continue;
        }
        let base = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !base.to_lowercase().contains(query_lower) {
            continue;
        }
        if !format.is_empty() {
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if ext != format.to_lowercase() {
                continue;
            }
        }

        // Depth is a decent proxy for "the one you meant": a file you named
        // sits nearer the top than a build artefact buried in a dependency
        // tree.
        let depth = path
            .strip_prefix(root)
            .map(|r| r.components().count())
            .unwrap_or(1);
        let mut score = 60000 - (depth as i64 * 1500);
        let lower = base.to_lowercase();
        match lower.find(query_lower) {
            Some(0) => score += 25000,
            Some(at) => score += 12000 - (at as i64 * 200),
            None => {}
        }
        if score < 1000 {
            score = 1000;
        }

        scored.push((score, base, path.to_string_lossy().into_owned()));
        if scored.len() >= MAX_WALK {
            // The walk is bounded so a home dir with a million files does not
            // spend the keystroke on them.
            break;
        }
    }

    scored.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    scored.truncate(MAX_ROWS);

    scored
        .into_iter()
        .filter_map(|(score, base, path)| {
            let meta = std::fs::metadata(&path).ok()?;
            let size = human_size(meta.len());
            let age = short_age(
                now.saturating_sub(
                    meta.modified()
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(now),
                ) as i64,
            );

            // The script's `?*.*` glob: a dot after the first character is
            // an extension — ".bashrc" has none, ".config.json" has JSON.
            let after_first = base
                .char_indices()
                .nth(1)
                .map(|(i, _)| &base[i..])
                .unwrap_or("");
            let ext = if after_first.contains('.') {
                let e = base.rsplit('.').next().unwrap_or("");
                if e.len() > 5 {
                    String::new()
                } else {
                    e.to_uppercase()
                }
            } else {
                String::new()
            };
            let kind = kind_of(&ext);
            let art = if kind == "image" {
                file_url(&path)
            } else {
                String::new()
            };
            let display = path.replacen(&home.to_string_lossy().into_owned(), "~", 1);
            let dir = display
                .rfind('/')
                .map(|i| display[..i].to_string())
                .unwrap_or_else(|| display.clone());
            let parent = path
                .rfind('/')
                .map(|i| path[..i].to_string())
                .unwrap_or_else(|| path.clone());

            Some(Hit {
                path,
                base,
                dir,
                ext,
                kind: kind.to_string(),
                size,
                age,
                art,
                parent,
                score,
            })
        })
        .collect()
}
