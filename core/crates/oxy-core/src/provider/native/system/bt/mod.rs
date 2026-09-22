//! `bt:` — paired and nearby Bluetooth devices, and the radio itself. A port
//! of `bin/oxy-bluetooth`.
//!
//! The rules the script's own comments insist on, kept:
//!
//! - Reading is **one** `busctl --json=short` call to BlueZ
//!   `GetManagedObjects`: every device with every property in a single
//!   message, so battery, kind and RSSI cost nothing extra. `bluetoothctl`
//!   is the fallback for a machine without busctl or the object manager —
//!   the extra fields are simply absent then.
//! - `signal`/`signalLabel`/`battery` are emitted **only when measured**: an
//!   absent field and a null field read the same to the `radios` view, but
//!   only the absent one leaves the meter and the battery cell undrawn.
//! - Adapter off is not one row among the devices — the radio-off row is the
//!   whole answer.
//! - Acting goes through `omarchy-bluetooth-device`, never a bare
//!   `bluetoothctl connect`, because a connect must survive the rfkill soft
//!   block Omarchy uses as the real on/off state (the §6.6 trap). The radio
//!   row runs `omarchy-bluetooth-power on|off`.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::native::util::on_path;
use crate::provider::process;
use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::support::quote::quote;

#[cfg(test)]
mod tests;

pub struct Bt;

/// The script's exact invocation — one message, every managed object.
const BUSCTL_CALL: &str = "timeout 3s busctl --json=short call org.bluez / \
    org.freedesktop.DBus.ObjectManager GetManagedObjects";

/// The fallback's three reads. The script ran them in one spawn separated by
/// \037; here they run as three bounded `process::run`s at once — sequential
/// `timeout` calls could sum past the worker deadline and read as "off".
const BLUETOOTHCTL_PROBE: &[&str] = &[
    "bluetoothctl show",
    "bluetoothctl devices Connected",
    "bluetoothctl devices Paired",
];

/// One row's worth of facts, from whichever reader ran. On the bluetoothctl
/// path `rssi`, `battery` and `icon` are simply absent — as they are in the
/// script's fallback.
struct Device {
    address: String,
    /// Alias → Name → Address, control characters folded to spaces. Empty is
    /// possible on the bluetoothctl path; the row falls back to the address.
    name: String,
    paired: bool,
    connected: bool,
    /// The raw reading. `None` means "not heard from", not "weak".
    rssi: Option<f64>,
    battery: Option<f64>,
    icon: String,
}

impl Device {
    /// The reading order the view wants — connected, paired, nearby.
    fn group(&self) -> u8 {
        if self.connected {
            0
        } else if self.paired {
            1
        } else {
            2
        }
    }
}

/// jq's `//` reading of `prop.data`: null and false both count as absent.
fn prop<'a>(iface: &'a Value, name: &str) -> Option<&'a Value> {
    iface
        .get(name)
        .and_then(|p| p.get("data"))
        .filter(|v| !matches!(v, Value::Null | Value::Bool(false)))
}

/// The managed-objects answer taken apart the way the script's jq did:
/// `data[0]` is a map of object path → interface → property → {type, data}.
/// Returns (adapter count, any adapter powered, device rows in view order).
///
/// An unparsable answer is the script's jq failing: `adapters` reads as zero
/// and the caller lands on the rfkill question rather than on bluetoothctl.
fn parse_managed(text: &str) -> (usize, bool, Vec<Device>) {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return (0, false, Vec::new());
    };
    let Some(map) = v
        .get("data")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_object)
    else {
        return (0, false, Vec::new());
    };

    let mut adapters = 0usize;
    let mut powered = false;
    let mut devices = Vec::new();
    for entry in map.values() {
        // jq's `has()` requires an object; a malformed entry kills the whole
        // read, which the caller then reads as "no adapters" — not a crash.
        let Some(entry) = entry.as_object() else {
            return (0, false, Vec::new());
        };
        if let Some(a) = entry.get("org.bluez.Adapter1") {
            adapters += 1;
            powered |= prop(a, "Powered").and_then(Value::as_bool).unwrap_or(false);
        }
        let Some(d) = entry.get("org.bluez.Device1") else {
            continue;
        };
        let address = prop(d, "Address")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if address.is_empty() {
            continue;
        }
        // A control character in an advertised name would eat a field
        // separator or a whole line, and a device gets to choose its own
        // name — fold them the way the jq `gsub` did.
        let name: String = ["Alias", "Name", "Address"]
            .iter()
            .find_map(|k| prop(d, k).and_then(Value::as_str))
            .unwrap_or("")
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        // `named` asks specifically about the advertised Name: an Alias-only
        // device does not count as something you could recognise.
        let named = prop(d, "Name")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
        let connected = prop(d, "Connected")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let paired = prop(d, "Paired").and_then(Value::as_bool).unwrap_or(false)
            || prop(d, "Bonded").and_then(Value::as_bool).unwrap_or(false);
        let rssi = prop(d, "RSSI").and_then(Value::as_f64);
        let battery = entry
            .get("org.bluez.Battery1")
            .and_then(|b| prop(b, "Percentage"))
            .and_then(Value::as_f64);
        let icon = prop(d, "Icon")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        // BlueZ remembers everything it has ever heard advertise. An
        // unpaired device earns a row only with both a name and a live
        // RSSI — "in the room right now", not a beacon that walked past
        // last week.
        if !paired && !(rssi.is_some() && named) {
            continue;
        }
        devices.push(Device {
            address,
            name,
            paired,
            connected,
            rssi,
            battery,
            icon,
        });
    }
    // The jq sort: group first, then strongest signal (an unheard-from
    // device reads as -127 and sinks), then name.
    devices.sort_by(|a, b| {
        a.group()
            .cmp(&b.group())
            .then_with(|| (-a.rssi.unwrap_or(-127.0)).total_cmp(&-b.rssi.unwrap_or(-127.0)))
            .then_with(|| {
                a.name
                    .to_ascii_lowercase()
                    .cmp(&b.name.to_ascii_lowercase())
            })
    });
    (adapters, powered, devices)
}

