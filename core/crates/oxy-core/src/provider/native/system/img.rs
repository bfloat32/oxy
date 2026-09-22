//! `img` — the image search: an in-process walk over the usual picture
//! folders (or the `in:` root), newest first, with dimensions only when
//! `identify` is installed. What was a `fd` call is a `WalkBuilder` here,
//! the same crate `file` walks with.
//!
//! The script's observable shape is kept: at most sixty rows, newest first,
//! out of a three-hundred-wide pool; `identify` is probed per row and only
//! when it exists, because a missing chip reads better than a slow search.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::native::system::file::file_url;
use crate::provider::native::util::on_path;
use crate::provider::process;
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::support::quote::quote;

/// fd's `--extension` list, verbatim.
const EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "avif", "bmp", "svg", "tiff", "heic",
];

/// fd's `--exclude` list. WalkBuilder already honors .gitignore; these are
/// skipped by name no matter what a gitignore says.
const EXCLUDE: &[&str] = &[".git", "node_modules", ".cache", ".thumbnails"];

/// `--max-results 300`: the pool the newest-first sort draws from. fd stops
/// there, so the walk does too.
const MAX_WALK: usize = 300;
/// `head -60`.
const MAX_ROWS: usize = 60;
/// Matches are capped by MAX_WALK, but a needle that hits nothing would
/// still read every dirent under ~. Bounding *visited* entries keeps a
/// keystroke's worst case a large-but-finite scan — the same bound `file`
/// walks with.
const MAX_VISITED: usize = 100_000;
/// One `identify` probe's deadline — the script had none, but a wedged
/// ImageMagick is an empty detail either way, not a hung answer.
const IDENTIFY_TIMEOUT: Duration = Duration::from_secs(2);
/// Probes run concurrently — the script's serial loop is sixty subprocesses
/// deep on a full answer — but not all at once.
const IDENTIFY_AT_ONCE: usize = 8;

#[derive(Default)]
pub struct Img;

/// One walked file that survived every filter, carrying the two facts the
/// row needs that the dirent did not hand over.
struct Hit {
    path: String,
    /// `stat -c %Y`; 0 when the file vanishes between walk and stat, which
    /// is also what the script's `|| echo 0` produced.
    mtime: i64,
    size: u64,
}

/// Where the walk looks. `in:~/work` names the only root — but only when it
/// is a directory; anything else falls through to the usual picture folders,
/// and a machine with none of them reads the home directory whole.
fn roots(home: &Path, in_filter: Option<&str>) -> Vec<PathBuf> {
    if let Some(root) = in_filter.filter(|r| !r.is_empty()) {
        // `${root/#\~/$HOME}`: only a leading tilde expands.
        let expanded = match root.strip_prefix('~') {
            Some(rest) => format!("{}{rest}", home.to_string_lossy()),
            None => root.to_string(),
        };
        let dir = PathBuf::from(expanded);
        if dir.is_dir() {
            return vec![dir];
        }
    }
    let mut roots: Vec<PathBuf> = ["Pictures", "Downloads", "Desktop", "Documents"]
        .iter()
        .map(|d| home.join(d))
        .filter(|d| d.is_dir())
        .collect();
    if roots.is_empty() {
        roots.push(home.to_path_buf());
    }
    roots
}

/// fd's `--exclude` is a name match at any depth.
fn is_excluded(path: &Path, root: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    rel.components()
        .any(|c| EXCLUDE.contains(&c.as_os_str().to_string_lossy().as_ref()))
}

