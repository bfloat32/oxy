//! The `bt:` suite: canned `busctl`/`bluetoothctl` output through the same
//! parse the provider runs — the tools do not exist on every machine the
//! tests do, so the fixtures are the verification.

use super::*;

/// A realistic `busctl --json=short call … GetManagedObjects` dump: one
/// powered adapter and five device objects — a connected headset with
/// battery and RSSI, a paired keyboard with RSSI, a paired mouse with
/// neither, a nearby unpaired phone, and a nameless beacon that must be
/// filtered out.
const MANAGED: &str = r#"{
  "type": "a{oa{sa{sv}}}",
  "data": [
    {
      "/org/bluez": {
        "org.bluez.AgentManager1": {},
        "org.bluez.ProfileManager1": {},
        "org.freedesktop.DBus.Introspectable1": {},
        "org.freedesktop.DBus.ObjectManager": {}
      },
      "/org/bluez/hci0": {
        "org.bluez.Adapter1": {
          "Address": {"type": "s", "data": "9C:B6:D0:AA:BB:CC"},
          "AddressType": {"type": "s", "data": "public"},
          "Name": {"type": "s", "data": "omarchy"},
          "Alias": {"type": "s", "data": "omarchy"},
          "Class": {"type": "u", "data": 7077904},
          "Powered": {"type": "b", "data": true},
          "Discoverable": {"type": "b", "data": false},
          "Pairable": {"type": "b", "data": true},
          "Discovering": {"type": "b", "data": false},
          "UUIDs": {"type": "as", "data": [
            "0000110e-0000-1000-8000-00805f9b34fb",
            "0000110a-0000-1000-8000-00805f9b34fb"
          ]},
          "Roles": {"type": "as", "data": ["central", "peripheral"]}
        },
        "org.bluez.Media1": {},
        "org.bluez.NetworkServer1": {},
        "org.bluez.LEAdvertisingManager1": {
          "ActiveInstances": {"type": "y", "data": 0},
          "SupportedInstances": {"type": "y", "data": 12}
        },
        "org.freedesktop.DBus.Introspectable1": {},
        "org.freedesktop.DBus.Properties": {}
      },
      "/org/bluez/hci0/dev_A1_B2_C3_D4_E5_F6": {
        "org.freedesktop.DBus.Introspectable1": {},
        "org.bluez.Device1": {
          "Address": {"type": "s", "data": "A1:B2:C3:D4:E5:F6"},
          "AddressType": {"type": "s", "data": "public"},
          "Name": {"type": "s", "data": "WH-1000XM5"},
          "Alias": {"type": "s", "data": "WH-1000XM5"},
          "Class": {"type": "u", "data": 2360328},
          "Icon": {"type": "s", "data": "audio-headset"},
          "Paired": {"type": "b", "data": true},
          "Bonded": {"type": "b", "data": true},
          "Trusted": {"type": "b", "data": true},
          "Blocked": {"type": "b", "data": false},
          "LegacyPairing": {"type": "b", "data": false},
          "Connected": {"type": "b", "data": true},
          "RSSI": {"type": "n", "data": -75},
          "UUIDs": {"type": "as", "data": [
            "0000110a-0000-1000-8000-00805f9b34fb"
          ]},
          "Modalias": {"type": "s", "data": "usb:v054Cp0D58d0410"},
          "Adapter": {"type": "o", "data": "/org/bluez/hci0"},
          "ServicesResolved": {"type": "b", "data": true}
        },
        "org.bluez.MediaControl1": {
          "Connected": {"type": "b", "data": true},
          "Player": {"type": "o",
            "data": "/org/bluez/hci0/dev_A1_B2_C3_D4_E5_F6/player0"}
        },
        "org.bluez.Battery1": {
          "Percentage": {"type": "y", "data": 85}
        },
        "org.freedesktop.DBus.Properties": {}
      },
      "/org/bluez/hci0/dev_B2_C3_D4_E5_F6_A7": {
        "org.bluez.Device1": {
          "Address": {"type": "s", "data": "B2:C3:D4:E5:F6:A7"},
          "AddressType": {"type": "s", "data": "random"},
          "Name": {"type": "s", "data": "MX Master 3S"},
          "Alias": {"type": "s", "data": "MX Master 3S"},
          "Icon": {"type": "s", "data": "input-mouse"},
          "Paired": {"type": "b", "data": true},
          "Bonded": {"type": "b", "data": true},
          "Trusted": {"type": "b", "data": true},
          "Connected": {"type": "b", "data": false},
          "Adapter": {"type": "o", "data": "/org/bluez/hci0"}
        },
        "org.freedesktop.DBus.Properties": {}
      },
      "/org/bluez/hci0/dev_C3_D4_E5_F6_A7_B8": {
        "org.bluez.Device1": {
          "Address": {"type": "s", "data": "C3:D4:E5:F6:A7:B8"},
          "AddressType": {"type": "s", "data": "public"},
          "Name": {"type": "s", "data": "ZSA Voyager"},
          "Alias": {"type": "s", "data": "ZSA Voyager"},
          "Icon": {"type": "s", "data": "input-keyboard"},
          "Paired": {"type": "b", "data": true},
          "Connected": {"type": "b", "data": false},
          "RSSI": {"type": "n", "data": -60},
          "Adapter": {"type": "o", "data": "/org/bluez/hci0"}
        },
        "org.freedesktop.DBus.Properties": {}
      },
      "/org/bluez/hci0/dev_D4_E5_F6_A7_B8_C9": {
        "org.bluez.Device1": {
          "Address": {"type": "s", "data": "D4:E5:F6:A7:B8:C9"},
          "AddressType": {"type": "s", "data": "random"},
          "Name": {"type": "s", "data": "Pixel 9"},
          "Alias": {"type": "s", "data": "Pixel 9"},
          "Icon": {"type": "s", "data": "phone"},
          "Paired": {"type": "b", "data": false},
          "Connected": {"type": "b", "data": false},
          "RSSI": {"type": "n", "data": -70},
          "Adapter": {"type": "o", "data": "/org/bluez/hci0"}
        },
        "org.freedesktop.DBus.Properties": {}
      },
      "/org/bluez/hci0/dev_E5_F6_A7_B8_C9_D0": {
        "org.bluez.Device1": {
          "Address": {"type": "s", "data": "E5:F6:A7:B8:C9:D0"},
          "AddressType": {"type": "s", "data": "random"},
          "Paired": {"type": "b", "data": false},
          "Connected": {"type": "b", "data": false},
          "RSSI": {"type": "n", "data": -91},
          "Adapter": {"type": "o", "data": "/org/bluez/hci0"}
        },
        "org.freedesktop.DBus.Properties": {}
      }
    }
  ]
}"#;

