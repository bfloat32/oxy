//! `spotify:` / `music:` — what is playing, and a keyless catalogue search.
//!
//! A port of `bin/oxy-search-music`:
//!
//!   spotify:               what is playing, as a player
//!   spotify:kind of blue   search a catalogue, then play or open the result
//!
//! Control goes over MPRIS through `busctl`, which every player that shows a
//! media widget already speaks. Search goes to Deezer's public endpoint, which
//! needs no key and returns cover art. Playing the exact track a search
//! returned stays with `oxy-music-play`: the script turns the ISRC into a
//! Spotify id through MusicBrainz before it calls OpenUri, which is a bigger
//! surface than this port takes on — so the rows keep calling it.
//!
//! The manifest gates on `command -v spotify`; the provider re-checks, since a
//! failed `when` still asks the native side first.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::native::media::jq_str;
use crate::provider::native::util::on_path;
use crate::provider::process;
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::settings::url_encode;
use crate::support::quote::quote;

pub struct Spotify;

const BUS_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";

/// `busctl <args>` with the script's two-second patience — a D-Bus call to a
/// player that is mid-exit must not hold the query open.
async fn busctl(args: &str) -> String {
    process::run(&format!("busctl {args}"), Duration::from_secs(2))
        .await
        .map(|f| f.stdout.trim_end_matches('\n').to_string())
        .unwrap_or_default()
}

async fn prop(player: &str, name: &str) -> String {
    busctl(&format!(
        "--user get-property {player} {BUS_PATH} {PLAYER_IFACE} {name}"
    ))
    .await
}

/// The first bus name under org.mpris.MediaPlayer2.* — the script's
/// `awk '$1 ~ /^org\.mpris\.MediaPlayer2\./ { print $1; exit }'`.
async fn player_name() -> String {
    busctl("--user list --no-legend")
        .await
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .find(|name| name.starts_with("org.mpris.MediaPlayer2."))
        .map(str::to_string)
        .unwrap_or_default()
}

/// The value inside `"key" s "..."`, busctl escapes and all — the script's
/// `field()`: `grep -oP "\"$key\"\s+\Ks[^\"]*\"([^\"\\\\]|\\\\.)*\"" | head -1`
/// plus its `unescape` (a backslash before any character quotes it).
fn s_field(meta: &str, key: &str) -> Option<String> {
    let rest = after_key(meta, key)?;
    let rest = rest.strip_prefix('s')?;
    let at = rest.find('"')?;
    dbus_string(&rest[at..])
}

/// The first element of `"key" as N "..." "..."` — the script greps the same
/// shape with `\d+` where the count sits.
fn as_field(meta: &str, key: &str) -> Option<String> {
    let rest = after_key(meta, key)?;
    let rest = rest.strip_prefix("as")?.trim_start();
    let digits = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let rest = rest[digits..].trim_start();
    dbus_string(rest)
}

/// `"key" t 12345` — an unsigned integer member, mpris:length's shape.
fn t_field(meta: &str, key: &str) -> Option<u64> {
    let rest = after_key(meta, key)?;
    let rest = rest.strip_prefix('t')?.trim_start();
    let digits = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
    rest[..digits].parse().ok().filter(|_| digits > 0)
}

/// Everything after `"key"` and its whitespace — where the type tag sits.
fn after_key<'a>(meta: &'a str, key: &str) -> Option<&'a str> {
    let at = meta.find(&format!("\"{key}\""))? + key.len() + 2;
    Some(meta[at..].trim_start())
}

/// A busctl double-quoted string at the head of `s`, unescaped the way the
/// script's `sed 's/\\\(.\)/\1/g'` leaves it: `\x` reads as `x` for any x.
fn dbus_string(s: &str) -> Option<String> {
    let mut chars = s.strip_prefix('"')?.chars();
    let mut out = String::new();
    loop {
        match chars.next()? {
            '"' => return Some(out),
            '\\' => out.extend(chars.next()),
            c => out.push(c),
        }
    }
}

/// `sed 's/^s\s*//; s/"//g'` on a `s "value"` property line.
fn prop_string(out: &str) -> String {
    out.trim()
        .strip_prefix('s')
        .unwrap_or_else(|| out.trim())
        .trim_start()
        .replace('"', "")
}

/// `awk '{print $2}'` on a `b true` property line — missing or odd means
/// nothing rather than false, the caller decides the default.
fn prop_bool(out: &str) -> Option<bool> {
    match out.split_whitespace().nth(1) {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    }
}

