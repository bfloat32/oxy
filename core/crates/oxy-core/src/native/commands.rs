//! Omarchy's own routes, as rows — a port of `plugin/Commands.js`.
//!
//! Most are menu routes summoned by name; a few are bar panels. Both look the
//! same from here, and both are scored by the same fuzzy function as apps so
//! the numbers mean the same thing.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::rank;
use crate::row::Row;
use crate::score::{Entry, fuzzy};

struct Cmd {
    id: &'static str,
    title: &'static str,
    subtitle: &'static str,
    glyph: &'static str,
    keywords: &'static [&'static str],
    exec: &'static str,
}

const COMMANDS: &[Cmd] = &[
    Cmd {
        id: "theme",
        title: "Change Theme",
        subtitle: "Appearance",
        glyph: "",
        keywords: &["theme", "colour", "color", "appearance", "dark", "light"],
        exec: "omarchy menu summon style.theme",
    },
    Cmd {
        id: "background",
        title: "Change Background",
        subtitle: "Appearance",
        glyph: "",
        keywords: &["wallpaper", "background", "desktop", "picture"],
        exec: "omarchy menu summon style.background",
    },
    Cmd {
        id: "font",
        title: "Change Font",
        subtitle: "Appearance",
        glyph: "",
        keywords: &["font", "typeface", "text"],
        exec: "omarchy menu summon style.font",
    },
    Cmd {
        id: "bar",
        title: "Bar Settings",
        subtitle: "Appearance",
        glyph: "",
        keywords: &["bar", "status", "panel", "top"],
        exec: "omarchy menu summon style.bar",
    },
    Cmd {
        id: "wifi",
        title: "Wi-Fi",
        subtitle: "Network",
        glyph: "",
        keywords: &["wifi", "wireless", "network", "internet"],
        exec: "omarchy-shell shell summon omarchy.network",
    },
    Cmd {
        id: "bluetooth",
        title: "Bluetooth",
        subtitle: "Network",
        glyph: "",
        keywords: &["bluetooth", "pair", "headphones", "device"],
        exec: "omarchy-shell shell summon omarchy.bluetooth",
    },
    Cmd {
        id: "audio",
        title: "Audio",
        subtitle: "Hardware",
        glyph: "",
        keywords: &["audio", "sound", "volume", "output", "microphone"],
        exec: "omarchy-shell shell summon omarchy.audio",
    },
    Cmd {
        id: "display",
        title: "Displays",
        subtitle: "Hardware",
        glyph: "",
        keywords: &["display", "monitor", "screen", "resolution", "brightness"],
        exec: "omarchy-shell shell summon omarchy.monitor",
    },
    Cmd {
        id: "keybindings",
        title: "Keybindings",
        subtitle: "Help",
        glyph: "",
        keywords: &["keys", "keybindings", "shortcuts", "bindings", "help"],
        exec: "omarchy-menu-keybindings",
    },
    Cmd {
        id: "monitors",
        title: "Monitor Setup",
        subtitle: "Settings",
        glyph: "",
        keywords: &["monitors", "arrange", "layout", "scaling"],
        exec: "omarchy menu summon setup.monitors",
    },
    Cmd {
        id: "input",
        title: "Input Setup",
        subtitle: "Settings",
        glyph: "",
        keywords: &["input", "keyboard", "mouse", "touchpad", "layout"],
        exec: "omarchy menu summon setup.input",
    },
    Cmd {
        id: "defaults",
        title: "Default Applications",
        subtitle: "Settings",
        glyph: "",
        keywords: &[
            "default", "defaults", "browser", "editor", "terminal", "handler",
        ],
        exec: "omarchy menu summon setup.default",
    },
    Cmd {
        id: "plugins",
        title: "Plugins",
        subtitle: "Settings",
        glyph: "",
        keywords: &["plugins", "widgets", "extensions"],
        exec: "omarchy menu summon setup.plugin",
    },
    Cmd {
        id: "install",
        title: "Install Software",
        subtitle: "Packages",
        glyph: "",
        keywords: &["install", "package", "software", "add", "app"],
        exec: "omarchy menu summon install",
    },
    Cmd {
        id: "remove",
        title: "Remove Software",
        subtitle: "Packages",
        glyph: "",
        keywords: &["remove", "uninstall", "delete", "package"],
        exec: "omarchy menu summon remove",
    },
    Cmd {
        id: "update",
        title: "Update Omarchy",
        subtitle: "System",
        glyph: "",
        keywords: &["update", "upgrade", "refresh", "restart"],
        exec: "omarchy menu summon update",
    },
    Cmd {
        id: "lock",
        title: "Lock Screen",
        subtitle: "Session",
        glyph: "",
        keywords: &["lock", "screen", "away"],
        exec: "omarchy-system-lock",
    },
    Cmd {
        id: "suspend",
        title: "Suspend",
        subtitle: "Session",
        glyph: "",
        keywords: &["suspend", "sleep", "standby"],
        exec: "omarchy menu summon system.suspend",
    },
    Cmd {
        id: "reboot",
        title: "Restart",
        subtitle: "Session",
        glyph: "",
        keywords: &["reboot", "restart"],
        exec: "omarchy menu summon system.reboot",
    },
    Cmd {
        id: "shutdown",
        title: "Shut Down",
        subtitle: "Session",
        glyph: "",
        keywords: &["shutdown", "power", "off", "halt"],
        exec: "omarchy menu summon system.shutdown",
    },
    Cmd {
        id: "logout",
        title: "Log Out",
        subtitle: "Session",
        glyph: "",
        keywords: &["logout", "log out", "sign out", "exit"],
        exec: "omarchy menu summon system.logout",
    },
    Cmd {
        id: "screenshot",
        title: "Screenshot",
        subtitle: "Capture",
        glyph: "",
        keywords: &["screenshot", "capture", "screen", "grab", "snip"],
        exec: "omarchy-capture-screenshot",
    },
    Cmd {
        id: "clipboard",
        title: "Clipboard History",
        subtitle: "Capture",
        glyph: "",
        keywords: &["clipboard", "history", "paste", "copy"],
        exec: "omarchy-shell shell toggle omarchy.clipboard",
    },
    Cmd {
        id: "emoji",
        title: "Emoji Picker",
        subtitle: "Capture",
        glyph: "",
        keywords: &["emoji", "emojis", "symbol", "smiley"],
        exec: "omarchy-shell shell toggle omarchy.emojis",
    },
];