/// The same dump with the adapter's Powered false.
const MANAGED_OFF: &str = r#"{
  "type": "a{oa{sa{sv}}}",
  "data": [
    {
      "/org/bluez/hci0": {
        "org.bluez.Adapter1": {
          "Address": {"type": "s", "data": "9C:B6:D0:AA:BB:CC"},
          "Powered": {"type": "b", "data": false}
        }
      },
      "/org/bluez/hci0/dev_A1_B2_C3_D4_E5_F6": {
        "org.bluez.Device1": {
          "Address": {"type": "s", "data": "A1:B2:C3:D4:E5:F6"},
          "Name": {"type": "s", "data": "WH-1000XM5"},
          "Paired": {"type": "b", "data": true},
          "Connected": {"type": "b", "data": false}
        }
      }
    }
  ]
}"#;

/// A BlueZ with no adapter objects — what a soft block leaves behind.
const MANAGED_NO_ADAPTER: &str = r#"{
  "type": "a{oa{sa{sv}}}",
  "data": [
    {
      "/org/bluez": {
        "org.bluez.AgentManager1": {},
        "org.freedesktop.DBus.ObjectManager": {}
      }
    }
  ]
}"#;

/// The fallback fixture: `bluetoothctl show`, `devices Connected`,
/// `devices Paired` — including chatter `^Device ` filters, an invalid
/// address the row loop drops, and the unsorted listing `sort -k3` orders.
const BT_SHOW: &str = "Controller 9C:B6:D0:AA:BB:CC (public)\n\tName: omarchy\n\
    \tAlias: omarchy\n\tPowered: yes\n\tDiscoverable: no\n";
const BT_CONNECTED: &str = "Device A1:B2:C3:D4:E5:F6 WH-1000XM5\n";
const BT_PAIRED: &str = "Attempting to retrieve devices\n\
    Device A1:B2:C3:D4:E5:F6 WH-1000XM5\n\
    Device B2:C3:D4:E5:F6:A7 MX Master 3S\n\
    Device not-an-address Broken\n";