/// `grep -oP '^x\s+\K\d+'` — Position's microseconds.
fn prop_x(out: &str) -> Option<u64> {
    let rest = out.trim_start().strip_prefix('x')?.trim_start();
    let digits = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
    rest[..digits].parse().ok().filter(|_| digits > 0)
}

/// SetPosition takes an object path and microseconds, and the view builds
/// that call by substituting seconds into a shell string. `trackid` is text
/// whichever process owns the bus name chose to emit, so it is confined to
/// object-path characters — all shell-inert; anything else gets no seek.
fn valid_trackid(trackid: &str) -> bool {
    trackid.starts_with('/')
        && trackid[1..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'/')
}

/// The player row, or nothing when no player is on the bus or it has no
/// track — `[[ -z $player ]] || [[ -z $title ]]` in the script.
async fn now_playing() -> Option<Value> {
    let player = player_name().await;
    if player.is_empty() {
        return None;
    }

    let metadata = prop(&player, "Metadata").await;
    let title = s_field(&metadata, "xesam:title").unwrap_or_default();
    if title.is_empty() {
        return None;
    }
    let album = s_field(&metadata, "xesam:album").unwrap_or_default();
    let art = s_field(&metadata, "mpris:artUrl").unwrap_or_default();
    let artist = as_field(&metadata, "xesam:artist").unwrap_or_default();
    let length = t_field(&metadata, "mpris:length").unwrap_or(0);
    let trackid = s_field(&metadata, "mpris:trackid").unwrap_or_default();

    // Seven reads on one player; busctl has no batch call, so they overlap —
    // a hung get-property is still bounded by its own deadline.
    let (status, position, shuffle, loop_status, can_next, can_prev) = tokio::join!(
        prop(&player, "PlaybackStatus"),
        prop(&player, "Position"),
        prop(&player, "Shuffle"),
        prop(&player, "LoopStatus"),
        prop(&player, "CanGoNext"),
        prop(&player, "CanGoPrevious"),
    );

    let status = {
        let s = prop_string(&status);
        if s.is_empty() {
            "Stopped".to_string()
        } else {
            s
        }
    };

    // Length and position are microseconds; the view wants seconds and a
    // fraction. `%.0f` and `%.4f` are the script's awk prints.
    let mut seconds = 0i64;
    let mut progress = 0.0f64;
    if length > 0 {
        seconds = (length as f64 / 1_000_000.0).round() as i64;
        if let Some(pos) = prop_x(&position) {
            progress = format!("{:.4}", pos as f64 / length as f64)
                .parse()
                .unwrap_or(0.0);
        }
    }

    let short = player
        .strip_prefix("org.mpris.MediaPlayer2.")
        .unwrap_or(&player);
    let call = format!("busctl --user call {player} {BUS_PATH} {PLAYER_IFACE}");
    let setprop = format!("busctl --user set-property {player} {BUS_PATH} {PLAYER_IFACE}");

    // Shuffle and repeat are real player state, not commands, so the view
    // needs to know which way they are set before it can draw them lit or
    // unlit.
    let shuffle_on = prop_bool(&shuffle).unwrap_or(false);
    let loop_status = {
        let s = prop_string(&loop_status);
        if s.is_empty() { "None".to_string() } else { s }
    };
    // LoopStatus cycles None, Playlist, Track — the order the buttons in
    // every player step through: off, repeat all, repeat one.
    let next_loop = match loop_status.as_str() {
        "None" => "Playlist",
        "Playlist" => "Track",
        _ => "None",
    };
    let can_next = prop_bool(&can_next).unwrap_or(true);
    let can_prev = prop_bool(&can_prev).unwrap_or(true);

    let seek = if valid_trackid(&trackid) {
        format!("{call} SetPosition ox {trackid} $(( {{seconds}} * 1000000 ))")
    } else {
        String::new()
    };

    let pp = format!("{call} PlayPause");
    let next = format!("{call} Next");
    let prev = format!("{call} Previous");
    let shuffle_cmd = format!(
        "{setprop} Shuffle b {}",
        if shuffle_on { "false" } else { "true" }
    );
    let loop_cmd = format!("{setprop} LoopStatus s {next_loop}");

    Some(json!({
        "id": "now",
        "title": title,
        "subtitle": artist,
        "detail": album,
        "art": art,
        "status": status,
        "player": short,
        "progress": progress,
        "lengthSeconds": seconds,
        "seek": seek,
        "view": "player",
        "score": 99000,
        "shuffle": shuffle_on,
        "loop": loop_status,
        "canNext": can_next,
        "canPrev": can_prev,
        "controls": {
            "playPause": pp,
            "next": next,
            "prev": prev,
            "shuffle": shuffle_cmd,
            "loop": loop_cmd,
        },
        "exec": pp,
        "actions": [
            {
                "title": if status == "Playing" { "Pause" } else { "Play" },
                "shortcut": "↵",
                "exec": pp,
                "query": "spotify:",
            },
            {"title": "Next Track", "exec": next, "query": "spotify:"},
            {"title": "Previous Track", "exec": prev, "query": "spotify:"},
            {
                "title": if shuffle_on { "Shuffle Off" } else { "Shuffle On" },
                "exec": shuffle_cmd,
                "query": "spotify:",
            },
            {
                "title": match loop_status.as_str() {
                    "None" => "Repeat All",
                    "Playlist" => "Repeat One",
                    _ => "Repeat Off",
                },
                "exec": loop_cmd,
                "query": "spotify:",
            },
            {
                "title": "Copy Title and Artist",
                "exec": format!(
                    "printf %s {} | wl-copy",
                    quote(&format!("{title} - {artist}"))
                ),
            },
        ],
    }))
}

