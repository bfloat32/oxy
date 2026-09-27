//! `radio:` — internet radio from radio-browser.info, played by mpv.
//!
//! A port of the read half of `bin/oxy-search-radio`:
//!
//!   radio:          what is playing, then the stations everybody votes for
//!   radio:fip       what is playing, then stations matching that name
//!   radio:status    the player row alone — the bar widget's poll, so it must
//!                   never touch the network
//!
//! `play` and `ctl` stay the script's job: the rows' exec strings still call
//! `oxy-search-radio ...`, so Enter takes the same one-station-at-a-time path
//! it always did. What lives here is everything a query can draw — the player
//! row read back out of the run directory and mpv's IPC socket, and the
//! station list read out of radio-browser with the script's own ten-minute
//! cache in front of it.
//!
//! The socket path is also how the radio is recognised among every other mpv
//! on the machine: it is on the command line, so a pgrep for it finds exactly
//! our stream and never the film someone is watching. On Windows there is no
//! Unix socket to talk to, so the player simply never reports — the station
//! list still answers.

use std::collections::hash_map::DefaultHasher;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::pin::Pin;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::native::media::jq_str;
use crate::provider::process;
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::settings::{paths, url_encode};
use crate::support::quote::quote;

pub struct Radio;

/// How long a station list is good for. The player refreshes this whole
/// answer every few seconds while it is on screen, and radio-browser runs on
/// donated mirrors: asking one of them the same question every three seconds
/// is both rude and slower than the launcher.
const LIST_TTL: u64 = 600;
const AGENT: &str = "oxy/0.1";
/// The project asks clients to resolve a mirror by DNS rather than pinning
/// one of the named hosts, which come and go. `all.api.radio-browser.info`
/// is itself the round-robin record.
const HOST: &str = "all.api.radio-browser.info";
/// The script the rows still call for `play` and `ctl`.
const SELF: &str = "oxy-search-radio";

fn run_dir() -> PathBuf {
    std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
        .join("oxy")
}

fn sock_path() -> PathBuf {
    run_dir().join("radio.sock")
}

fn state_path() -> PathBuf {
    run_dir().join("radio.json")
}

fn marker() -> String {
    format!("--input-ipc-server={}", sock_path().display())
}

fn cache_dir() -> PathBuf {
    std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| paths::home().join(".cache"))
        .join("oxy")
}

/// `pgrep -f -- "$MARKER"` — true while the mpv that owns the socket lives.
async fn alive() -> bool {
    process::check(&format!("pgrep -f -- {}", quote(&marker()))).await
}

