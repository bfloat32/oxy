//! `wifi — networks, the radio and saved profiles`. A port of `bin/oxy-wifi`.
//!
//! What the script proved, kept:
//!   * `--rescan no` on the list call, always — a rescan takes seconds and
//!     empties the list while it runs, which per keystroke is unusable.
//!     NetworkManager scans on its own, and `Rescan` stays an action on the
//!     radio row for when it has not.
//!   * The `-f` field list is version-sensitive: one unknown field name and
//!     nmcli prints nothing at all, so it is copied verbatim — FREQ and RATE,
//!     not the younger BAND.
//!   * Enter means three things, so there are three row kinds: the active
//!     network disconnects, a saved profile comes `up` with no password to
//!     collect, and a new secured network hands off to Omarchy's network
//!     panel, which owns the passphrase prompt, the retry and the enterprise
//!     fields. (The script's comment predates the form view — `form` exists
//!     now, PORTING-BACKLOG §4.3, and a later pass could make this one row
//!     with a field in it. The wire shape stays as shipped.)
//!   * Terse output is colon-separated with `\:` escaping a colon inside a
//!     field, and SSID is asked for last so it is always the remainder of
//!     the line — nothing before it can contain one.
//!   * A switched-off radio is the whole answer: one row, no list — it is
//!     the reason the list is empty, not a member of it.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::native::util::on_path;
use crate::provider::process::run;
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::support::quote::quote;

pub struct Wifi;

/// One read is bounded well under the manifest's 6000ms; the calls run one
/// after another exactly the way the script ran them, and a slow one answers
/// empty rather than stalling the launcher.
const TIMEOUT: Duration = Duration::from_secs(2);

/// Omarchy's network panel owns the passphrase prompt for a network with no
/// saved profile — the row hands off rather than collecting a secret.
const PANEL: &str = "omarchy-shell -q shell summon omarchy.network";

/// What the radio row answers for besides the bare query — `wifi:rescan`
/// still finds the switch.
const RADIO_WORDS: &str = "wifi radio scan rescan on off";

/// Copy Password resolves the interface when the action runs, not when the
/// row was drawn: the row may still be on screen after a roam, and reading
/// the password off the wrong device fails in a way that looks like the
/// password is gone. The `\$` survive into the emitted string — they guard
/// awk's fields from the shell that later runs the row.
const COPY_PASSWORD: &str = "iface=$(nmcli -t -f DEVICE,TYPE,STATE device | awk -F: -v w=wifi -v c=connected \"\\$2 == w && \\$3 == c { print \\$1; exit }\"); omarchy-network-password \"$iface\" | tr -d \"\\n\" | wl-copy";

/// One `nmcli` read under the login env; a spawn failure or a slow call
/// answers empty, which is what the script saw through its `2>/dev/null`s.
async fn nmcli(args: &str) -> String {
    run(&format!("nmcli {args}"), TIMEOUT)
        .await
        .map(|f| f.stdout)
        .unwrap_or_default()
}

/// nmcli's terse split: the field is everything up to the next `:` — the
/// same naive cut the script's `${x%%:*}` made, safe because only the last
/// field of a line (SSID, NAME) can carry an escaped `\:`.
fn field(s: &str) -> (&str, &str) {
    match s.find(':') {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, ""),
    }
}

/// `nmcli -t -f WIFI radio` says one word; a NetworkManager that is not
/// running answers neither on nor off, and neither is guessed.
fn radio_state(text: &str) -> &str {
    text.trim()
}

/// Saved 802-11-wireless profiles by name. TYPE is read first so the name,
/// which may hold an escaped colon, is the remainder of the line.
fn saved_profiles(text: &str) -> HashSet<String> {
    text.lines()
        .filter_map(|line| {
            let (ty, name) = field(line);
            let name = name.replace("\\:", ":");
            (ty == "802-11-wireless" && !name.is_empty()).then_some(name)
        })
        .collect()
}