/// The walk: fd's `--type f --follow --fixed-strings --ignore-case` plus the
/// extension and exclude filters, capped at the same 300 fd was. No
/// `--hidden` was passed, so WalkBuilder's default (skip dotfiles) stands.
fn walk(roots: &[PathBuf], query_lower: &str) -> Vec<Hit> {
    let mut found: Vec<Hit> = Vec::new();
    let mut visited = 0usize;
    'roots: for root in roots {
        let mut builder = ignore::WalkBuilder::new(root);
        builder.follow_links(true);
        // `--exclude` prunes: a `.git` or `node_modules` tree is never
        // descended into, so its entries cannot burn the visited budget.
        let prune_root = root.clone();
        builder.filter_entry(move |e| {
            if e.file_type().is_some_and(|t| t.is_dir()) {
                !is_excluded(e.path(), &prune_root)
            } else {
                true
            }
        });
        for entry in builder.build().flatten() {
            visited += 1;
            if visited > MAX_VISITED {
                break 'roots;
            }
            let path = entry.path();
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            if is_excluded(path, root) {
                continue;
            }
            let base = match path.file_name() {
                Some(n) => n.to_string_lossy(),
                None => continue,
            };
            if !base.to_lowercase().contains(query_lower) {
                continue;
            }
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if !EXTENSIONS.contains(&ext.as_str()) {
                continue;
            }
            // `[[ -e $path ]] || continue` — the script's newline-in-name
            // guard. The stat is also where `sort -rn`'s key comes from.
            let Ok(meta) = std::fs::metadata(path) else {
                continue;
            };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            found.push(Hit {
                path: path.to_string_lossy().into_owned(),
                mtime,
                size: meta.len(),
            });
            if found.len() >= MAX_WALK {
                break 'roots;
            }
        }
    }

    // `sort -rn` on `mtime\x1fpath`: newest first, and GNU sort's
    // last-resort whole-line compare puts equal mtimes in reverse
    // path order.
    found.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| b.path.cmp(&a.path)));
    found.truncate(MAX_ROWS);
    found
}

/// `numfmt --to=iec --suffix=B`. numfmt's default rounding is `from-zero` —
/// for byte counts, a plain ceiling — taken at one decimal place when the
/// scaled value is under ten, and a "999.9" that rounds into the next unit
/// rescales (1048575 bytes reads "1.0MB", not "1024KB").
fn human_iec(bytes: u64) -> String {
    const UNITS: [&str; 11] = ["", "K", "M", "G", "T", "P", "E", "Z", "Y", "R", "Q"];
    let mut power = 0usize;
    let mut val = bytes as f64;
    while val >= 1024.0 && power + 1 < UNITS.len() {
        val /= 1024.0;
        power += 1;
    }
    let decimals = usize::from(val < 10.0);
    let factor = 10f64.powi(decimals as i32);
    val = (val * factor).ceil() / factor;
    if val >= 1024.0 {
        val /= 1024.0;
        power += 1;
    }
    if val != 0.0 && val < 10.0 && power > 0 {
        format!("{val:.1}{}B", UNITS[power])
    } else {
        format!("{val:.0}{}B", UNITS[power.min(UNITS.len() - 1)])
    }
}

/// One row, the jq object verbatim: `id` the path, `title` its basename,
/// `subtitle` the folder with a leading $HOME folded to ~, `art` a file://
/// URL, `detail` the identify dimensions ("" without it), `accessory` the
/// iec size, `exec` the opener, `score` mtime % 90000.
fn row(hit: &Hit, home: &str, dimensions: String) -> Value {
    let path = Path::new(&hit.path);
    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| hit.path.clone());
    // `dir=${path%/*}` — everything before the last separator, the whole
    // string when there is none, and "" for a file at the root: `Path`'s
    // `parent()` would say "/" where the script said "".
    let dir = match hit.path.rfind('/') {
        Some(i) => hit.path[..i].to_string(),
        None => hit.path.clone(),
    };
    // `${dir/#$HOME/\~}` — anchored: only a leading $HOME folds.
    let subtitle = match dir.strip_prefix(home) {
        Some(rest) => format!("~{rest}"),
        None => dir,
    };

    let exec = format!("xdg-open {}", quote(&hit.path));
    json!({
        "id": hit.path,
        "title": base,
        "subtitle": subtitle,
        "art": file_url(&hit.path),
        "detail": dimensions,
        "accessory": human_iec(hit.size),
        "exec": exec,
        "score": hit.mtime % 90000,
        "actions": [
            { "title": "Open", "shortcut": "↵", "exec": exec },
            { "title": "Copy Image",
              "exec": format!("wl-copy < {}", quote(&hit.path)) },
            { "title": "Copy Path",
              "exec": format!("printf %s {} | wl-copy", quote(&hit.path)) },
            { "title": "Reveal in Files",
              "exec": format!("nautilus --select {}", quote(&hit.path)) },
        ],
    })
}