/// The bluetoothctl reading: `show` carries Powered, `devices Connected` and
/// `devices Paired` carry names and connectedness — no battery, kind or
/// signal, and the view draws the empty meter that means exactly that.
fn parse_bluetoothctl(show: &str, connected: &str, paired: &str) -> (bool, Vec<Device>) {
    let powered = show.contains("Powered: yes");
    // `sort -t' ' -k3` orders the raw lines by the name field; the MAC check
    // in the row loop is what drops chatter that made it past `^Device `.
    let mut listed: Vec<(String, String)> = paired
        .lines()
        .filter(|l| l.starts_with("Device "))
        .map(|l| {
            let rest = l["Device ".len()..].trim_start();
            match rest.find(char::is_whitespace) {
                Some(i) => (rest[..i].to_string(), rest[i..].trim().to_string()),
                None => (rest.to_string(), String::new()),
            }
        })
        .collect();
    listed.sort_by(|a, b| a.1.cmp(&b.1));
    let devices = listed
        .into_iter()
        .filter(|(a, _)| is_address(a))
        .map(|(address, name)| {
            let conn = connected.contains(&address);
            Device {
                address,
                name,
                paired: true,
                connected: conn,
                rssi: None,
                battery: None,
                icon: String::new(),
            }
        })
        .collect();
    (powered, devices)
}

/// `rfkill list bluetooth` printing anything at all is how the script tells
/// "the soft block took hci0 down" from "this machine has no Bluetooth" —
/// the two want opposite answers: the off card, or silence.
async fn rfkill_has_bluetooth() -> bool {
    process::run("rfkill list bluetooth", Duration::from_secs(2))
        .await
        .map(|f| f.stdout.lines().any(|l| !l.trim().is_empty()))
        .unwrap_or(false)
}