/// The first device whose type is wifi — the interface the radio row names.
fn wifi_iface(text: &str) -> String {
    for line in text.lines() {
        let (dev, rest) = field(line);
        let (ty, _) = field(rest);
        if ty == "wifi" {
            return dev.to_string();
        }
    }
    String::new()
}

/// One line of `device wifi list`: IN-USE, SIGNAL, SECURITY, FREQ, RATE and
/// SSID — asked for last so it is the remainder of the line.
struct Ap {
    in_use: String,
    signal: String,
    security: String,
    freq: String,
    rate: String,
    ssid: String,
}

fn parse_aps(text: &str) -> Vec<Ap> {
    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (in_use, rest) = field(line);
            let (signal, rest) = field(rest);
            let (security, rest) = field(rest);
            let (freq, rest) = field(rest);
            let (rate, rest) = field(rest);
            Ap {
                in_use: in_use.into(),
                signal: signal.into(),
                security: security.into(),
                freq: freq.into(),
                rate: rate.into(),
                ssid: rest.replace("\\:", ":"),
            }
        })
        .collect()
}

/// FREQ to the band's name: "5180 MHz" is 5 GHz. The megahertz never reach
/// the screen — they only say which of the three bands a row is on.
fn band_of(freq: &str) -> &'static str {
    let Ok(mhz) = freq.split(' ').next().unwrap_or_default().parse::<u64>() else {
        return "";
    };
    if mhz >= 5925 {
        "6 GHz"
    } else if mhz >= 4900 {
        "5 GHz"
    } else if mhz > 0 {
        "2.4 GHz"
    } else {
        ""
    }
}

/// The on-radio row answers the bare query and the radio words —
/// `wifi:cafe` is not asking for a switch.
fn radio_row_wanted(radio: &str, needle: &str) -> bool {
    radio == "enabled" && (needle.is_empty() || RADIO_WORDS.contains(needle))
}

