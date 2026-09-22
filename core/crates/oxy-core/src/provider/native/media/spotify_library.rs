//! `sp:` — the authenticated Spotify catalogue: tracks, albums, artists.
//!
//! A port of the search half of `bin/oxy-spotify`:
//!
//!   sp:kind of blue                tracks first, then albums, then artists
//!   sp:kind of blue type:album     albums only
//!   sp:miles davis type:artist     artists only
//!
//! `play` and `queue` stay the script's job — the rows' exec strings still
//! call `oxy-spotify ...`, and `oxy-spotify-auth` is untouched: this port only
//! reads the credentials it wrote and refreshes them the way the script does.
//!
//! Credentials live in `$XDG_STATE_HOME/omarchy/oxy-spotify.json`, not in the
//! committed-and-shared config — the file is a live session for whoever can
//! read it. Every call to Spotify hands curl its options on stdin rather than
//! on a command line: a bearer token in argv is readable by `ps` from any
//! account on the box for as long as the request runs.
//!
//! One deliberate difference from the script: where the script answers a 401
//! or a dead refresh token with silence, this answers with a row that says to
//! run `oxy-spotify-auth` — a keyword that goes quiet reads as broken rather
//! than signed out.

use std::cmp::Ordering;
use std::fs::OpenOptions;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::native::media::{jq_str, now_secs};
use crate::provider::native::util::on_path;
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::settings::{paths, url_encode};
use crate::support::quote::quote;

pub struct SpotifyLibrary;

const API: &str = "https://api.spotify.com/v1";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
/// The script the rows still call for `play` and `queue`.
const SELF: &str = "oxy-spotify";

fn state_file() -> PathBuf {
    paths::state_home().join("omarchy/oxy-spotify.json")
}

fn lock_path() -> PathBuf {
    let mut p = state_file().into_os_string();
    p.push(".lock");
    PathBuf::from(p)
}

// ------------------------------------------------------------------ state

struct Creds {
    client_id: String,
    access_token: String,
    refresh_token: String,
    expires_at: u64,
    scope: String,
}

impl Creds {
    /// `fresh_enough`: a token with a minute left is spent for our purposes.
    fn fresh(&self) -> bool {
        !self.access_token.is_empty() && now_secs() + 60 < self.expires_at
    }
}

/// `load_state` — the five fields as strings, `expires_at` digits-or-0, and
/// missing client id or refresh token means there is nothing to search with.
fn load_state() -> Option<Creds> {
    let text = std::fs::read_to_string(state_file()).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    // jq's `(.expires_at // 0) | tostring` then `^[0-9]+$`: an integral float
    // prints as digits and passes; a fraction or a non-number reads as 0.
    let expires_at = match v.get("expires_at") {
        Some(Value::Number(n)) => n
            .as_u64()
            .or_else(|| {
                n.as_f64()
                    .filter(|f| f.fract() == 0.0 && *f >= 0.0)
                    .map(|f| f as u64)
            })
            .unwrap_or(0),
        Some(Value::String(s)) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
            s.parse().unwrap_or(0)
        }
        _ => 0,
    };
    let creds = Creds {
        client_id: jq_str(v.get("client_id")),
        access_token: jq_str(v.get("access_token")),
        refresh_token: jq_str(v.get("refresh_token")),
        expires_at,
        scope: jq_str(v.get("scope")),
    };
    if creds.client_id.is_empty() || creds.refresh_token.is_empty() {
        return None;
    }
    Some(creds)
}