/// Deezer rather than iTunes, for one reason: it returns an ISRC on every
/// row, and an ISRC is what `oxy-music-play` can turn into a Spotify track
/// id. Without it a result can only ever open a search rather than play the
/// track.
async fn search(query: &str) -> Vec<Value> {
    if !on_path("curl") {
        return Vec::new();
    }
    let url = format!(
        "https://api.deezer.com/search?q={}&limit=9",
        url_encode(query)
    );
    let body = process::run(
        &format!("curl -sf --max-time 6 {}", quote(&url)),
        Duration::from_secs(7),
    )
    .await
    .filter(|f| f.code == Some(0))
    .map(|f| f.stdout)
    .unwrap_or_default();
    let Ok(json) = serde_json::from_str::<Value>(&body) else {
        return Vec::new();
    };
    let Some(data) = json.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    data.iter().filter_map(track_row).collect()
}

/// One Deezer track to the row the script's jq emits.
fn track_row(item: &Value) -> Option<Value> {
    let title = jq_str(item.get("title"));
    if item.get("title").is_none_or(Value::is_null) {
        // The script emits whatever .title is; a null title drops at to_row,
        // so drop it here instead of carrying a row with no name.
        return None;
    }
    let subtitle = jq_str(item.get("artist").and_then(|a| a.get("name")));
    let duration = item.get("duration").and_then(Value::as_f64).unwrap_or(0.0);
    let accessory = format!(
        "{}:{:02}",
        (duration / 60.0).floor() as i64,
        (duration % 60.0).round() as i64
    );
    let art = jq_str(item.get("album").and_then(|a| a.get("cover_medium")));
    let preview = jq_str(item.get("preview"));
    let isrc = jq_str(item.get("isrc"));

    let phrase = format!("{subtitle} {title}");
    // `oxy-music-play` is still the script: it owns the ISRC→MusicBrainz→
    // OpenUri dance, and the manifest keeps it installed for exactly that.
    let play = format!("oxy-music-play {} {}", quote(&isrc), quote(&phrase));
    // Shift+Enter raises the Spotify window on that search, for when you want
    // the app rather than the track. OpenUri rather than xdg-open: xdg-open
    // claims the spotify: handler and then does nothing with it.
    let open = format!(
        "busctl --user call org.mpris.MediaPlayer2.spotify {BUS_PATH} {PLAYER_IFACE} OpenUri s {} && omarchy-launch-or-focus '^[Ss]potify$' 'uwsm-app -- spotify'",
        quote(&format!("spotify:search:{}", url_encode(&phrase)))
    );
    let web = format!(
        "omarchy-launch-browser {}",
        quote(&format!(
            "https://open.spotify.com/search/{}",
            url_encode(&phrase)
        ))
    );

    let mut actions = vec![
        json!({"title": "Play", "shortcut": "↵", "exec": play, "query": "spotify:"}),
        json!({"title": "Open in Spotify", "shortcut": "⇧↵", "exec": open}),
    ];
    if !preview.is_empty() {
        actions.push(json!({
            "title": "Play 30s Preview",
            "exec": format!("mpv --no-video --really-quiet {}", quote(&preview)),
        }));
    }
    actions.push(json!({"title": "Open in Browser", "exec": web}));
    actions.push(json!({
        "title": "Copy Title and Artist",
        "exec": format!("printf %s {} | wl-copy", quote(&format!("{title} - {subtitle}"))),
    }));

    Some(json!({
        "id": jq_str(item.get("id")),
        "title": item.get("title").cloned().unwrap_or(Value::Null),
        "subtitle": item
            .get("artist")
            .and_then(|a| a.get("name"))
            .cloned()
            .unwrap_or(Value::Null),
        "detail": item
            .get("album")
            .and_then(|a| a.get("title"))
            .cloned()
            .unwrap_or(Value::Null),
        "accessory": accessory,
        "art": art,
        "view": "cards",
        "score": 90000,
        "exec": play,
        "actions": actions,
    }))
}