/// The script's `emit` shape for one network: the base row, the primary
/// action in front of the extras, and the view's fields merged in.
fn network_row(ap: &Ap, saved: &HashSet<String>, score: i64) -> Value {
    // An SSID is attacker-controlled text — a nearby access point chooses
    // it — so every appearance in an exec goes through `quote`: the nmcli
    // argument, the notification text and the copy action alike.
    let q = quote(&ap.ssid);
    let lock = if ap.security.is_empty() {
        "Open"
    } else {
        &ap.security
    };
    let note_off = quote(&format!("Disconnected from {}", ap.ssid));
    let note_on = quote(&format!("Connected to {}", ap.ssid));
    let note_bad = quote(&format!("Could not connect to {}", ap.ssid));

    // 2.4 is one crowded band and 5/6 are two quiet ones — the whole reason
    // two access points with the same name behave differently. The rate is
    // only worth the width on the network actually in use.
    let mut meta = band_of(&ap.freq).to_string();
    if ap.in_use == "*" && !ap.rate.is_empty() {
        if !meta.is_empty() {
            meta.push_str(" · ");
        }
        meta.push_str(&ap.rate);
    }

    let (subtitle, primary, exec, extra, bonus, joined, known, mark);
    if ap.in_use == "*" {
        subtitle = "Connected";
        primary = "Disconnect";
        exec = format!(
            "out=$(nmcli connection down id {q} 2>&1) && omarchy-notification-send {note_off} || omarchy-notification-send -u normal \"Disconnect failed\" \"$out\""
        );
        extra = vec![
            json!({"title": "Copy Password", "exec": COPY_PASSWORD}),
            json!({"title": "Show QR Code", "exec": "omarchy-shell -q shell summon omarchy.wifiqr"}),
            json!({"title": "Speed Test", "exec": "omarchy-network-speedtest"}),
            json!({"title": "Open Network Panel", "exec": PANEL}),
        ];
        bonus = 9000;
        joined = true;
        known = true;
        mark = "saved";
    } else if saved.contains(&ap.ssid) {
        subtitle = "Saved";
        primary = "Connect";
        exec = format!(
            "out=$(nmcli connection up id {q} 2>&1) && omarchy-notification-send {note_on} || omarchy-notification-send -u normal {note_bad} \"$out\""
        );
        extra = vec![
            json!({"title": "Forget Network", "exec": format!("nmcli connection delete id {q}")}),
            json!({"title": "Open Network Panel", "exec": PANEL}),
        ];
        bonus = 4000;
        joined = false;
        known = true;
        mark = "saved";
    } else if ap.security.is_empty() {
        // Nothing to prompt for, so this one connects from the row.
        subtitle = "Open network";
        primary = "Connect";
        exec = format!(
            "out=$(nmcli device wifi connect {q} 2>&1) && omarchy-notification-send {note_on} || omarchy-notification-send -u normal {note_bad} \"$out\""
        );
        extra = vec![json!({"title": "Open Network Panel", "exec": PANEL})];
        bonus = 0;
        joined = false;
        known = false;
        mark = "";
    } else {
        subtitle = "Needs a password";
        primary = "Open Network Panel";
        exec = PANEL.to_string();
        extra = vec![
            json!({"title": "Connect in a Terminal", "exec": format!("omarchy-launch-tui --app-id=org.omarchy.wifi nmcli --ask device wifi connect {q}")}),
            json!({"title": "Copy Name", "exec": format!("printf %s {q} | wl-copy")}),
        ];
        bonus = 0;
        joined = false;
        known = false;
        mark = "";
    }

    let mut actions = Vec::with_capacity(extra.len() + 1);
    actions.push(json!({"title": primary, "shortcut": "↵", "exec": exec}));
    actions.extend(extra);

    let mut row = json!({
        "id": ap.ssid,
        "title": ap.ssid,
        "subtitle": subtitle,
        "detail": lock,
        "accessory": format!("{}%", ap.signal),
        "group": "Wi-Fi",
        "glyph": "",
        "exec": exec,
        "score": score + bonus,
        "actions": actions,
        "kind": "network",
        "radioOn": true,
        "radioLabel": "Wi-Fi",
        "joined": joined,
        "known": known,
        "mark": mark,
        "meta": meta,
    });
    // Emitted only when measured, never as a null or a fabricated zero: the
    // view draws an empty track for an unread strength and "open" for a
    // network with no security to report.
    if let Ok(sig) = ap.signal.parse::<i64>() {
        row["signal"] = json!(sig);
        row["signalLabel"] = json!(format!("{sig}%"));
    }
    if !ap.security.is_empty() {
        row["secure"] = json!(ap.security);
    }
    row
}

/// Every AP of a name arrives sorted by signal — the first row an SSID
/// produces is the strongest, so it is the one kept. Hidden networks have
/// no name to show.
fn network_rows(aps: &[Ap], saved: &HashSet<String>, needle: &str) -> Vec<Value> {
    let mut rows = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut score = 90000i64;
    for ap in aps {
        if ap.ssid.is_empty() || !seen.insert(ap.ssid.as_str()) {
            continue;
        }
        if !needle.is_empty() && !ap.ssid.to_lowercase().contains(needle) {
            continue;
        }
        rows.push(network_row(ap, saved, score));
        score -= 100;
    }
    rows
}

/// A radio that is off is the reason the list is empty, so it is the only
/// row — the view draws it as the whole answer.
fn radio_off_row() -> Value {
    json!({
        "id": "radio",
        "title": "Wi-Fi is off",
        "subtitle": "Radio",
        "accessory": "Off",
        "group": "Wi-Fi",
        "glyph": "",
        "exec": "nmcli radio wifi on",
        "score": 99000,
        "kind": "radio",
        "radioOn": false,
        "radioLabel": "Wi-Fi",
        "actions": [
            {"title": "Turn Wi-Fi On", "shortcut": "↵", "exec": "nmcli radio wifi on"},
            {"title": "Restart Wi-Fi", "exec": "omarchy-restart-wifi"},
        ],
    })
}