impl NativeExt for Img {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        let query_lower = ctx.arg.trim().to_lowercase();
        let in_filter = ctx.filters.get("in").cloned();
        Box::pin(async move {
            let home = crate::settings::paths::home();
            let roots = roots(&home, in_filter.as_deref());
            // The walk is blocking IO; keep it off the reactor thread.
            let hits = tokio::task::spawn_blocking(move || walk(&roots, &query_lower))
                .await
                .unwrap_or_default();

            // `identify -format '%wx%h' "$path[0]"` per row — `[0]` is
            // ImageMagick's first-frame syntax, kept inside the quotes —
            // and only when identify exists at all. The probes run
            // concurrently but bounded, where the script ran them one at
            // a time inside the row loop.
            let have_identify = on_path("identify");
            let permits = Arc::new(tokio::sync::Semaphore::new(IDENTIFY_AT_ONCE));
            let probes: Vec<Option<tokio::task::JoinHandle<String>>> = hits
                .iter()
                .map(|hit| {
                    if !have_identify {
                        return None;
                    }
                    let probe = format!(
                        "identify -format '%wx%h' {}",
                        quote(&format!("{}[0]", hit.path))
                    );
                    let permits = permits.clone();
                    Some(tokio::spawn(async move {
                        let _permit = permits.acquire_owned().await;
                        process::run(&probe, IDENTIFY_TIMEOUT)
                            .await
                            .map(|fin| fin.stdout.trim().to_string())
                            .unwrap_or_default()
                    }))
                })
                .collect();

            let home = home.to_string_lossy().into_owned();
            let mut rows = Vec::with_capacity(hits.len());
            for (hit, probe) in hits.iter().zip(probes) {
                let dimensions = match probe {
                    Some(handle) => handle.await.unwrap_or_default(),
                    None => String::new(),
                };
                rows.push(row(hit, &home, dimensions));
            }
            NativeOutcome::Rows(rows)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// numfmt --to=iec --suffix=B, checked against the real thing:
    /// from-zero rounding is a ceiling here, one decimal under ten scaled,
    /// and a rounded-up 1024 rescales into the next unit.
    #[test]
    fn sizes_match_numfmt() {
        let cases: &[(u64, &str)] = &[
            (0, "0B"),
            (1, "1B"),
            (512, "512B"),
            (1023, "1023B"),
            (1024, "1.0KB"),
            (1025, "1.1KB"),
            (1536, "1.5KB"),
            (2048, "2.0KB"),
            (10000, "9.8KB"),
            (10239, "10KB"),
            (10240, "10KB"),
            (102400, "100KB"),
            (999999, "977KB"),
            (1048575, "1.0MB"),
            (1048576, "1.0MB"),
            (1234567, "1.2MB"),
            (1073741824, "1.0GB"),
            (5000000000, "4.7GB"),
        ];
        for (bytes, want) in cases {
            assert_eq!(human_iec(*bytes), *want, "{bytes} bytes");
        }
    }

    #[test]
    fn the_row_is_the_jq_object() {
        let hit = Hit {
            path: "/home/u/Pictures/cat #2?.jpg".to_string(),
            mtime: 1_700_000_123,
            size: 1_234_567,
        };
        let row = row(&hit, "/home/u", "1200x800".to_string());
        let exec = "xdg-open '/home/u/Pictures/cat #2?.jpg'";
        assert_eq!(
            row,
            json!({
                "id": "/home/u/Pictures/cat #2?.jpg",
                "title": "cat #2?.jpg",
                "subtitle": "~/Pictures",
                "art": "file:///home/u/Pictures/cat%20%232%3F.jpg",
                "detail": "1200x800",
                "accessory": "1.2MB",
                "exec": exec,
                "score": 1_700_000_123 % 90000,
                "actions": [
                    { "title": "Open", "shortcut": "↵", "exec": exec },
                    { "title": "Copy Image",
                      "exec": "wl-copy < '/home/u/Pictures/cat #2?.jpg'" },
                    { "title": "Copy Path",
                      "exec": "printf %s '/home/u/Pictures/cat #2?.jpg' | wl-copy" },
                    { "title": "Reveal in Files",
                      "exec": "nautilus --select '/home/u/Pictures/cat #2?.jpg'" },
                ],
            })
        );
    }

    #[test]
    fn a_dir_outside_home_stays_absolute() {
        let hit = Hit {
            path: "/tmp/x.png".to_string(),
            mtime: 5,
            size: 1,
        };
        assert_eq!(row(&hit, "/home/u", String::new())["subtitle"], "/tmp");
    }

    /// A directory tree with the traps the walk must honor: an image in a
    /// skipped dir, a non-image, a dotfile, and the newest file named second.
    struct Fixture(PathBuf);
    impl Fixture {
        fn new(name: &str) -> Fixture {
            let dir =
                std::env::temp_dir().join(format!("oxy-img-test-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("sub")).unwrap();
            std::fs::create_dir_all(dir.join("node_modules")).unwrap();
            Fixture(dir)
        }
        fn write(&self, name: &str, mtime: u64) {
            let f = std::fs::File::create(self.0.join(name)).unwrap();
            f.set_modified(UNIX_EPOCH + Duration::from_secs(mtime))
                .unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn walk_finds_images_newest_first() {
        let fx = Fixture::new("walk");
        fx.write("old.jpg", 1_000);
        fx.write("sub/new.png", 2_000);
        fx.write("note.txt", 3_000);
        fx.write("node_modules/skip.png", 4_000);
        fx.write(".hidden.gif", 5_000);
        let hits = walk(std::slice::from_ref(&fx.0), "");
        let names: Vec<String> = hits
            .iter()
            .map(|h| {
                Path::new(&h.path)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["new.png", "old.jpg"]);
        assert!(hits[0].mtime > hits[1].mtime);
    }

    #[test]
    fn walk_filters_by_name_case_insensitively() {
        let fx = Fixture::new("filter");
        fx.write("Cat.JPG", 1_000);
        fx.write("dog.png", 2_000);
        let hits = walk(std::slice::from_ref(&fx.0), "cat");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].path.ends_with("Cat.JPG"));
    }

    #[test]
    fn roots_prefer_the_in_filter_then_the_usual_folders() {
        let fx = Fixture::new("roots");
        let home = fx.0.join("home");
        std::fs::create_dir_all(home.join("Pictures")).unwrap();
        let elsewhere = fx.0.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();

        // `in:` on a real dir is the only root — even with Pictures there.
        let r = roots(&home, Some(elsewhere.to_str().unwrap()));
        assert_eq!(r, vec![elsewhere.clone()]);

        // `~` expands against home.
        let r = roots(&home, Some("~/Pictures"));
        assert_eq!(r, vec![home.join("Pictures")]);

        // A bogus `in:` falls through to whichever usual folders exist.
        let r = roots(&home, Some("~/no-such-place"));
        assert_eq!(r, vec![home.join("Pictures")]);

        // And with none of them, home itself.
        let bare = fx.0.join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        assert_eq!(roots(&bare, None), vec![bare]);
    }
}