fn managed_devices() -> Vec<Device> {
    let (adapters, powered, devices) = parse_managed(MANAGED);
    assert_eq!(adapters, 1);
    assert!(powered);
    devices
}

#[test]
fn managed_parses_devices_in_view_order() {
    let devices = managed_devices();
    // Connected first, then paired by signal strength (the keyboard's -60
    // beats the mouse's silence, which sorts as -127), then nearby. The
    // nameless beacon is filtered out entirely.
    let addrs: Vec<&str> = devices.iter().map(|d| d.address.as_str()).collect();
    assert_eq!(
        addrs,
        [
            "A1:B2:C3:D4:E5:F6",
            "C3:D4:E5:F6:A7:B8",
            "B2:C3:D4:E5:F6:A7",
            "D4:E5:F6:A7:B8:C9"
        ]
    );
    let a = &devices[0];
    assert!(a.connected && a.paired);
    assert_eq!(a.rssi, Some(-75.0));
    assert_eq!(a.battery, Some(85.0));
    assert_eq!(a.icon, "audio-headset");
    let b = &devices[2];
    assert!(!b.connected && b.paired);
    assert_eq!(b.rssi, None);
    assert_eq!(b.battery, None);
}

#[test]
fn managed_off_and_absent_adapters() {
    let (adapters, powered, _) = parse_managed(MANAGED_OFF);
    assert_eq!(adapters, 1);
    assert!(!powered);

    let (adapters, powered, devices) = parse_managed(MANAGED_NO_ADAPTER);
    assert_eq!(adapters, 0);
    assert!(!powered);
    assert!(devices.is_empty());

    // jq dying on garbage reads as zero adapters, not as a crash.
    let (adapters, _, _) = parse_managed("not json at all");
    assert_eq!(adapters, 0);
}

#[test]
fn radio_off_is_the_whole_answer() {
    let devices = managed_devices();
    let rows = rows_for(false, &devices, "");
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!(r["id"], "power");
    assert_eq!(r["title"], "Bluetooth is off");
    assert_eq!(r["subtitle"], "Adapter");
    assert_eq!(r["accessory"], "Off");
    assert_eq!(r["kind"], "radio");
    assert_eq!(r["radioOn"], false);
    assert_eq!(r["radioLabel"], "Bluetooth");
    assert_eq!(r["score"], 99000);
    assert_eq!(r["exec"], "omarchy-bluetooth-power on");
    let actions = r["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 2);
    assert_eq!(actions[0]["title"], "Turn Bluetooth On");
    assert_eq!(actions[0]["shortcut"], "↵");
    assert_eq!(actions[0]["exec"], "omarchy-bluetooth-power on");
    assert_eq!(actions[1]["exec"], "omarchy-restart-bluetooth");
}

#[test]
fn radio_on_row_emitted_before_devices() {
    let devices = managed_devices();
    let rows = rows_for(true, &devices, "");
    assert_eq!(rows.len(), 5);
    let r = &rows[0];
    assert_eq!(r["kind"], "radio");
    assert_eq!(r["title"], "Bluetooth is on");
    assert_eq!(r["accessory"], "On");
    assert_eq!(r["radioOn"], true);
    assert_eq!(r["radioLabel"], "Bluetooth");
    assert_eq!(r["iface"], "hci0");
    assert_eq!(r["score"], 60000);
    assert_eq!(r["exec"], "omarchy-bluetooth-power off");
    let titles: Vec<&str> = r["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        [
            "Turn Bluetooth Off",
            "Pair a New Device",
            "Restart Bluetooth"
        ]
    );
}