/// Talk to the running mpv over its JSON IPC socket. Each command array gets
/// a `request_id`; the replies come back as one JSON array in the same order,
/// `null` for any command mpv never answered. `None` when there is no socket
/// or no mpv behind it — the caller then draws "Connecting", which is what
/// the script does when IPC cannot be had.
#[cfg(unix)]
async fn mpv_ipc(commands: &[Vec<Value>]) -> Option<Vec<Value>> {
    use std::os::unix::fs::FileTypeExt;
    use tokio::io::{AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let sock = sock_path();
    // [[ -S $SOCK ]] — connecting to a path that is not a socket only burns
    // the timeout.
    let is_socket = std::fs::metadata(&sock)
        .map(|m| m.file_type().is_socket())
        .unwrap_or(false);
    if !is_socket {
        return None;
    }

    let work = async move {
        let stream = UnixStream::connect(&sock).await.ok()?;
        let (rd, mut wr) = stream.into_split();
        for (i, cmd) in commands.iter().enumerate() {
            let line = serde_json::to_string(&json!({
                "command": cmd,
                "request_id": i + 1,
            }))
            .ok()?;
            wr.write_all(line.as_bytes()).await.ok()?;
            wr.write_all(b"\n").await.ok()?;
        }
        wr.flush().await.ok()?;

        let mut rd = BufReader::new(rd);
        let mut buf = Vec::new();
        let mut got: Vec<Option<Value>> = vec![None; commands.len()];
        let mut have = 0usize;
        while have < commands.len() {
            let line = match crate::support::lines::next(
                &mut rd,
                &mut buf,
                crate::support::lines::MAX_LINE,
            )
            .await
            {
                Ok(Some(line)) => line,
                _ => break,
            };
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            // mpv streams events down the same socket it answers on, so a
            // reply is only a reply if it carries back an id we handed out.
            let Some(rid) = msg.get("request_id").and_then(Value::as_u64) else {
                continue;
            };
            let rid = rid as usize;
            if rid == 0 || rid > commands.len() || msg.get("error").is_none() {
                continue;
            }
            if got[rid - 1].is_none() {
                got[rid - 1] = Some(msg.get("data").cloned().unwrap_or(Value::Null));
                have += 1;
            }
        }
        Some(
            got.into_iter()
                .map(|v| v.unwrap_or(Value::Null))
                .collect::<Vec<Value>>(),
        )
    };

    // The script's python helper gives the socket one second. A wedged mpv is
    // indistinguishable from a dead one past that, and a slow answer must look
    // like Connecting, not like a hung query.
    tokio::time::timeout(Duration::from_millis(1200), work)
        .await
        .ok()
        .flatten()
}

/// No Unix sockets here: no player row, same as a machine with no mpv.
#[cfg(not(unix))]
async fn mpv_ipc(_commands: &[Vec<Value>]) -> Option<Vec<Value>> {
    None
}

/// The player row, when there is one. Silent when nothing is playing, which
/// is what makes `radio:` with no stream just a list of stations.
async fn now_playing(follow_up: &str) -> Option<Value> {
    let state = state_path();
    if !state.is_file() {
        return None;
    }
    if !alive().await {
        // The process is gone: someone closed it, or it lost the stream and
        // gave up. Clear up rather than drawing a player for a stream nobody
        // can hear.
        let _ = std::fs::remove_file(&state);
        let _ = std::fs::remove_file(sock_path());
        return None;
    }

    let text = std::fs::read_to_string(&state).ok()?;
    let state_json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let name = jq_str(state_json.get("name"));
    let subtitle = jq_str(state_json.get("subtitle"));
    let art = jq_str(state_json.get("art"));
    let homepage = jq_str(state_json.get("homepage"));
    let url = jq_str(state_json.get("url"));
    let mut volume = match state_json.get("volume") {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.trunc() as i64))
            .unwrap_or(100),
        Some(Value::String(s)) => s.parse().unwrap_or(100),
        _ => 100,
    };

    // pause, mute, the ICY title, volume, core-idle, time-pos. One connection
    // for all six: opening the socket six times to read six properties is most
    // of the cost of answering at all.
    //
    // icy-title by key rather than media-title. media-title falls back to the
    // filename when a station sends no metadata, so a stream at .../dance.mp3
    // reported "dance.mp3" as the thing that was playing.
    let mut muted = false;
    let mut elapsed = 0i64;
    let mut status = "Connecting";
    let mut title = String::new();
    if let Some(reply) = mpv_ipc(&[
        vec![json!("get_property"), json!("pause")],
        vec![json!("get_property"), json!("mute")],
        vec![json!("get_property"), json!("metadata/by-key/icy-title")],
        vec![json!("get_property"), json!("volume")],
        vec![json!("get_property"), json!("core-idle")],
        vec![json!("get_property"), json!("time-pos")],
    ])
    .await
    {
        let paused = reply.first().is_some_and(|v| *v == Value::Bool(true));
        muted = reply.get(1).is_some_and(|v| *v == Value::Bool(true));
        let icy = jq_str(reply.get(2));
        let vol = jq_str(reply.get(3));
        let idle = reply.get(4).is_some_and(|v| *v == Value::Bool(true));
        let pos = jq_str(reply.get(5));

        // Paused first: mpv reports core-idle true while paused as well, and
        // "Buffering" on a stream the user paused themselves is a lie.
        status = if paused {
            "Paused"
        } else if idle {
            "Buffering"
        } else {
            "Playing"
        };

        if !vol.is_empty() {
            // awk's %d truncates a decimal volume and reads a non-number as 0.
            volume = vol.parse::<f64>().map(|v| v.trunc() as i64).unwrap_or(0);
        }
        elapsed = pos
            .parse::<f64>()
            .map(|p| if p > 0.0 { p.trunc() as i64 } else { 0 })
            .unwrap_or(0);

        // The ICY title is what the station is playing right now, which is
        // the thing worth putting in the big text. Stations that send their
        // own name there instead say nothing new, so they fall back to the
        // layout below.
        if icy != name {
            title = icy;
        }
    }

    // Connecting has no title yet and no station metadata worth reading. It
    // is a real state, not an error: mpv is opening the stream, and the view
    // draws a placeholder rather than an empty line.
    let (head, sub) = if title.is_empty() {
        (name.clone(), subtitle)
    } else {
        (title, name.clone())
    };

    let ctl = |rest: &str| format!("{SELF} ctl {rest}");
    let toggle = ctl("toggle");
    let stop = ctl("stop");
    let mute = ctl("mute");
    let vol_up = ctl("volume up");
    let vol_down = ctl("volume down");

    let mut actions = vec![
        json!({
            "title": if status == "Paused" { "Resume" } else { "Pause" },
            "shortcut": "↵",
            "exec": toggle,
            "query": follow_up,
        }),
        json!({"title": "Stop", "exec": stop, "query": follow_up}),
        json!({
            "title": if muted { "Unmute" } else { "Mute" },
            "exec": mute,
            "query": follow_up,
        }),
        json!({"title": "Volume Up", "exec": vol_up, "query": follow_up}),
        json!({"title": "Volume Down", "exec": vol_down, "query": follow_up}),
        json!({
            "title": "Copy Stream URL",
            "exec": format!("printf %s {} | wl-copy", quote(&url)),
        }),
    ];
    if !homepage.is_empty() {
        actions.push(json!({
            "title": "Open Homepage",
            "exec": format!("omarchy-launch-browser {}", quote(&homepage)),
        }));
    }

    Some(json!({
        "id": "now",
        "kind": "player",
        "view": "radioplayer",
        // The ceiling of what a row can score for itself. `local` is clamped
        // to 99999 and never crosses a tier, so this is the highest a row in
        // this answer can rank and the player cannot be pushed down the list
        // by a station the ranker happens to like.
        "score": 99999,
        "title": head,
        "subtitle": sub,
        "station": name,
        "art": art,
        "status": status,
        "volume": volume,
        "muted": muted,
        "elapsedSeconds": elapsed,
        "controls": {
            "playPause": toggle,
            "stop": stop,
            "mute": mute,
            "volumeUp": vol_up,
            "volumeDown": vol_down,
        },
        "exec": toggle,
        "actions": actions,
    }))
}