/// The radio strip: emitted before the networks even though it ranks below
/// them — maxRows truncates in emission order and the launcher sorts by
/// score after, so emitting it first is how the strip survives a busy
/// street while still drawing at the bottom.
fn radio_on_row(iface: &str) -> Value {
    json!({
        "id": "radio",
        "title": "Wi-Fi is on",
        "subtitle": "Radio",
        "accessory": "On",
        "group": "Wi-Fi",
        "glyph": "",
        "exec": "nmcli radio wifi off",
        "score": 60000,
        "kind": "radio",
        "radioOn": true,
        "radioLabel": "Wi-Fi",
        "iface": iface,
        "actions": [
            {"title": "Turn Wi-Fi Off", "shortcut": "↵", "exec": "nmcli radio wifi off"},
            {"title": "Rescan", "exec": "nmcli device wifi rescan"},
            {"title": "Restart Wi-Fi", "exec": "omarchy-restart-wifi"},
        ],
    })
}

/// The enabled answer: the radio strip first, then the networks in signal
/// order. `iface` is `None` when the strip was not wanted, which the caller
/// decided before paying for the `device` call.
fn enabled_rows(
    iface: Option<&str>,
    aps: &[Ap],
    saved: &HashSet<String>,
    needle: &str,
) -> Vec<Value> {
    let mut rows = Vec::new();
    if let Some(iface) = iface {
        rows.push(radio_on_row(iface));
    }
    rows.extend(network_rows(aps, saved, needle));
    rows
}