fn as_entry(c: &Cmd) -> Entry {
    Entry {
        id: format!("cmd.{}", c.id),
        name: c.title.to_string(),
        generic_name: c.subtitle.to_string(),
        comment: String::new(),
        keywords: c.keywords.iter().map(|k| k.to_string()).collect(),
        payload: Value::Null,
    }
}

pub struct Commands;

impl NativeExt for Commands {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        Box::pin(async move {
            let arg = ctx.arg.trim();
            let mut out = Vec::new();
            for cmd in COMMANDS {
                let f = fuzzy(&as_entry(cmd), arg);
                if f < 0 {
                    continue;
                }
                let mut row = Row::new(format!("cmd:{}", cmd.id), "commands");
                row.group = "Commands".into();
                row.title = cmd.title.into();
                row.subtitle = cmd.subtitle.into();
                row.icon_glyph = cmd.glyph.into();
                row.exec = cmd.exec.into();
                // A command beats an app only at equal match quality: the bias
                // moves it inside a tier and can never lift it into a higher
                // one. Typing "the" puts Change Theme above Thunderbird;
                // typing "thun" still puts Thunderbird first, because a name
                // prefix outranks a substring.
                row.tier = rank::tier_for_fuzzy(f);
                row.local = rank::local_for_fuzzy(f);
                row.score = rank::score(row.tier, row.local, 3000);
                out.push(row);
            }
            NativeOutcome::Built(out)
        })
    }
}