/// The `^([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}$` the while-loop guarded on.
fn is_address(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 17
        && b.iter().enumerate().all(|(i, &c)| {
            if i % 3 == 2 {
                c == b':'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

/// The script's `matches`: an empty needle passes everything; otherwise the
/// lowercase `name address` haystack must contain it.
fn matches(needle: &str, name: &str, extra: &str) -> bool {
    needle.is_empty() || format!("{name} {extra}").to_lowercase().contains(needle)
}

/// The freedesktop icon name BlueZ reports, said out loud — the script's
/// `kind_of` table, with the family-strip fallback for an unlisted icon.
fn kind_of(icon: &str) -> String {
    match icon {
        "audio-headset" => "headset".into(),
        "audio-headphones" => "headphones".into(),
        "audio-card" | "audio-speakers" => "speaker".into(),
        "input-mouse" => "mouse".into(),
        "input-keyboard" => "keyboard".into(),
        "input-gaming" => "gamepad".into(),
        "input-tablet" => "tablet".into(),
        "camera-photo" | "camera-video" => "camera".into(),
        "video-display" => "display".into(),
        "phone" => "phone".into(),
        "computer" => "computer".into(),
        "printer" => "printer".into(),
        "scanner" => "scanner".into(),
        "" => String::new(),
        other => {
            let s = other.strip_prefix("audio-").unwrap_or(other);
            let s = s.strip_prefix("input-").unwrap_or(s);
            s.replace('-', " ")
        }
    }
}

/// RSSI is dBm, a negative logarithm nobody compares by eye. The meter wants
/// a proportion, so -100 is nothing and -50 is everything — the band a
/// room-sized radio lives in, and either end of it saturates. A non-integral
/// reading fails the script's `^-?[0-9]+$` and draws nothing.
fn signal_fields(rssi: Option<f64>) -> Option<(i64, String)> {
    let r = rssi?;
    if r.fract() != 0.0 || r < i64::MIN as f64 || r > i64::MAX as f64 {
        return None;
    }
    let r = r as i64;
    Some((((r + 100) * 2).clamp(0, 100), format!("{r} dBm")))
}

/// A measured value as JSON: whole numbers stay ints (`85`, not `85.0`).
fn measured(v: f64) -> Value {
    if v.fract() == 0.0 && v.abs() < 9e15 {
        json!(v as i64)
    } else {
        json!(v)
    }
}

/// The one row a switched-off adapter emits — the whole answer, not a row
/// among devices.
fn radio_row_off() -> Value {
    json!({
        "id": "power",
        "title": "Bluetooth is off",
        "subtitle": "Adapter",
        "accessory": "Off",
        "group": "Bluetooth",
        "glyph": "",
        "exec": "omarchy-bluetooth-power on",
        "score": 99000,
        "kind": "radio",
        "radioOn": false,
        "radioLabel": "Bluetooth",
        "actions": [
            { "title": "Turn Bluetooth On", "shortcut": "↵",
              "exec": "omarchy-bluetooth-power on" },
            { "title": "Restart Bluetooth", "exec": "omarchy-restart-bluetooth" },
        ],
    })
}

/// The strip at the foot of the list while the radio is on.
fn radio_row_on() -> Value {
    json!({
        "id": "power",
        "title": "Bluetooth is on",
        "subtitle": "Adapter",
        "accessory": "On",
        "group": "Bluetooth",
        "glyph": "",
        "exec": "omarchy-bluetooth-power off",
        "score": 60000,
        "kind": "radio",
        "radioOn": true,
        "radioLabel": "Bluetooth",
        "iface": "hci0",
        "actions": [
            { "title": "Turn Bluetooth Off", "shortcut": "↵",
              "exec": "omarchy-bluetooth-power off" },
            { "title": "Pair a New Device",
              "exec": "omarchy-shell -q shell summon omarchy.bluetooth" },
            { "title": "Restart Bluetooth", "exec": "omarchy-restart-bluetooth" },
        ],
    })
}

/// One device row, field-for-field the script's jq emit. `rank` is the
/// script's `score` bookkeeping handed in by the caller.
fn device_row(d: &Device, name: &str, rank: i64) -> Value {
    // Enter's promise swaps by group — and `omarchy-bluetooth-device`, not
    // bluetoothctl, because a connect has to survive the rfkill soft block.
    let (state, primary, exec, joined, known, mark, extra) = match d.group() {
        0 => (
            "Connected",
            "Disconnect",
            format!("omarchy-bluetooth-device disconnect {}", d.address),
            true,
            true,
            "paired",
            vec![json!({
                "title": "Connect",
                "exec": format!("omarchy-bluetooth-device connect {}", d.address),
            })],
        ),
        1 => (
            "Paired",
            "Connect",
            format!("omarchy-bluetooth-device connect {}", d.address),
            false,
            true,
            "paired",
            vec![json!({
                "title": "Disconnect",
                "exec": format!("omarchy-bluetooth-device disconnect {}", d.address),
            })],
        ),
        _ => (
            "Nearby",
            "Pair",
            format!("omarchy-bluetooth-device pair {}", d.address),
            false,
            false,
            "nearby",
            Vec::new(),
        ),
    };

    let mut actions = vec![json!({ "title": primary, "shortcut": "↵", "exec": &exec })];
    actions.extend(extra);
    actions.push(json!({
        "title": "Forget Device",
        "exec": format!("omarchy-bluetooth-device forget {}", d.address),
    }));
    actions.push(json!({
        "title": "Copy Address",
        "exec": format!("printf %s {} | wl-copy", quote(&d.address)),
    }));
    actions.push(json!({
        "title": "Open Bluetooth Panel",
        "exec": "omarchy-shell -q shell summon omarchy.bluetooth",
    }));

    let mut row = json!({
        "id": d.address,
        "title": name,
        "subtitle": state,
        "detail": d.address,
        "accessory": state,
        "group": "Bluetooth",
        "glyph": "",
        "exec": exec,
        "score": rank,
        "kind": "device",
        "radioOn": true,
        "radioLabel": "Bluetooth",
        "joined": joined,
        "known": known,
        "mark": mark,
        "deviceKind": kind_of(&d.icon),
        "meta": d.address,
        "actions": actions,
    });
    // An absent field and a field set to null read the same to the view, but
    // only the absent one leaves the meter and the battery cell undrawn — so
    // nothing that was never measured gets a zero drawn for it.
    if let Some((signal, label)) = signal_fields(d.rssi) {
        let obj = row.as_object_mut().unwrap();
        obj.insert("signal".into(), json!(signal));
        obj.insert("signalLabel".into(), json!(label));
    }
    if let Some(b) = d.battery {
        row.as_object_mut()
            .unwrap()
            .insert("battery".into(), measured(b));
    }
    row
}

/// The whole emit sequence, pure so the tests can drive it: off is one row;
/// on is the adapter strip (when it matches) then devices in view order.
fn rows_for(powered: bool, devices: &[Device], needle: &str) -> Vec<Value> {
    if !powered {
        return vec![radio_row_off()];
    }
    let mut rows = Vec::new();
    // The adapter row is emitted before the devices even though it ranks
    // below them: maxRows truncates in emission order and the launcher sorts
    // by score after, so this is how the strip survives a room full of
    // headphones.
    if matches(needle, "bluetooth power adapter on off pair", "") {
        rows.push(radio_row_on());
    }
    let mut score = 90000i64;
    for d in devices {
        if !is_address(&d.address) {
            continue;
        }
        let name = if d.name.is_empty() {
            d.address.as_str()
        } else {
            d.name.as_str()
        };
        if !matches(needle, name, &d.address) {
            continue;
        }
        // Connected rows rank 5000 above, paired 2000 — enough that a
        // connection always sorts over the list, not by name order.
        let rank = score
            + match d.group() {
                0 => 5000,
                1 => 2000,
                _ => 0,
            };
        rows.push(device_row(d, name, rank));
        // Skipped rows do not spend score — the script's `continue` jumps
        // over the decrement too.
        score -= 100;
    }
    rows
}

async fn answer(needle: &str) -> NativeOutcome {
    let mut powered = None;
    let mut devices = Vec::new();

    // An absent busctl, or a call that prints nothing, takes the
    // bluetoothctl path exactly as the script's empty `bluez` did.
    let bluez = if on_path("busctl") {
        process::run(BUSCTL_CALL, Duration::from_secs(4)).await
    } else {
        None
    };
    if let Some(out) = bluez
        && !out.stdout.trim().is_empty()
    {
        let (adapters, any_on, found) = parse_managed(&out.stdout);
        devices = found;
        if adapters == 0 {
            // No adapter object at all. Either this machine has no
            // Bluetooth, or the soft block took hci0 down with it, and
            // those two want opposite answers: silence, or the whole card
            // saying it is off. rfkill is the one that knows.
            if !rfkill_has_bluetooth().await {
                return NativeOutcome::Empty;
            }
            powered = Some(false);
        } else {
            powered = Some(any_on);
        }
    }

    let powered = match powered {
        Some(p) => p,
        None => {
            // The fallback: names and connectedness is all bluetoothctl
            // gives without a round trip per device. `show` not saying
            // `Powered: yes` reads as off — but a probe that timed out is
            // "unknown", not "off": the script leg answers it instead of a
            // slow stack being reported dead.
            let (a, b, c) = tokio::join!(
                process::run(BLUETOOTHCTL_PROBE[0], Duration::from_millis(2500)),
                process::run(BLUETOOTHCTL_PROBE[1], Duration::from_millis(2500)),
                process::run(BLUETOOTHCTL_PROBE[2], Duration::from_millis(2500)),
            );
            let (Some(a), Some(b), Some(c)) = (a, b, c) else {
                return NativeOutcome::Fallback;
            };
            if a.timed_out || b.timed_out || c.timed_out {
                return NativeOutcome::Fallback;
            }
            let (on, found) = parse_bluetoothctl(&a.stdout, &b.stdout, &c.stdout);
            devices = found;
            on
        }
    };

    NativeOutcome::Rows(rows_for(powered, &devices, needle))
}

impl NativeExt for Bt {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        // `needle="${query,,}"` — the argument lowercased, untrimmed, as the
        // script took it.
        let needle = ctx.arg.to_lowercase();
        Box::pin(async move {
            // The manifest's `when` re-checked: a worker asks natives even
            // when the gate fails, so an absent bluetoothctl declines to the
            // script leg rather than answering for it.
            if !on_path("bluetoothctl") {
                return NativeOutcome::Fallback;
            }
            answer(&needle).await
        })
    }
}