impl NativeExt for Spotify {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        let arg = ctx.arg;
        Box::pin(async move {
            // The manifest's `when` is `command -v spotify`; a provider still
            // re-checks, because a failed gate does not stop the native call
            // — it only decides whether the script fallback may run.
            if !on_path("spotify") {
                return NativeOutcome::Fallback;
            }
            if arg.is_empty() {
                return match now_playing().await {
                    Some(row) => NativeOutcome::Rows(vec![row]),
                    None => NativeOutcome::Empty,
                };
            }
            let rows = search(&arg).await;
            if rows.is_empty() {
                NativeOutcome::Empty
            } else {
                NativeOutcome::Rows(rows)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s_field_reads_busctl_strings() {
        let meta = r#"a{sv} 5 "xesam:title" s "Doesn\'t Just Happen" "xesam:album" s "Blue""#;
        assert_eq!(
            s_field(meta, "xesam:title"),
            Some("Doesn't Just Happen".to_string())
        );
        assert_eq!(s_field(meta, "xesam:album"), Some("Blue".to_string()));
        assert_eq!(s_field(meta, "xesam:missing"), None);
    }

    #[test]
    fn as_field_reads_the_first_array_element() {
        let meta = r#""xesam:artist" as 2 "Miles Davis" "Someone Else" "mpris:length" t 196893000"#;
        assert_eq!(
            as_field(meta, "xesam:artist"),
            Some("Miles Davis".to_string())
        );
        assert_eq!(t_field(meta, "mpris:length"), Some(196893000));
    }

    #[test]
    fn prop_parsers_match_the_sed_and_awk() {
        assert_eq!(prop_string("s \"Playing\""), "Playing");
        assert_eq!(prop_bool("b true"), Some(true));
        assert_eq!(prop_bool("b false"), Some(false));
        assert_eq!(prop_bool(""), None);
        assert_eq!(prop_x("x 12345678"), Some(12345678));
        assert_eq!(prop_x("s \"x\""), None);
    }

    #[test]
    fn a_hostile_trackid_gets_no_seek_command() {
        assert!(valid_trackid("/org/mpris/MediaPlayer2/Track/1234"));
        assert!(valid_trackid("/com/spotify/track/abc_DEF"));
        assert!(!valid_trackid("/x; rm -rf ~"));
        assert!(!valid_trackid("$(reboot)"));
        assert!(!valid_trackid("/x'`id`'"));
        assert!(!valid_trackid(""));
        assert!(!valid_trackid("no/slash/at/head"));
    }

    #[test]
    fn track_row_builds_the_script_shape() {
        let item = json!({
            "id": 1234,
            "title": "So What",
            "duration": 545,
            "preview": "https://cdns-preview/p.mp3",
            "isrc": "USRC16000001",
            "artist": {"name": "Miles Davis"},
            "album": {"title": "Kind of Blue", "cover_medium": "https://img/c.jpg"},
        });
        let row = track_row(&item).unwrap();
        assert_eq!(row["id"], "1234");
        assert_eq!(row["title"], "So What");
        assert_eq!(row["subtitle"], "Miles Davis");
        assert_eq!(row["detail"], "Kind of Blue");
        assert_eq!(row["accessory"], "9:05");
        assert_eq!(row["view"], "cards");
        assert!(
            row["exec"]
                .as_str()
                .unwrap()
                .starts_with("oxy-music-play 'USRC16000001' 'Miles Davis So What'")
        );
        let actions = row["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 5);
        assert_eq!(actions[2]["title"], "Play 30s Preview");
        // The script's jq deletes .isrc and .preview from the emitted row.
        assert!(row.get("isrc").is_none());
        assert!(row.get("preview").is_none());
    }
}