/// `save_state` — a temp file in the same directory, mode 600, renamed over.
/// Stated rather than inherited: mktemp honours a umask the caller does not
/// control, and a token file that is group readable is a token file that
/// leaks.
fn save_state(creds: &Creds) -> bool {
    let file = state_file();
    let Some(dir) = file.parent() else {
        return false;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        file.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    let body = json!({
        "client_id": creds.client_id,
        "access_token": creds.access_token,
        "refresh_token": creds.refresh_token,
        "expires_at": creds.expires_at,
        "scope": creds.scope,
    });
    let written = std::fs::write(&tmp, body.to_string()).is_ok();
    #[cfg(unix)]
    let written = written && {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).is_ok()
    };
    if !written {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    if std::fs::rename(&tmp, &file).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    true
}

// ------------------------------------------------------------------- http

/// `curl -K - --max-time 10 -w '\n%{http_code}'` — the config comes on stdin
/// so the bearer token is never an argument. Direct spawn rather than
/// `process::run`: the config has to be *written* to the child, which the
/// shared runner cannot do; the `on_path("curl")` gate keeps the spawn honest.
async fn curl_stdin(config: &str) -> Option<String> {
    let mut child = tokio::process::Command::new("curl")
        .args(["-K", "-", "--max-time", "10", "-w", "\\n%{http_code}"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;

    if let Some(mut stdin) = child.stdin.take() {
        let config = config.to_string();
        // The config is a few lines — pipe-buffer sized. A curl that died
        // early just breaks the write, which is a failed call either way.
        tokio::spawn(async move {
            let _ = stdin.write_all(config.as_bytes()).await;
            // Dropping stdin is the EOF `-K -` waits for.
        });
    }

    let mut stdout = child.stdout.take()?;
    let read = async move {
        let mut buf = Vec::with_capacity(4096);
        stdout.read_to_end(&mut buf).await.ok().map(|_| buf)
    };
    match tokio::time::timeout(Duration::from_secs(12), read).await {
        Ok(Some(buf)) => Some(String::from_utf8_lossy(&buf).into_owned()),
        _ => {
            let _ = child.kill().await;
            None
        }
    }
}

/// `api()` — every call to Spotify goes through here. Returns the http code
/// and the body: stdout is `body \n status` and the status rides the last
/// line.
async fn api(method: &str, url: &str, token: &str, body: Option<&str>) -> Option<(u16, String)> {
    let mut config = format!(
        "silent\nrequest = \"{method}\"\nurl = \"{url}\"\nheader = \"Authorization: Bearer {token}\"\n"
    );
    if let Some(body) = body.filter(|b| !b.is_empty()) {
        // The JSON body goes inside a quoted curl `data` line: backslashes
        // and quotes are the two bytes that would end it early.
        let escaped = body.replace('\\', "\\\\").replace('"', "\\\"");
        config.push_str("header = \"Content-Type: application/json\"\n");
        config.push_str(&format!("data = \"{escaped}\"\n"));
    }
    let out = curl_stdin(&config).await?;
    let (body, status) = out.rsplit_once('\n')?;
    Some((status.trim().parse().ok()?, body.to_string()))
}

enum Refresh {
    Renewed,
    /// The grant itself is dead — 400/401 — re-auth is the only way back.
    Expired,
    /// The call never got a usable answer: offline, a 5xx, an empty body.
    Unreachable,
}

/// `refresh_tokens` — PKCE refresh, same stdin route as every other call:
/// the refresh token is a longer-lived secret than the access token.
async fn refresh_tokens(creds: &mut Creds) -> Refresh {
    let config = format!(
        "silent\nrequest = \"POST\"\nurl = \"{TOKEN_URL}\"\nheader = \"Content-Type: application/x-www-form-urlencoded\"\ndata = \"grant_type=refresh_token&refresh_token={}&client_id={}\"\n",
        url_encode(&creds.refresh_token),
        url_encode(&creds.client_id)
    );
    let Some(out) = curl_stdin(&config).await else {
        return Refresh::Unreachable;
    };
    let Some((body, status)) = out.rsplit_once('\n') else {
        return Refresh::Unreachable;
    };
    let Ok(status) = status.trim().parse::<u16>() else {
        return Refresh::Unreachable;
    };
    if status == 400 || status == 401 {
        return Refresh::Expired;
    }
    if status != 200 {
        return Refresh::Unreachable;
    }
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return Refresh::Unreachable;
    };
    let new_access = jq_str(v.get("access_token"));
    if new_access.is_empty() {
        return Refresh::Unreachable;
    }
    let new_refresh = jq_str(v.get("refresh_token"));
    let new_scope = jq_str(v.get("scope"));
    let expires_in = match v.get("expires_in") {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(3600),
        Some(Value::String(s)) if s.bytes().all(|b| b.is_ascii_digit()) && !s.is_empty() => {
            s.parse().unwrap_or(3600)
        }
        _ => 3600,
    };

    creds.access_token = new_access;
    // PKCE usually returns a rotated refresh token but is not required to.
    // Keeping the old one when the field is absent is the difference between
    // a session that survives and one that silently ends at the next refresh.
    if !new_refresh.is_empty() {
        creds.refresh_token = new_refresh;
    }
    if !new_scope.is_empty() {
        creds.scope = new_scope;
    }
    creds.expires_at = now_secs() + expires_in;

    if save_state(creds) {
        Refresh::Renewed
    } else {
        Refresh::Unreachable
    }
}

enum Deny {
    /// No usable credentials — the script's silent `exit 0`.
    Missing,
    /// The session is dead and re-auth is the fix — the affordance row.
    Expired,
    /// Refresh failed transiently — answer nothing, try again next ask.
    Unreachable,
}

/// `ensure_token` — fresh enough answers immediately; otherwise the refresh
/// is serialized on `$STATE_FILE.lock`, because typing `sp:blue` fires several
/// searches and Spotify retires a refresh token the moment it is spent: two
/// unserialised refreshes race and the loser writes a token that is already
/// dead. The lock is create-new rather than flock — same contract — and a
/// stale lock is broken rather than waited on forever, because a wedged
/// holder must not take search down with it.
async fn ensure_token() -> Result<Creds, Deny> {
    let creds = load_state().ok_or(Deny::Missing)?;
    if creds.fresh() {
        return Ok(creds);
    }

    let lock = lock_path();
    let mut held = false;
    for _ in 0..40 {
        match OpenOptions::new().write(true).create_new(true).open(&lock) {
            Ok(_) => {
                held = true;
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    if !held {
        let _ = std::fs::remove_file(&lock);
        held = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .is_ok();
    }

    let out = ensure_locked().await;
    if held {
        let _ = std::fs::remove_file(&lock);
    }
    out
}

/// Inside the lock: the state is re-read first — the process that held the
/// lock may have already refreshed, making ours unnecessary.
async fn ensure_locked() -> Result<Creds, Deny> {
    let mut creds = load_state().ok_or(Deny::Missing)?;
    if creds.fresh() {
        return Ok(creds);
    }
    match refresh_tokens(&mut creds).await {
        Refresh::Renewed => Ok(creds),
        Refresh::Expired => Err(Deny::Expired),
        Refresh::Unreachable => Err(Deny::Unreachable),
    }
}

// ------------------------------------------------------------------ rows

/// jq's `x // ""`: null and false collapse to the empty string, anything
/// else passes through untouched.
fn or_empty(v: Option<&Value>) -> Value {
    match v {
        Some(v) if !v.is_null() && *v != Value::Bool(false) => v.clone(),
        _ => Value::String(String::new()),
    }
}

/// `pickart` — cards draw art at 44px, so the smallest image at or above
/// 64px is sharp on a hidpi panel without pulling a 640px jpeg per row; with
/// nothing that big, the largest there is.
fn pickart(images: Option<&Value>) -> String {
    let Some(imgs) = images.and_then(Value::as_array) else {
        return String::new();
    };
    let width = |v: &Value| v.get("width").and_then(Value::as_f64).unwrap_or(0.0);
    let by_width =
        |a: &&Value, b: &&Value| width(a).partial_cmp(&width(b)).unwrap_or(Ordering::Equal);
    let pick = imgs
        .iter()
        .filter(|i| width(i) >= 64.0)
        .min_by(by_width)
        .or_else(|| imgs.iter().max_by(by_width));
    pick.and_then(|i| i.get("url"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// `duration` — milliseconds to m:ss; anything that is not a number is 0.
fn duration(v: Option<&Value>) -> String {
    let ms = v.and_then(Value::as_f64).unwrap_or(0.0);
    let s = (ms / 1000.0).floor() as i64;
    format!("{}:{:02}", (s as f64 / 60.0).floor() as i64, s % 60)
}

/// `followers` — 1.2M, 12.3K, or the bare count.
fn followers(v: Option<&Value>) -> String {
    let n = v.and_then(Value::as_f64).unwrap_or(0.0);
    if n >= 1_000_000.0 {
        format!("{}M followers", (n / 100_000.0).floor() / 10.0)
    } else if n >= 1_000.0 {
        format!("{}K followers", (n / 100.0).floor() / 10.0)
    } else {
        // `jq`'s `\($n)` prints an integral float without its fraction.
        format!("{} followers", n)
    }
}

/// `year` — the first four characters of a date string, or nothing.
fn year(v: Option<&Value>) -> String {
    let Some(s) = v.and_then(Value::as_str) else {
        return String::new();
    };
    if s.chars().count() >= 4 {
        s.chars().take(4).collect()
    } else {
        String::new()
    }
}

/// `titlecase` — first character up, rest as found.
fn titlecase(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// `($t.id // "\($i)")` with the `track:` prefix — an id that is missing or
/// unprintable falls back to the index, never to nothing.
fn id_or_index(v: Option<&Value>, i: usize) -> String {
    v.filter(|v| !v.is_null())
        .map(|v| jq_str(Some(v)))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| i.to_string())
}

/// `[artists[].name] | join(", ")` — string names only.
fn artist_names(v: Option<&Value>) -> String {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.get("name").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// The row a dead session answers with — the script's silence, made visible:
/// a `sp:` that prints nothing reads as broken rather than signed out.
fn auth_row() -> Value {
    let exec = "omarchy-launch-tui --app-id=org.omarchy.spotify oxy-spotify-auth";
    json!({
        "id": "spotify:auth",
        "title": "Spotify login expired",
        "subtitle": "Press ↵ to sign in again",
        "view": "cards",
        "group": "Spotify",
        "score": 99999,
        "exec": exec,
        "actions": [
            {"title": "Sign in to Spotify", "shortcut": "↵", "exec": exec},
        ],
    })
}

fn track_row(t: &Value, i: usize, note: &str) -> Value {
    let uri = jq_str(t.get("uri"));
    let y = year(t.get("album").and_then(|a| a.get("release_date")));
    let album = jq_str(t.get("album").and_then(|a| a.get("name")));
    let play = format!("{SELF} play {}", quote(&uri));
    let link = jq_str(t.get("external_urls").and_then(|e| e.get("spotify")));
    json!({
        "id": format!("track:{}", id_or_index(t.get("id"), i)),
        "title": or_empty(t.get("name")),
        "subtitle": format!("{}{}", artist_names(t.get("artists")), note),
        "detail": if y.is_empty() {
            album
        } else {
            format!("{album}  ·  {y}")
        },

        "accessory": duration(t.get("duration_ms")),
        "art": pickart(t.get("album").and_then(|a| a.get("images"))),
        "view": "cards",
        "group": "Tracks",
        "score": 90000 - i as i64 * 100,
        "exec": play,
        "actions": [
            {"title": "Play", "shortcut": "↵", "exec": play},
            {"title": "Add to Queue", "exec": format!("{SELF} queue {}", quote(&uri))},
            {"title": "Open in Spotify", "exec": format!("xdg-open {}", quote(&uri))},
            {
                "title": "Copy Link",
                "exec": format!("printf %s {} | wl-copy", quote(&link)),
            },
        ],
    })
}

fn album_row(a: &Value, i: usize) -> Value {
    let uri = jq_str(a.get("uri"));
    let total = match a.get("total_tracks") {
        Some(v) if !v.is_null() => jq_str(Some(v)),
        _ => "0".to_string(),
    };
    // `.album_type // "album"` — jq's `//` collapses null and false, both of
    // which jq_str already renders as empty.
    let album_type = jq_str(a.get("album_type"));
    let album_type = if album_type.is_empty() {
        "album".to_string()
    } else {
        album_type
    };
    let play = format!("{SELF} play {}", quote(&uri));
    let link = jq_str(a.get("external_urls").and_then(|e| e.get("spotify")));
    json!({
        "id": format!("album:{}", id_or_index(a.get("id"), i)),
        "title": or_empty(a.get("name")),
        "subtitle": artist_names(a.get("artists")),
        "detail": format!("{}  ·  {total} tracks", titlecase(&album_type)),
        "accessory": year(a.get("release_date")),
        "art": pickart(a.get("images")),
        "view": "cards",
        "group": "Albums",
        "score": 60000 - i as i64 * 100,
        "exec": play,
        "actions": [
            {"title": "Play", "shortcut": "↵", "exec": play},
            {"title": "Open in Spotify", "exec": format!("xdg-open {}", quote(&uri))},
            {
                "title": "Copy Link",
                "exec": format!("printf %s {} | wl-copy", quote(&link)),
            },
        ],
    })
}

fn artist_row(r: &Value, i: usize) -> Value {
    let uri = jq_str(r.get("uri"));
    let genres = r
        .get("genres")
        .and_then(Value::as_array)
        .map(|g| {
            g.iter()
                .filter_map(Value::as_str)
                .take(2)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|s| !s.is_empty());
    let play = format!("{SELF} play {}", quote(&uri));
    let link = jq_str(r.get("external_urls").and_then(|e| e.get("spotify")));
    json!({
        "id": format!("artist:{}", id_or_index(r.get("id"), i)),
        "title": or_empty(r.get("name")),
        "subtitle": genres.unwrap_or_else(|| "Artist".to_string()),
        "detail": followers(r.get("followers").and_then(|f| f.get("total"))),
        "art": pickart(r.get("images")),
        "view": "cards",
        "group": "Artists",
        "score": 40000 - i as i64 * 100,
        "exec": play,
        "actions": [
            {"title": "Play", "shortcut": "↵", "exec": play},
            {"title": "Open in Spotify", "exec": format!("xdg-open {}", quote(&uri))},
            {
                "title": "Copy Link",
                "exec": format!("printf %s {} | wl-copy", quote(&link)),
            },
        ],
    })
}

// ----------------------------------------------------------------- search

/// `do_search` — device state and the search are two independent round
/// trips, so they overlap rather than adding up; both have to be in hand
/// before a row can be printed.
async fn do_search(query: &str, kind: &str, creds: &Creds) -> Vec<Value> {
    // `[[ -n ${query//[[:space:]]/} ]]` — a whitespace box asks nothing.
    if !query.bytes().any(|b| !b.is_ascii_whitespace()) {
        return Vec::new();
    }

    // No type: filter searches all three and leads with tracks, because that
    // is what a bare `sp:` is asking for nine times out of ten.
    let types = match kind.to_lowercase().as_str() {
        "album" | "albums" => "album",
        "artist" | "artists" => "artist",
        "track" | "tracks" | "song" | "songs" => "track",
        _ => "track,album,artist",
    };
    // A mixed answer is a sampler — 4/3/2 of each — a typed one goes deep.
    let (ntrack, nalbum, nartist) = if types.contains(',') {
        (4usize, 3usize, 2usize)
    } else {
        (8, 8, 8)
    };

    let search_url = format!("{API}/search?q={}&type={types}&limit=8", url_encode(query));
    let devices_url = format!("{API}/me/player/devices");
    let (search, devices) = tokio::join!(
        api("GET", &search_url, &creds.access_token, None),
        api("GET", &devices_url, &creds.access_token, None),
    );

    let Some((status, body)) = search else {
        return Vec::new();
    };
    if status == 401 {
        return vec![auth_row()];
    }
    if status != 200 {
        return Vec::new();
    }

    // Zero devices is the one case playback cannot recover from, so the row
    // says so before it is pressed instead of failing quietly afterwards.
    let note = devices
        .and_then(|(status, body)| {
            if status == 200 {
                serde_json::from_str::<Value>(&body).ok()
            } else {
                None
            }
        })
        .and_then(|v| v.get("devices").and_then(Value::as_array).map(|d| d.len()))
        .map_or("", |n| {
            if n == 0 {
                "  ·  no Spotify device to play on"
            } else {
                ""
            }
        });

    let Ok(v) = serde_json::from_str::<Value>(&body) else {
        return Vec::new();
    };

    let mut rows = Vec::new();
    if let Some(items) = v.pointer("/tracks/items").and_then(Value::as_array) {
        for (i, t) in items.iter().take(ntrack).enumerate() {
            rows.push(track_row(t, i, note));
        }
    }
    if let Some(items) = v.pointer("/albums/items").and_then(Value::as_array) {
        for (i, a) in items.iter().take(nalbum).enumerate() {
            rows.push(album_row(a, i));
        }
    }
    if let Some(items) = v.pointer("/artists/items").and_then(Value::as_array) {
        for (i, r) in items.iter().take(nartist).enumerate() {
            rows.push(artist_row(r, i));
        }
    }
    rows
}

impl NativeExt for SpotifyLibrary {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        let arg = ctx.arg;
        let kind = ctx.filters.get("type").cloned().unwrap_or_default();
        Box::pin(async move {
            if !on_path("curl") {
                return NativeOutcome::Fallback;
            }
            match ensure_token().await {
                Err(Deny::Expired) => NativeOutcome::Rows(vec![auth_row()]),
                Ok(creds) => {
                    let rows = do_search(&arg, &kind, &creds).await;
                    if rows.is_empty() {
                        NativeOutcome::Empty
                    } else {
                        NativeOutcome::Rows(rows)
                    }
                }
                // Missing creds or a dead network answer the same way the
                // script does: nothing.
                Err(_) => NativeOutcome::Empty,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pickart_prefers_the_smallest_image_over_64px() {
        let imgs = json!([
            {"url": "big", "width": 640},
            {"url": "mid", "width": 300},
            {"url": "small", "width": 64},
        ]);
        assert_eq!(pickart(Some(&imgs)), "small");
        let only_small = json!([{"url": "a", "width": 40}, {"url": "b", "width": 60}]);
        assert_eq!(pickart(Some(&only_small)), "b");
        assert_eq!(pickart(None), "");
    }

    #[test]
    fn duration_and_followers_and_year_match_jq() {
        assert_eq!(duration(Some(&json!(565000))), "9:25");
        assert_eq!(duration(None), "0:00");
        assert_eq!(duration(Some(&json!(5000))), "0:05");
        assert_eq!(followers(Some(&json!(1234567))), "1.2M followers");
        assert_eq!(followers(Some(&json!(12345))), "12.3K followers");
        assert_eq!(followers(Some(&json!(999))), "999 followers");
        assert_eq!(followers(None), "0 followers");
        assert_eq!(year(Some(&json!("1985-03-04"))), "1985");
        assert_eq!(year(Some(&json!("19"))), "");
        assert_eq!(titlecase("album"), "Album");
        assert_eq!(titlecase(""), "");
    }

    #[test]
    fn track_row_matches_the_script_shape() {
        let t = json!({
            "id": "4uLU6hMCjMI75M1A2tKUQC",
            "name": "So What",
            "uri": "spotify:track:4uLU6hMCjMI75M1A2tKUQC",
            "duration_ms": 545000,
            "external_urls": {"spotify": "https://open.spotify.com/track/x"},
            "artists": [{"name": "Miles Davis"}],
            "album": {
                "name": "Kind of Blue",
                "release_date": "1959-08-17",
                "images": [{"url": "https://img/c.jpg", "width": 300}],
            },
        });
        let row = track_row(&t, 0, "");
        assert_eq!(row["id"], "track:4uLU6hMCjMI75M1A2tKUQC");
        assert_eq!(row["title"], "So What");
        assert_eq!(row["subtitle"], "Miles Davis");
        assert_eq!(row["detail"], "Kind of Blue  ·  1959");
        assert_eq!(row["accessory"], "9:05");
        assert_eq!(row["group"], "Tracks");
        assert_eq!(row["score"], 90000);
        assert_eq!(
            row["exec"].as_str().unwrap(),
            "oxy-spotify play 'spotify:track:4uLU6hMCjMI75M1A2tKUQC'"
        );
        assert_eq!(row["actions"].as_array().unwrap().len(), 4);

        // The no-device note is appended to the subtitle, not a field.
        let noted = track_row(&t, 2, "  ·  no Spotify device to play on");
        assert_eq!(
            noted["subtitle"],
            "Miles Davis  ·  no Spotify device to play on"
        );
        assert_eq!(noted["score"], 89800);
    }

    #[test]
    fn album_and_artist_rows() {
        let a = json!({
            "id": "a1", "name": "Kind of Blue", "uri": "spotify:album:a1",
            "album_type": "album", "total_tracks": 5, "release_date": "1959",
            "artists": [{"name": "Miles Davis"}],
            "images": [{"url": "u", "width": 300}],
            "external_urls": {"spotify": "https://x"},
        });
        let row = album_row(&a, 0);
        assert_eq!(row["id"], "album:a1");
        assert_eq!(row["detail"], "Album  ·  5 tracks");
        assert_eq!(row["accessory"], "1959");

        let r = json!({
            "id": "r1", "name": "Miles Davis", "uri": "spotify:artist:r1",
            "genres": ["jazz", "modal jazz", "cool jazz"],
            "followers": {"total": 1900000},
            "images": [],
        });
        let row = artist_row(&r, 0);
        assert_eq!(row["id"], "artist:r1");
        assert_eq!(row["subtitle"], "jazz, modal jazz");
        assert_eq!(row["detail"], "1.9M followers");
    }
}