/// The cache file name is the script's: md5 of `endpoint:query`, first 16
/// hex, under the same directory — so whichever leg answered last is the one
/// the other leg reads. md5sum is coreutils; the hasher fallback only matters
/// where coreutils is not.
async fn cache_key(endpoint: &str, query: &str) -> String {
    let seed = format!("{endpoint}:{query}");
    let body = format!("printf %s {} | md5sum | cut -c1-16", quote(&seed));
    let out = process::run(&body, Duration::from_secs(2))
        .await
        .map(|f| f.stdout.trim().to_string())
        .unwrap_or_default();
    if out.len() == 16 && out.bytes().all(|b| b.is_ascii_hexdigit()) {
        return out;
    }
    let mut h = DefaultHasher::new();
    seed.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// The station list: the ten most-voted on an empty box, the ten best-voted
/// name matches otherwise, from cache when it is still young.
async fn stations(query: &str, follow_up: &str) -> Vec<Value> {
    // `radio:` on its own used to answer nothing, which reads as a search
    // that failed rather than as a question not yet asked. The stations
    // everybody else votes for are a better opening than an empty box.
    let endpoint = if query.is_empty() {
        "topvote"
    } else {
        "search"
    };
    let url = if endpoint == "topvote" {
        format!("https://{HOST}/json/stations/topvote?limit=10&hidebroken=true")
    } else {
        format!(
            "https://{HOST}/json/stations/search?name={}&limit=10&hidebroken=true&order=votes&reverse=true",
            url_encode(query)
        )
    };

    let dir = cache_dir();
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join(format!("radio-{}.json", cache_key(endpoint, query).await));

    let fresh = std::fs::metadata(&file)
        .ok()
        .filter(|m| m.len() > 0)
        .and_then(|m| m.modified().ok())
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age.as_secs() < LIST_TTL);

    if !fresh {
        let body = process::run(
            &format!(
                "curl -sf --max-time 6 -H {} {}",
                quote(&format!("User-Agent: {AGENT}")),
                quote(&url)
            ),
            Duration::from_secs(7),
        )
        .await
        .filter(|f| f.code == Some(0))
        .map(|f| f.stdout)
        .unwrap_or_default();
        if !body.is_empty() && std::fs::write(&file, &body).is_ok() {
            // One file per distinct search, and nothing ever removed one.
            // Anything past ten minutes is already refetched on sight, so
            // deleting it costs nothing that was going to be used.
            prune(&dir);
        }
    }

    // Nothing cached and nothing fetched: no rows, no noise, no error. A
    // launcher that prints a diagnostic here would show it as a result.
    let Ok(text) = std::fs::read_to_string(&file) else {
        return Vec::new();
    };
    let Ok(list) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let Some(items) = list.as_array() else {
        return Vec::new();
    };

    items
        .iter()
        .filter_map(|st| station_row(st, follow_up))
        .collect()
}