#[test]
fn connected_device_row_is_verbatim() {
    let devices = managed_devices();
    let rows = rows_for(true, &devices, "");
    let r = &rows[1];
    assert_eq!(r["id"], "A1:B2:C3:D4:E5:F6");
    assert_eq!(r["title"], "WH-1000XM5");
    assert_eq!(r["subtitle"], "Connected");
    assert_eq!(r["detail"], "A1:B2:C3:D4:E5:F6");
    assert_eq!(r["accessory"], "Connected");
    assert_eq!(r["group"], "Bluetooth");
    assert_eq!(r["kind"], "device");
    assert_eq!(r["radioOn"], true);
    assert_eq!(r["radioLabel"], "Bluetooth");
    assert_eq!(r["joined"], true);
    assert_eq!(r["known"], true);
    assert_eq!(r["mark"], "paired");
    assert_eq!(r["deviceKind"], "headset");
    assert_eq!(r["meta"], "A1:B2:C3:D4:E5:F6");
    assert_eq!(
        r["exec"],
        "omarchy-bluetooth-device disconnect A1:B2:C3:D4:E5:F6"
    );
    assert_eq!(r["score"], 95000);
    assert_eq!(r["signal"], 50);
    assert_eq!(r["signalLabel"], "-75 dBm");
    assert_eq!(r["battery"], 85);
    let actions = r["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 5);
    assert_eq!(actions[0]["title"], "Disconnect");
    assert_eq!(actions[0]["shortcut"], "↵");
    assert_eq!(
        actions[0]["exec"],
        "omarchy-bluetooth-device disconnect A1:B2:C3:D4:E5:F6"
    );
    assert_eq!(
        actions[1]["exec"],
        "omarchy-bluetooth-device connect A1:B2:C3:D4:E5:F6"
    );
    assert_eq!(
        actions[2]["exec"],
        "omarchy-bluetooth-device forget A1:B2:C3:D4:E5:F6"
    );
    assert_eq!(
        actions[3]["exec"],
        "printf %s 'A1:B2:C3:D4:E5:F6' | wl-copy"
    );
    assert_eq!(
        actions[4]["exec"],
        "omarchy-shell -q shell summon omarchy.bluetooth"
    );
}

#[test]
fn paired_and_nearby_rows_swap_the_promise() {
    let devices = managed_devices();
    let rows = rows_for(true, &devices, "");
    // Keyboard: paired, still has a live RSSI.
    let kbd = &rows[2];
    assert_eq!(kbd["id"], "C3:D4:E5:F6:A7:B8");
    assert_eq!(kbd["subtitle"], "Paired");
    assert_eq!(
        kbd["exec"],
        "omarchy-bluetooth-device connect C3:D4:E5:F6:A7:B8"
    );
    assert_eq!(kbd["score"], 91900);
    assert_eq!(kbd["signal"], 80);
    assert_eq!(kbd["signalLabel"], "-60 dBm");
    assert_eq!(kbd["deviceKind"], "keyboard");
    assert!(kbd.get("battery").is_none());
    assert_eq!(kbd["actions"][1]["title"], "Disconnect");
    // Mouse: paired, never heard from — no meter, no cell, not zeros.
    let mouse = &rows[3];
    assert_eq!(mouse["id"], "B2:C3:D4:E5:F6:A7");
    assert_eq!(mouse["score"], 91800);
    assert_eq!(mouse["mark"], "paired");
    assert_eq!(mouse["joined"], false);
    assert_eq!(mouse["known"], true);
    assert_eq!(mouse["deviceKind"], "mouse");
    assert!(mouse.get("signal").is_none());
    assert!(mouse.get("signalLabel").is_none());
    assert!(mouse.get("battery").is_none());
    // Phone: nearby — Enter pairs, a different promise, marked so.
    let phone = &rows[4];
    assert_eq!(phone["id"], "D4:E5:F6:A7:B8:C9");
    assert_eq!(phone["subtitle"], "Nearby");
    assert_eq!(
        phone["exec"],
        "omarchy-bluetooth-device pair D4:E5:F6:A7:B8:C9"
    );
    assert_eq!(phone["score"], 89700);
    assert_eq!(phone["mark"], "nearby");
    assert_eq!(phone["joined"], false);
    assert_eq!(phone["known"], false);
    assert_eq!(phone["signal"], 60);
    let actions = phone["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 4);
    assert_eq!(actions[0]["title"], "Pair");
}

#[test]
fn the_needle_filters_rows_and_the_radio() {
    let devices = managed_devices();
    // "power" hits the radio haystack and no device name.
    let rows = rows_for(true, &devices, "power");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["kind"], "radio");
    // A device name keeps only it; skipped rows do not spend score.
    let rows = rows_for(true, &devices, "master");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "B2:C3:D4:E5:F6:A7");
    assert_eq!(rows[0]["score"], 92000);
    // The address is part of the haystack — a substring, not a prefix:
    // "a7:b8" would name the keyboard too.
    let rows = rows_for(true, &devices, "b8:c9");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "D4:E5:F6:A7:B8:C9");
    // Nothing matching is a genuinely empty answer.
    assert!(rows_for(true, &devices, "zzz").is_empty());
}

