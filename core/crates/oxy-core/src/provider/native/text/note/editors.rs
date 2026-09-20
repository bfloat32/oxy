//! The editor list `note:` offers — the script's `EDITOR_TABLE` plus the
//! resolution that picks which entries a machine actually has.
//!
//! Found, never assumed: an entry is only offered if its program is really
//! installed, because a row promising Typora on a machine without Typora is
//! a row that opens nothing. Omarchy-aware and desktop-aware entries get
//! labelled as the declared choice they are.

use std::path::PathBuf;
use std::time::Duration;

use crate::provider::native::text::note::render::Editor;
use crate::provider::native::util::on_path;
use crate::provider::process::run;
use crate::settings::paths;

/// Each probe gets its own small bound — a native run has no worker
/// deadline.
const CALL: Duration = Duration::from_secs(2);

/// Triples of key, label, command, in the order they are offered. `{}` is
/// where the quoted path lands. Terminal editors go through
/// omarchy-launch-tui (the user's own terminal, Omarchy's styling); GUI
/// ones through `setsid uwsm-app --`, which is what omarchy-launch-editor
/// does for the same set.
///
/// Obsidian is deliberately absent even where it is installed: it opens
/// vaults, not paths.
const EDITOR_TABLE: &[(&str, &str, &str)] = &[
    ("omawrite", "Omawrite", "omawrite {}"),
    (
        "nvim",
        "Neovim",
        "omarchy-launch-tui --app-id=org.omarchy.note nvim {}",
    ),
    (
        "vim",
        "Vim",
        "omarchy-launch-tui --app-id=org.omarchy.note vim {}",
    ),
    (
        "hx",
        "Helix",
        "omarchy-launch-tui --app-id=org.omarchy.note hx {}",
    ),
    (
        "helix",
        "Helix",
        "omarchy-launch-tui --app-id=org.omarchy.note helix {}",
    ),
    (
        "micro",
        "Micro",
        "omarchy-launch-tui --app-id=org.omarchy.note micro {}",
    ),
    (
        "nano",
        "Nano",
        "omarchy-launch-tui --app-id=org.omarchy.note nano {}",
    ),
    (
        "kak",
        "Kakoune",
        "omarchy-launch-tui --app-id=org.omarchy.note kak {}",
    ),
    ("emacs", "Emacs", "setsid uwsm-app -- emacs {}"),
    ("zeditor", "Zed", "setsid uwsm-app -- zeditor {}"),
    ("code", "VS Code", "setsid uwsm-app -- code {}"),
    ("codium", "VSCodium", "setsid uwsm-app -- codium {}"),
    ("cursor", "Cursor", "setsid uwsm-app -- cursor {}"),
    (
        "sublime_text",
        "Sublime Text",
        "setsid uwsm-app -- sublime_text {}",
    ),
    ("typora", "Typora", "setsid uwsm-app -- typora {}"),
    ("marktext", "Mark Text", "setsid uwsm-app -- marktext {}"),
    (
        "apostrophe",
        "Apostrophe",
        "setsid uwsm-app -- apostrophe {}",
    ),
    (
        "ghostwriter",
        "Ghostwriter",
        "setsid uwsm-app -- ghostwriter {}",
    ),
    (
        "gnome-text-editor",
        "Text Editor",
        "setsid uwsm-app -- gnome-text-editor {}",
    ),
    ("gedit", "gedit", "setsid uwsm-app -- gedit {}"),
    ("kate", "Kate", "setsid uwsm-app -- kate {}"),
    ("mousepad", "Mousepad", "setsid uwsm-app -- mousepad {}"),
    ("xed", "Text Editor", "setsid uwsm-app -- xed {}"),
    ("pluma", "Pluma", "setsid uwsm-app -- pluma {}"),
];

/// What Omarchy was told to open a note with —
/// `$HOME/.local/state/omarchy/defaults/editor` is the file
/// omarchy-launch-editor itself reads, so this reads the same file rather
/// than forking omarchy-default-editor to be told the same string. `zed` is
/// written both ways; the table is keyed by the program name.
pub fn omarchy_editor() -> String {
    let state = paths::home().join(".local/state/omarchy/defaults/editor");
    let first = std::fs::read_to_string(state)
        .ok()
        .and_then(|t| t.lines().next().map(|l| l.trim().to_string()))
        .unwrap_or_default();
    let editor = if first.is_empty() {
        "nvim".to_string()
    } else {
        first
    };
    if editor == "zed" {
        "zeditor".to_string()
    } else {
        editor
    }
}