/// `find "$CACHEDIR" -maxdepth 1 -name 'radio-*.json' -mmin +11 -delete`.
fn prune(dir: &std::path::Path) {
    let cutoff = Duration::from_secs(LIST_TTL + 60);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("radio-") && name.ends_with(".json")) {
            continue;
        }
        let old = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > cutoff);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// One station object to the row the script's jq emits.
fn station_row(st: &Value, follow_up: &str) -> Option<Value> {
    let stream = jq_str(st.get("url_resolved"));
    if stream.is_empty() {
        return None;
    }
    let name = jq_str(st.get("name"));
    let sub = [st.get("country"), st.get("language")]
        .into_iter()
        .filter_map(|v| v.and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("  ·  ");
    let codec = jq_str(st.get("codec"));
    let bitrate = st.get("bitrate").and_then(Value::as_f64).unwrap_or(0.0);
    let detail = [
        codec,
        if bitrate > 0.0 {
            format!("{} kbps", jq_str(st.get("bitrate")))
        } else {
            String::new()
        },
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join("  ·  ");
    let votes = st.get("votes").and_then(Value::as_i64).unwrap_or(0);
    let art = jq_str(st.get("favicon"));
    let home = jq_str(st.get("homepage"));

    let play = format!(
        "{SELF} play {} {} {} {} {}",
        quote(&stream),
        quote(&name),
        quote(&sub),
        quote(&art),
        quote(&home)
    );

    Some(json!({
        "id": st.get("stationuuid").cloned().unwrap_or(Value::Null),
        "kind": "station",
        "title": st.get("name").cloned().unwrap_or(Value::Null),
        "subtitle": sub,
        "detail": detail,
        "accessory": if votes > 0 { format!("{votes} votes") } else { String::new() },
        "art": art,
        "view": "radioplayer",
        "score": 90000,
        "exec": play,
        "actions": [
            // `query` is what keeps the launcher here. Without it Enter
            // starts a station and closes, which is how someone ends up with
            // four of them playing and no way to reach any: the follow-up
            // re-runs this same question, and the answer now has a player on
            // top of it.
            {"title": "Play", "shortcut": "↵", "exec": play, "query": follow_up},
            {
                "title": "Copy Stream URL",
                "exec": format!("printf %s {} | wl-copy", quote(&stream)),
            },
            {
                "title": "Open Homepage",
                "exec": format!("omarchy-launch-browser {}", quote(&home)),
            },
        ],
    }))
}

impl NativeExt for Radio {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        let arg = ctx.arg;
        Box::pin(async move {
            match arg.as_str() {
                // The bar widget's poll: the player row alone. Nothing playing
                // prints nothing, which is how the widget knows to be absent.
                "status" => {
                    return match now_playing("radio:").await {
                        Some(row) => NativeOutcome::Rows(vec![row]),
                        None => NativeOutcome::Empty,
                    };
                }
                // `ctl` and `play` are the script's subcommands; typed bare
                // into the box they arrive as one quoted word, and the script
                // answers them with nothing — it got a control call with no
                // argument, not a search.
                "ctl" | "play" => return NativeOutcome::Empty,
                _ => {}
            }

            let follow_up = format!("radio:{arg}");
            let mut rows = Vec::new();
            if let Some(row) = now_playing(&follow_up).await {
                rows.push(row);
            }
            rows.extend(stations(&arg, &follow_up).await);
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
    fn station_row_builds_the_script_shape() {
        let st = json!({
            "stationuuid": "abc-123",
            "name": "FIP",
            "country": "France",
            "language": "French",
            "codec": "MP3",
            "bitrate": 128,
            "votes": 42,
            "favicon": "https://fip.fr/f.png",
            "homepage": "https://fip.fr",
            "url_resolved": "https://stream.fip.fr/fip.mp3",
        });
        let row = station_row(&st, "radio:fip").unwrap();
        assert_eq!(row["id"], "abc-123");
        assert_eq!(row["kind"], "station");
        assert_eq!(row["title"], "FIP");
        assert_eq!(row["subtitle"], "France  ·  French");
        assert_eq!(row["detail"], "MP3  ·  128 kbps");
        assert_eq!(row["accessory"], "42 votes");
        assert_eq!(row["view"], "radioplayer");
        let exec = row["exec"].as_str().unwrap();
        assert!(exec.starts_with("oxy-search-radio play 'https://stream.fip.fr/fip.mp3'"));
        // The five play args: url, name, subtitle, art, homepage — each
        // quoted, empty ones as ''.
        let with_empty =
            station_row(&json!({"url_resolved": "http://x", "name": "n"}), "radio:").unwrap();
        assert!(
            with_empty["exec"]
                .as_str()
                .unwrap()
                .ends_with(" 'n' '' '' ''")
        );
    }

    #[test]
    fn station_row_skips_streams_without_a_url() {
        assert!(station_row(&json!({"name": "x"}), "radio:").is_none());
        assert!(station_row(&json!({"url_resolved": ""}), "radio:").is_none());
    }

    #[test]
    fn station_row_quotes_untrusted_text() {
        let st = json!({
            "url_resolved": "http://x/$(rm -rf ~)",
            "name": "it's a $(bad) `name`",
        });
        let row = station_row(&st, "radio:").unwrap();
        let exec = row["exec"].as_str().unwrap();
        // Every interpolated field lands as one single-quoted shell word —
        // the text is still there, but inside the quotes where it cannot run.
        assert!(exec.contains("'http://x/$(rm -rf ~)'"));
        assert!(exec.contains(&quote("it's a $(bad) `name`")));
    }
}