#[test]
fn bluetoothctl_fallback_parses_and_sorts_by_name() {
    let (powered, devices) = parse_bluetoothctl(BT_SHOW, BT_CONNECTED, BT_PAIRED);
    assert!(powered);
    // `sort -k3` orders by name: the mouse leads the headset, and the
    // connected headset keeps group 0 underneath a worse emission index.
    assert_eq!(devices.len(), 2);
    assert_eq!(devices[0].address, "B2:C3:D4:E5:F6:A7");
    assert!(!devices[0].connected);
    assert_eq!(devices[1].address, "A1:B2:C3:D4:E5:F6");
    assert!(devices[1].connected);

    let rows = rows_for(true, &devices, "");
    assert_eq!(rows.len(), 3);
    // Fallback rows have no measured fields at all.
    assert!(rows[1].get("signal").is_none());
    assert!(rows[1].get("battery").is_none());
    assert_eq!(rows[1]["deviceKind"], "");
    // Emission order is the name sort; scores still put connected on top.
    assert_eq!(rows[1]["id"], "B2:C3:D4:E5:F6:A7");
    assert_eq!(rows[1]["score"], 92000);
    assert_eq!(rows[2]["id"], "A1:B2:C3:D4:E5:F6");
    assert_eq!(rows[2]["score"], 94900);
    // A `show` that does not say "Powered: yes" reads as off.
    let (powered, _) = parse_bluetoothctl("\tPowered: no\n", "", "");
    assert!(!powered);
}

#[test]
fn device_kinds_are_words() {
    assert_eq!(kind_of("audio-headset"), "headset");
    assert_eq!(kind_of("audio-headphones"), "headphones");
    assert_eq!(kind_of("audio-card"), "speaker");
    assert_eq!(kind_of("audio-speakers"), "speaker");
    assert_eq!(kind_of("input-mouse"), "mouse");
    assert_eq!(kind_of("input-keyboard"), "keyboard");
    assert_eq!(kind_of("input-gaming"), "gamepad");
    assert_eq!(kind_of("camera-photo"), "camera");
    assert_eq!(kind_of("phone"), "phone");
    assert_eq!(kind_of(""), "");
    // The unlisted-icon fallback: strip the family, dashes become words.
    assert_eq!(kind_of("audio-video"), "video");
    assert_eq!(kind_of("input-touchpad"), "touchpad");
    assert_eq!(kind_of("generic-thing"), "generic thing");
}

#[test]
fn signal_is_a_proportion_not_a_number() {
    assert_eq!(signal_fields(None), None);
    assert_eq!(
        signal_fields(Some(-75.0)),
        Some((50, "-75 dBm".to_string()))
    );
    assert_eq!(
        signal_fields(Some(-50.0)),
        Some((100, "-50 dBm".to_string()))
    );
    assert_eq!(
        signal_fields(Some(-100.0)),
        Some((0, "-100 dBm".to_string()))
    );
    assert_eq!(
        signal_fields(Some(-120.0)),
        Some((0, "-120 dBm".to_string()))
    );
    assert_eq!(signal_fields(Some(10.0)), Some((100, "10 dBm".to_string())));
    // A fractional reading fails the script's integer regex — no field.
    assert_eq!(signal_fields(Some(-52.5)), None);
}

#[test]
fn addresses_are_checked() {
    assert!(is_address("A1:B2:C3:D4:E5:F6"));
    assert!(is_address("aa:bb:cc:dd:ee:ff"));
    assert!(!is_address("not-an-address"));
    assert!(!is_address("A1:B2:C3:D4:E5"));
    assert!(!is_address("A1:B2:C3:D4:E5:F6:00"));
    assert!(!is_address("G1:B2:C3:D4:E5:F6"));
    assert!(!is_address(""));
}

#[test]
fn the_matcher_is_the_scripts() {
    assert!(matches("", "anything", "at all"));
    assert!(matches("master", "MX Master 3S", "B2:C3:D4:E5:F6:A7"));
    assert!(matches("a1:b2", "WH-1000XM5", "A1:B2:C3:D4:E5:F6"));
    assert!(!matches("power", "WH-1000XM5", "A1:B2:C3:D4:E5:F6"));
    assert!(matches("power", "bluetooth power adapter on off pair", ""));
}