/// The desktop's own answer for a `.md`, resolved from a desktop file back
/// to the program it runs — "dev.zed.Zed.desktop" is not something the
/// table can be keyed by. `xdg-settings` is not consulted: it only knows
/// the browser and URL handlers and has no opinion about text files.
pub async fn mime_editor() -> String {
    let mut desktop = String::new();
    for mime in ["text/markdown", "text/plain"] {
        if let Some(fin) = run(&format!("xdg-mime query default {mime}"), CALL).await {
            let d = fin.stdout.trim();
            if !d.is_empty() {
                desktop = d.to_string();
                break;
            }
        }
    }
    if desktop.is_empty() {
        return String::new();
    }

    for dir in [
        paths::data_home().join("applications"),
        PathBuf::from("/usr/local/share/applications"),
        PathBuf::from("/usr/share/applications"),
    ] {
        let app = dir.join(&desktop);
        if !app.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&app).unwrap_or_default();
        let exec = text
            .lines()
            .find(|l| l.starts_with("Exec="))
            .map(|l| &l["Exec=".len()..])
            .unwrap_or("");
        // The first word of Exec is the program; the rest is %U and
        // friends. A flatpak or snap wrapper resolves to "flatpak", which
        // is not an editor and is left unmatched rather than guessed at.
        let program = exec.split(' ').next().unwrap_or("");
        return program.rsplit('/').next().unwrap_or("").to_string();
    }
    String::new()
}

/// The script's `build_editors`: Omawrite first when it is here (Enter has
/// always meant Omawrite), then the Omarchy default, then the desktop's
/// `.md` handler, then everything else that is installed — each label once,
/// so a machine with both `hx` and `helix` does not offer Helix twice.
pub fn build(omarchy: &str, mime: &str, have: impl Fn(&str) -> bool) -> Vec<Editor> {
    let mut editors: Vec<Editor> = Vec::new();
    let mut labels: Vec<&'static str> = Vec::new();
    let mut add = |key: &str, note: &str| {
        let Some(&(_, label, cmd)) = EDITOR_TABLE.iter().find(|e| e.0 == key) else {
            return;
        };
        if labels.contains(&label) || !have(key) {
            return;
        }
        labels.push(label);
        editors.push((
            key.to_string(),
            if note.is_empty() {
                label.to_string()
            } else {
                format!("{label} ({note})")
            },
            cmd.to_string(),
        ));
    };
    add("omawrite", "");
    add(omarchy, "Omarchy default");
    if !mime.is_empty() {
        add(mime, "system default");
    }
    for &(key, _, _) in EDITOR_TABLE {
        add(key, "");
    }
    editors
}

/// The editors actually available — `command -v` on each key as a PATH
/// walk.
pub fn available(omarchy: &str, mime: &str) -> Vec<Editor> {
    build(omarchy, mime, on_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omawrite_leads_when_installed() {
        let eds = build("nvim", "", |k| k == "omawrite" || k == "nvim");
        assert_eq!(eds[0].1, "Omawrite");
        assert_eq!(eds[1].1, "Neovim (Omarchy default)");
        // nvim in the table pass was already claimed by the default slot.
        assert_eq!(eds.len(), 2);
    }

    #[test]
    fn one_label_is_offered_once() {
        // hx and helix are both "Helix"; whichever resolution lands first
        // owns it.
        let eds = build("hx", "", |k| k == "hx" || k == "helix");
        let helixes = eds.iter().filter(|e| e.1.starts_with("Helix")).count();
        assert_eq!(helixes, 1);
        assert_eq!(eds[0].1, "Helix (Omarchy default)");
    }

    #[test]
    fn the_mime_default_is_marked_and_resolved_first_after_omarchy() {
        let eds = build("nvim", "typora", |k| {
            matches!(k, "nvim" | "typora" | "code")
        });
        assert_eq!(eds[0].1, "Neovim (Omarchy default)");
        assert_eq!(eds[1].1, "Typora (system default)");
        assert_eq!(eds[2].1, "VS Code");
    }

    #[test]
    fn an_unknown_key_or_missing_program_is_not_offered() {
        let eds = build("obsidian", "flatpak", |_| false);
        assert!(eds.is_empty());
        // "zed" has already been translated to "zeditor" by the caller;
        // a name the table does not know adds nothing.
        let eds = build("not-an-editor", "", |k| k == "nano");
        assert_eq!(eds.len(), 1);
        assert_eq!(eds[0].1, "Nano");
    }
}