impl NativeExt for Wifi {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        let arg = ctx.arg.clone();
        Box::pin(async move {
            // The manifest's `when` is `command -v nmcli`; a worker asks a
            // native even when the gate failed, so it is re-checked here
            // before the script's work is taken over.
            if !on_path("nmcli") {
                return NativeOutcome::Fallback;
            }
            let needle = arg.to_lowercase();

            // The radio first — nothing below it can be true while it is
            // off, and the script stops after this one read in that case.
            let radio = radio_state(&nmcli("-t -f WIFI radio").await).to_string();
            if radio == "disabled" {
                return NativeOutcome::Rows(vec![radio_off_row()]);
            }

            // Three independent reads, joined: sequential 2s-bounded calls
            // could sum past the manifest's timeout under load. The device
            // read runs even when the radio row will not — its cost is a
            // local nmcli call, and the join makes it free anyway.
            let (saved_out, dev_out, aps_out) = tokio::join!(
                nmcli("-t -f TYPE,NAME connection show"),
                nmcli("-t -f DEVICE,TYPE device"),
                // The field list is verbatim — one unknown name and nmcli
                // prints nothing — and `--rescan no` keeps a keystroke from
                // emptying the list for the seconds a scan takes.
                nmcli("-t -f IN-USE,SIGNAL,SECURITY,FREQ,RATE,SSID device wifi list --rescan no"),
            );
            let saved = saved_profiles(&saved_out);
            let iface = radio_row_wanted(&radio, &needle).then(|| wifi_iface(&dev_out));
            let aps = parse_aps(&aps_out);
            NativeOutcome::Rows(enabled_rows(iface.as_deref(), &aps, &saved, &needle))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `nmcli -t` fixtures, shaped the way nmcli really writes them: colon
    /// fields, `\:` inside a value, IN-USE empty on every row but the one.
    const CONNECTIONS: &str = "802-11-wireless:HomeNet\n802-11-wireless:Cafe Guest\n802-3-ethernet:Wired connection 1\n802-11-wireless:My\\:Corp\n802-11-wireless:\nloopback:lo\n";
    const DEVICES: &str = "wlo1:wifi\nenp3s0:ethernet\nlo:loopback\n";
    const LIST: &str = "*:80:WPA2:5180 MHz:270 Mbit/s:HomeNet\n:65:WPA2:2412 MHz::Cafe Guest\n:55::2412 MHz:54 Mbit/s:Free Wifi\n:40:WPA3:5955 MHz::Corp\\:Net\n:30:WPA2:5180 MHz::HomeNet\n:::2412 MHz::\n";

    #[test]
    fn the_radio_reading() {
        assert_eq!(radio_state("enabled\n"), "enabled");
        assert_eq!(radio_state("disabled\n"), "disabled");
        assert_eq!(radio_state(""), "");
        assert_eq!(radio_state("garbage\n"), "garbage");
    }

    #[test]
    fn saved_profiles_are_wireless_only() {
        let saved = saved_profiles(CONNECTIONS);
        assert!(saved.contains("HomeNet"));
        assert!(saved.contains("Cafe Guest"));
        // The escaped colon in a profile name is undone.
        assert!(saved.contains("My:Corp"));
        assert!(!saved.contains("Wired connection 1"));
        assert_eq!(saved.len(), 3);
    }

    #[test]
    fn the_first_wifi_device_wins() {
        assert_eq!(wifi_iface(DEVICES), "wlo1");
        assert_eq!(wifi_iface("enp3s0:ethernet\n"), "");
        assert_eq!(wifi_iface(""), "");
    }

    #[test]
    fn an_ap_line_parses_its_six_fields() {
        let aps = parse_aps(LIST);
        assert_eq!(aps.len(), 6);
        let first = &aps[0];
        assert_eq!(first.in_use, "*");
        assert_eq!(first.signal, "80");
        assert_eq!(first.security, "WPA2");
        assert_eq!(first.freq, "5180 MHz");
        assert_eq!(first.rate, "270 Mbit/s");
        assert_eq!(first.ssid, "HomeNet");
        // The escaped colon in an SSID is undone, and only there.
        assert_eq!(aps[3].ssid, "Corp:Net");
        // An empty IN-USE column is the common case, not a missing field.
        assert_eq!(aps[1].in_use, "");
        assert_eq!(aps[2].security, "");
        // The hidden network parses with an empty SSID and is dropped later.
        assert_eq!(aps[5].ssid, "");
    }

    #[test]
    fn freq_names_a_band() {
        assert_eq!(band_of("2412 MHz"), "2.4 GHz");
        assert_eq!(band_of("5180 MHz"), "5 GHz");
        assert_eq!(band_of("5955 MHz"), "6 GHz");
        assert_eq!(band_of("6100 MHz"), "6 GHz");
        assert_eq!(band_of(""), "");
        assert_eq!(band_of("0 MHz"), "");
        assert_eq!(band_of("garbage"), "");
    }

    #[test]
    fn the_radio_row_answers_radio_words() {
        assert!(radio_row_wanted("enabled", ""));
        assert!(radio_row_wanted("enabled", "rescan"));
        assert!(radio_row_wanted("enabled", "radio"));
        assert!(!radio_row_wanted("enabled", "cafe"));
        assert!(!radio_row_wanted("disabled", ""));
        assert!(!radio_row_wanted("", ""));
    }

    #[test]
    fn a_disabled_radio_is_the_whole_answer() {
        let row = radio_off_row();
        assert_eq!(row["id"], "radio");
        assert_eq!(row["title"], "Wi-Fi is off");
        assert_eq!(row["kind"], "radio");
        assert_eq!(row["radioOn"], false);
        assert_eq!(row["radioLabel"], "Wi-Fi");
        assert_eq!(row["exec"], "nmcli radio wifi on");
        assert_eq!(row["score"], 99000);
        let actions = row["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0]["title"], "Turn Wi-Fi On");
        assert_eq!(actions[0]["exec"], "nmcli radio wifi on");
        assert_eq!(actions[1]["title"], "Restart Wi-Fi");
        assert_eq!(actions[1]["exec"], "omarchy-restart-wifi");
    }

    #[test]
    fn an_enabled_radio_is_a_strip() {
        let row = radio_on_row("wlo1");
        assert_eq!(row["title"], "Wi-Fi is on");
        assert_eq!(row["kind"], "radio");
        assert_eq!(row["radioOn"], true);
        assert_eq!(row["iface"], "wlo1");
        assert_eq!(row["exec"], "nmcli radio wifi off");
        assert_eq!(row["score"], 60000);
        let actions = row["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0]["exec"], "nmcli radio wifi off");
        assert_eq!(actions[1]["title"], "Rescan");
        assert_eq!(actions[1]["exec"], "nmcli device wifi rescan");
        assert_eq!(actions[2]["exec"], "omarchy-restart-wifi");
    }

    #[test]
    fn the_active_network_disconnects() {
        let saved = saved_profiles(CONNECTIONS);
        let rows = network_rows(&parse_aps(LIST), &saved, "");
        // Four networks: the duplicate AP and the hidden one are gone.
        assert_eq!(rows.len(), 4);
        let row = &rows[0];
        assert_eq!(row["id"], "HomeNet");
        assert_eq!(row["subtitle"], "Connected");
        assert_eq!(row["detail"], "WPA2");
        assert_eq!(row["accessory"], "80%");
        assert_eq!(row["score"], 99000);
        assert_eq!(
            row["exec"],
            "out=$(nmcli connection down id 'HomeNet' 2>&1) && omarchy-notification-send 'Disconnected from HomeNet' || omarchy-notification-send -u normal \"Disconnect failed\" \"$out\""
        );
        assert_eq!(row["joined"], true);
        assert_eq!(row["known"], true);
        assert_eq!(row["mark"], "saved");
        assert_eq!(row["kind"], "network");
        assert_eq!(row["radioOn"], true);
        assert_eq!(row["signal"], 80);
        assert_eq!(row["signalLabel"], "80%");
        assert_eq!(row["secure"], "WPA2");
        // In use: the rate joins the band on the dim line.
        assert_eq!(row["meta"], "5 GHz · 270 Mbit/s");
        let actions = row["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 5);
        assert_eq!(actions[0]["title"], "Disconnect");
        assert_eq!(actions[0]["exec"], row["exec"]);
        assert_eq!(actions[1]["title"], "Copy Password");
        assert_eq!(actions[1]["exec"], COPY_PASSWORD);
        assert_eq!(actions[2]["title"], "Show QR Code");
        assert_eq!(actions[3]["title"], "Speed Test");
        assert_eq!(actions[3]["exec"], "omarchy-network-speedtest");
        assert_eq!(actions[4]["title"], "Open Network Panel");
        assert_eq!(actions[4]["exec"], PANEL);
    }

    #[test]
    fn a_saved_profile_comes_up() {
        let saved = saved_profiles(CONNECTIONS);
        let rows = network_rows(&parse_aps(LIST), &saved, "");
        let row = &rows[1];
        assert_eq!(row["id"], "Cafe Guest");
        assert_eq!(row["subtitle"], "Saved");
        assert_eq!(row["detail"], "WPA2");
        // Second emitted: the base score has already stepped once.
        assert_eq!(row["score"], 93900);
        assert_eq!(
            row["exec"],
            "out=$(nmcli connection up id 'Cafe Guest' 2>&1) && omarchy-notification-send 'Connected to Cafe Guest' || omarchy-notification-send -u normal 'Could not connect to Cafe Guest' \"$out\""
        );
        assert_eq!(row["joined"], false);
        assert_eq!(row["known"], true);
        assert_eq!(row["mark"], "saved");
        // Not joined: the rate stays off the dim line.
        assert_eq!(row["meta"], "2.4 GHz");
        let actions = row["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0]["title"], "Connect");
        assert_eq!(actions[1]["title"], "Forget Network");
        assert_eq!(
            actions[1]["exec"],
            "nmcli connection delete id 'Cafe Guest'"
        );
        assert_eq!(actions[2]["title"], "Open Network Panel");
    }

    #[test]
    fn an_open_network_joins_from_the_row() {
        let saved = saved_profiles(CONNECTIONS);
        let rows = network_rows(&parse_aps(LIST), &saved, "");
        let row = &rows[2];
        assert_eq!(row["id"], "Free Wifi");
        assert_eq!(row["subtitle"], "Open network");
        assert_eq!(row["detail"], "Open");
        assert_eq!(row["score"], 89800);
        assert_eq!(
            row["exec"],
            "out=$(nmcli device wifi connect 'Free Wifi' 2>&1) && omarchy-notification-send 'Connected to Free Wifi' || omarchy-notification-send -u normal 'Could not connect to Free Wifi' \"$out\""
        );
        assert_eq!(row["joined"], false);
        assert_eq!(row["known"], false);
        assert_eq!(row["mark"], "");
        // Open: no lock word is emitted, which is what the view reads as
        // "open" — absent, not null and not an empty string.
        assert!(row.get("secure").is_none());
        let actions = row["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0]["title"], "Connect");
        assert_eq!(actions[1]["title"], "Open Network Panel");
    }

    #[test]
    fn a_new_secured_network_opens_the_panel() {
        let saved = saved_profiles(CONNECTIONS);
        let rows = network_rows(&parse_aps(LIST), &saved, "");
        let row = &rows[3];
        assert_eq!(row["id"], "Corp:Net");
        assert_eq!(row["subtitle"], "Needs a password");
        assert_eq!(row["detail"], "WPA3");
        assert_eq!(row["exec"], PANEL);
        assert_eq!(row["score"], 89700);
        assert_eq!(row["secure"], "WPA3");
        assert_eq!(row["meta"], "6 GHz");
        let actions = row["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0]["title"], "Open Network Panel");
        assert_eq!(actions[0]["exec"], PANEL);
        assert_eq!(actions[1]["title"], "Connect in a Terminal");
        assert_eq!(
            actions[1]["exec"],
            "omarchy-launch-tui --app-id=org.omarchy.wifi nmcli --ask device wifi connect 'Corp:Net'"
        );
        assert_eq!(actions[2]["title"], "Copy Name");
        assert_eq!(actions[2]["exec"], "printf %s 'Corp:Net' | wl-copy");
    }

    #[test]
    fn a_hostile_ssid_is_quoted_everywhere() {
        let aps = parse_aps(":50:WPA2:2412 MHz::It's a trap\n");
        let rows = network_rows(&aps, &HashSet::new(), "");
        assert_eq!(rows[0]["id"], "It's a trap");
        // The panel exec carries no SSID; the terminal connect does.
        assert_eq!(
            rows[0]["actions"][1]["exec"],
            "omarchy-launch-tui --app-id=org.omarchy.wifi nmcli --ask device wifi connect 'It'\\''s a trap'"
        );
    }

    #[test]
    fn the_needle_filters_by_ssid() {
        let saved = saved_profiles(CONNECTIONS);
        let rows = network_rows(&parse_aps(LIST), &saved, "cafe");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], "Cafe Guest");
        // Case-insensitive on the SSID side, the script's `${ssid,,}` — the
        // needle itself arrives already folded (`query` lowers `${query,,}`).
        let rows = network_rows(&parse_aps(LIST), &saved, "corp");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], "Corp:Net");
        // A needle nothing carries is an empty answer.
        assert!(network_rows(&parse_aps(LIST), &saved, "zzz").is_empty());
    }

    #[test]
    fn an_unmeasured_signal_draws_nothing() {
        let aps = parse_aps(":::2412 MHz::NoSig\n");
        let rows = network_rows(&aps, &HashSet::new(), "");
        assert_eq!(rows.len(), 1);
        // Absent, never a fabricated zero on the meter.
        assert!(rows[0].get("signal").is_none());
        assert!(rows[0].get("signalLabel").is_none());
    }

    #[test]
    fn the_enabled_answer_puts_the_strip_first() {
        let saved = saved_profiles(CONNECTIONS);
        let rows = enabled_rows(Some("wlo1"), &parse_aps(LIST), &saved, "");
        // Emission order is what maxRows truncates, so the strip leads even
        // though its score ranks it below every network.
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0]["kind"], "radio");
        assert_eq!(rows[0]["iface"], "wlo1");
        assert_eq!(rows[1]["id"], "HomeNet");
        // And when the needle does not want it, the strip is simply absent.
        let rows = enabled_rows(None, &parse_aps(LIST), &saved, "cafe");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], "Cafe Guest");
    }
}
