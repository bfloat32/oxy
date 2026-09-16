#!/bin/bash
# Oxy installer.
#
#   curl -fsSL https://raw.githubusercontent.com/bfloat32/oxy/main/install.sh | bash
#
# Installs the launcher on an Omarchy system: clones the repo, links the plugin,
# the helper commands, the extensions and the keybinding into place, and enables
# it. Re-running the same line updates an existing install.
#
# What goes where:
#
#   repo clone        ~/.local/share/oxy            (or beside this script)
#   plugin            ~/.config/omarchy/plugins/bo.oxy      -> repo/plugin
#   commands          ~/.local/bin/oxy-*                    -> repo/bin
#   extensions        ~/.config/omarchy/oxy/extensions/*    -> repo/config/...
#   keybinding        ~/.config/hypr/modules.d/oxy-keys.lua -> repo/hypr/keys.lua
#
# Everything is a symlink into the clone, so `git pull` in ~/.local/share/oxy
# updates the whole thing and nothing drifts out of sync. Anything already at a
# target path that this script did not put there is moved aside with a
# .before-oxy suffix, never deleted.
#
#   ./install.sh --uninstall   takes it all back out (your settings stay)
#   ./install.sh --yes         answer yes to every question, for scripts

set -uo pipefail

REPO_URL="${OXY_REPO:-https://github.com/bfloat32/oxy.git}"
BRANCH="${OXY_BRANCH:-main}"
INSTALL_DIR="${OXY_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/oxy}"
PLUGIN_ID="bo.oxy"

CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
HYPR_DIR="$CONFIG_HOME/hypr"
HYPR_MODULES="$HYPR_DIR/modules.d"
PLUGINS_DIR="$CONFIG_HOME/omarchy/plugins"
BIN_DIR="$HOME/.local/bin"

ASSUME_YES=0
UNINSTALL=0
for arg in "$@"; do
  case "$arg" in
  -y | --yes) ASSUME_YES=1 ;;
  --uninstall) UNINSTALL=1 ;;
  -h | --help)
    sed -n '2,25p' "$0"
    exit 0
    ;;
  *) echo "unknown argument: $arg" >&2; exit 1 ;;
  esac
done

bold() { printf '\033[1m%s\033[0m' "$1"; }
dim() { printf '\033[2m%s\033[0m' "$1"; }
green() { printf '\033[32m%s\033[0m' "$1"; }
yellow() { printf '\033[33m%s\033[0m' "$1"; }
red() { printf '\033[31m%s\033[0m' "$1"; }
cyan() { printf '\033[36m%s\033[0m' "$1"; }

die() {
  printf '\n%s\n  %s\n\n' "$(red 'That did not work')" "$*" >&2
  exit 1
}
step() { printf '\n%s %s\n' "$(cyan '::')" "$(bold "$1")"; }
note() { printf '   %s\n' "$(dim "$1")"; }
ok() { printf '   %s %s\n' "$(green ok)" "$1"; }
warn() { printf '   %s %s\n' "$(yellow '!!')" "$1"; }

# Piping this script through bash leaves stdin on the pipe, so anything that
# reads a key would read the rest of the script instead. /dev/tty is the
# terminal itself, which is still there. A script run without one takes the
# default and never blocks on a question nobody can see.
tty_available() { { : >/dev/tty; } 2>/dev/null; }

ask() {
  local prompt="$1" default="${2:-y}" answer
  tty_available || {
    printf '%s' "$default"
    return 0
  }
  printf '   %s %s ' "$prompt" "$(dim "$([[ $default == y ]] && echo '[Y/n]' || echo '[y/N]')")" >/dev/null
  read -r answer </dev/tty
  answer="${answer:-$default}"
  [[ ${answer,,} == y* ]]
}

# Link src -> dst, moving a stranger aside first. A link we already made (or
# one pointing at the same file) is left alone, so re-running is free.
link_path() {
  local src="$1" dst="$2"
  if [[ -L $dst ]]; then
    [[ $(readlink "$dst") == "$src" ]] && return 0
    rm -f "$dst"
  elif [[ -e $dst ]]; then
    mv "$dst" "$dst.before-oxy" && warn "$dst exists — moved to $dst.before-oxy"
  fi
  mkdir -p "$(dirname "$dst")"
  ln -sfn "$src" "$dst"
}

# The mirror image for --uninstall: only remove links that point into this
# repo, so a file somebody replaced ours with is never taken with it.
unlink_path() {
  local src="$1" dst="$2"
  [[ -L $dst && $(readlink "$dst") == "$src" ]] || return 1
  rm -f "$dst"
}

# ------------------------------------------------------------------ uninstall

if ((UNINSTALL)); then
  printf '\n%s %s\n' "$(cyan '::')" "$(bold 'Removing Oxy')"

  unlink_path "$INSTALL_DIR/plugin" "$PLUGINS_DIR/$PLUGIN_ID" && ok "plugin unlinked"

  for file in "$INSTALL_DIR"/bin/*; do
    unlink_path "$file" "$BIN_DIR/$(basename "$file")" && ok "$(basename "$file") unlinked"
  done

  (cd "$INSTALL_DIR/config" 2>/dev/null && find . -type f -printf '%P\n') | while read -r rel; do
    unlink_path "$INSTALL_DIR/config/$rel" "$CONFIG_HOME/$rel" && ok "config/$rel unlinked"
  done

  for file in "$INSTALL_DIR"/hypr/*.lua; do
    [[ -e $file ]] || continue
    unlink_path "$file" "$HYPR_MODULES/oxy-$(basename "$file")" && ok "hypr/$(basename "$file") unlinked"
  done

  # Deliberately not `omarchy plugin disable`: that deletes the layout entry,
  # taking its position and settings with it, so a later install would not come
  # back where it was. The stale entry renders nothing while the files are
  # gone. What does have to go is the id in plugins[] — that list is what makes
  # the shell load it, and leaving it means loading a plugin with no files.
  local_cfg="$CONFIG_HOME/omarchy/shell.json"
  if [[ -f $local_cfg ]] && command -v python3 >/dev/null &&
    python3 - "$local_cfg" "$PLUGIN_ID" <<'PY' 2>/dev/null
import json, sys
try:
    plugins = json.load(open(sys.argv[1])).get("plugins") or []
except Exception:
    sys.exit(1)
def name(e):
    return e.get("id") if isinstance(e, dict) else e
sys.exit(0 if any(name(e) == sys.argv[2] for e in plugins) else 1)
PY
  then
    cp -p "$local_cfg" "$local_cfg.before-oxy.$(date +%s)"
    BO_FORGET_ID="$PLUGIN_ID" BO_FORGET_CONFIG="$local_cfg" python3 - <<'PY' 2>/dev/null
import json, os
path = os.environ["BO_FORGET_CONFIG"]
wanted = os.environ["BO_FORGET_ID"]
with open(path) as handle:
    config = json.load(handle)
plugins = config.get("plugins")
if not isinstance(plugins, list):
    raise SystemExit(0)
def name(e):
    return e.get("id") if isinstance(e, dict) else e
kept = [e for e in plugins if name(e) != wanted]
if len(kept) == len(plugins):
    raise SystemExit(0)
config["plugins"] = kept
with open(path, "w") as handle:
    json.dump(config, handle, indent=2)
    handle.write("\n")
PY
    ok "removed from the shell's plugin list"
  fi
  # The shell holds shell.json in memory and rewrites the whole file on its own
  # mutations; until its reload arrives, a shell-side write would take this one
  # with it. reloadConfig closes that window.
  command -v omarchy-shell >/dev/null && {
    omarchy-shell shell rescanPlugins >/dev/null 2>&1
    omarchy-shell shell reloadConfig >/dev/null 2>&1
  }
  command -v hyprctl >/dev/null && hyprctl reload >/dev/null 2>&1

  printf '\n%s\n' "$(green 'Oxy is off.')"
  note "Your settings, pins and history were left in place:"
  note "  ~/.config/omarchy/oxy*.json   ~/.local/state/omarchy/oxy-*"
  note "The clone at $INSTALL_DIR is still there — delete it to finish."
  echo
  exit 0
fi

# ------------------------------------------------------------------ preflight

printf '\n%s %s\n' "$(cyan '::')" "$(bold 'Oxy — a launcher for Omarchy')"
note "one box: apps, arithmetic, files, git, music, notes, the web"

step "Checking this is Omarchy"
command -v git >/dev/null || die "git is not installed — pacman -S git"
command -v curl >/dev/null || die "curl is not installed — pacman -S curl"
if command -v omarchy-shell >/dev/null; then
  ok "omarchy-shell found"
else
  warn "omarchy-shell not found — the launcher cannot register without it."
  warn "Continuing anyway: everything links, and it works once Omarchy is here."
fi
command -v hyprctl >/dev/null || warn "hyprctl not found — the keybinding will not load"

# ------------------------------------------------------------- the repo itself

# Running from a checkout (./install.sh) installs that checkout; piped through
# bash there is no checkout, so clone one to a stable home and update on re-run.
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-/dev/null}")" 2>/dev/null && pwd)"
if [[ -f $SELF_DIR/unit.toml && -f $SELF_DIR/plugin/manifest.json ]]; then
  INSTALL_DIR="$SELF_DIR"
  step "Using this checkout"
  ok "$INSTALL_DIR"
else
  step "Getting the source"
  if [[ -d $INSTALL_DIR/.git ]]; then
    git -C "$INSTALL_DIR" fetch --quiet origin ||
      die "could not reach the remote — check your network and try again"
    # Fast-forward only: a clone you edited (the keys.lua preset, say) is not
    # yours to throw away on what looked like an update. Everything still
    # links from whatever the checkout holds.
    if git -C "$INSTALL_DIR" merge --quiet --ff-only "origin/$BRANCH" 2>/dev/null; then
      ok "updated $INSTALL_DIR"
    else
      warn "$INSTALL_DIR has local changes or diverged — left as it is."
      note "git -C $INSTALL_DIR pull --rebase to update it, or reset --hard to start over."
    fi
  elif [[ -e $INSTALL_DIR ]]; then
    die "$INSTALL_DIR exists and is not an Oxy checkout. Move it aside, or set OXY_DIR."
  else
    mkdir -p "$(dirname "$INSTALL_DIR")"
    git clone --quiet --depth 1 --branch "$BRANCH" "$REPO_URL" "$INSTALL_DIR" ||
      die "clone failed — check the URL ($REPO_URL) and your network"
    ok "cloned to $INSTALL_DIR"
  fi
fi

# --------------------------------------------------------------- dependencies

step "Dependencies"
# What the core extensions cannot answer without. busctl comes with systemd and
# the rest of the 'needs' list is already on any Omarchy install.
declare -A PKG_FOR=(
  [qalc]=libqalculate [jq]=jq [curl]=curl [fd]=fd
  [mpv]=mpv [python3]=python [git]=git [wl-copy]=wl-clipboard
)
missing_cmds=() missing_pkgs=()
for cmd in "${!PKG_FOR[@]}"; do
  command -v "$cmd" >/dev/null || { missing_cmds+=("$cmd"); missing_pkgs+=("${PKG_FOR[$cmd]}"); }
done

if ((${#missing_pkgs[@]})); then
  warn "missing: ${missing_cmds[*]}"
  if command -v pacman >/dev/null &&
    { ((ASSUME_YES)) || ask "Install them with pacman?"; }; then
    sudo pacman -S --needed --noconfirm "${missing_pkgs[@]}" ||
      warn "pacman did not finish — install by hand: ${missing_pkgs[*]}"
  else
    note "skipped. The keywords that need them stay hidden until they exist."
  fi
else
  ok "everything the core needs is here"
fi

# Worth having, never required: an extension whose 'when' fails simply does not
# appear, so these are a suggestion rather than an install.
optional=()
for cmd in gh docker playerctl socat notify-send; do
  command -v "$cmd" >/dev/null || optional+=("$cmd")
done
((${#optional[@]})) && note "optional, unlock more keywords: ${optional[*]}"

# -------------------------------------------------------------------- plugin

step "Linking the plugin"
link_path "$INSTALL_DIR/plugin" "$PLUGINS_DIR/$PLUGIN_ID" || die "could not link the plugin"
ok "$PLUGINS_DIR/$PLUGIN_ID -> $INSTALL_DIR/plugin"

# -------------------------------------------------------------------- bin

step "Linking the commands"
count=0
for file in "$INSTALL_DIR"/bin/*; do
  [[ -f $file ]] || continue
  chmod +x "$file" 2>/dev/null
  link_path "$file" "$BIN_DIR/$(basename "$file")" && ((count++))
done
ok "$count commands -> $BIN_DIR"
case :$PATH: in
*":$BIN_DIR:"*) ;;
*)
  warn "$BIN_DIR is not on PATH in this shell."
  note "Omarchy puts it there by default; if this shell disagrees, add it to your profile." ;;
esac

# -------------------------------------------------------------------- config

step "Linking the extension definitions"
# Each file under config/ lands at the same relative path under ~/.config, as
# its own link — the directories stay real, so extensions you write yourself
# sit beside the shipped ones and are never touched.
count=0
while IFS= read -r rel; do
  link_path "$INSTALL_DIR/config/$rel" "$CONFIG_HOME/$rel" && ((count++))
done < <(cd "$INSTALL_DIR/config" && find . -type f -printf '%P\n')
ok "$count files -> $CONFIG_HOME (extensions, snippets defaults)"

# --------------------------------------------------------------- keybinding

step "The keybinding"
# Omarchy reads hyprland.lua; the modules.d loader means adding a module never
# edits a config file again. If better-omarchy already installed its loader it
# does exactly this job — the file is left alone and only the link is ours.
loader="$HYPR_DIR/modules.lua"
main="$HYPR_DIR/hyprland.lua"
if [[ -f $main ]]; then
  if [[ ! -f $loader ]]; then
    cat >"$loader" <<'LUA'
-- Loads every .lua in ~/.config/hypr/modules.d, in name order.
--
-- Two reasons this uses neither require() nor default.hypr.require_all:
--   require() turns every dot in a module name into a path separator, so
--     "hypr.modules.d.foo" would look for hypr/modules/d/foo.lua.
--   require_all runs `find -type f`, which does not match a symlink, and every
--     file in here is a symlink into a checkout.
-- dofile on an absolute path has neither problem, and always re-executes.

local dir = os.getenv("HOME") .. "/.config/hypr/modules.d"
local handle = io.popen("find -L '" .. dir .. "' -maxdepth 1 -name '*.lua' -printf '%f\\n' 2>/dev/null | sort")

if handle then
  for filename in handle:lines() do
    dofile(dir .. "/" .. filename)
  end
  handle:close()
end
LUA
    ok "wrote $loader"
  fi
  grep -q 'require("hypr.modules")' "$main" || {
    printf '\n-- load every linked module from ~/.config/hypr/modules.d\nrequire("hypr.modules")\n' >>"$main"
    ok "added require to hyprland.lua"
  }
  for file in "$INSTALL_DIR"/hypr/*.lua; do
    [[ -e $file ]] || continue
    link_path "$file" "$HYPR_MODULES/oxy-$(basename "$file")"
    ok "hypr/$(basename "$file") -> $HYPR_MODULES/oxy-$(basename "$file")"
  done
  hyprctl reload >/dev/null 2>&1 && note "hyprland reloaded — Super+K is live"
else
  warn "$main not found — link skipped."
  note "Source $INSTALL_DIR/hypr/keys.lua from your Hyprland config by hand."
fi

# ------------------------------------------------------------------ enable

step "Turning it on"
if command -v omarchy-shell >/dev/null; then
  omarchy-shell shell rescanPlugins >/dev/null 2>&1
  sleep 1
  # `plugin enable` appends the plugin to its default section. On one the
  # layout already knows, that would move it and drop its settings, so an
  # existing placement is only reloaded.
  if command -v omarchy >/dev/null; then
    if python3 - "$PLUGIN_ID" <<'PY' 2>/dev/null
import json, os, sys
try:
    layout = json.load(open(os.path.expanduser("~/.config/omarchy/shell.json")))["bar"]["layout"]
except Exception:
    sys.exit(1)
sys.exit(0 if any(w.get("id") == sys.argv[1] for s in layout.values() for w in s) else 1)
PY
    then
      omarchy-shell shell reloadConfig >/dev/null 2>&1
      ok "kept its position in the shell"
    else
      omarchy plugin enable "$PLUGIN_ID" >/dev/null 2>&1 &&
        ok "enabled" ||
        warn "could not enable it — run: omarchy plugin enable $PLUGIN_ID"
    fi
  fi
else
  note "no shell to talk to — it will be picked up when Omarchy starts"
fi

# ------------------------------------------------------------------ omacast

# Upgrading from omacast leaves the old plugin registered beside the new one.
# The launcher itself migrates settings, snippets, extensions and state on its
# first run; what it cannot do is unregister the old plugin id.
for stale in "$PLUGINS_DIR/bo.omacast" "$PLUGINS_DIR/omacast"; do
  if [[ -e $stale || -L $stale ]]; then
    warn "omacast is still installed at $stale"
    note "your data migrates on first run; remove the old plugin with: bo remove omacast"
    note "or by hand: rm $stale && omarchy-shell shell rescanPlugins"
  fi
done

# ------------------------------------------------------------------ done

step "Verify"
problems=0
[[ -f $PLUGINS_DIR/$PLUGIN_ID/Launcher.qml ]] || { warn "plugin/Launcher.qml not reachable through the link"; ((problems++)); }
command -v oxy-search-files >/dev/null || { warn "oxy-* commands not on PATH yet (open a new shell)"; }
[[ -f $CONFIG_HOME/omarchy/oxy/extensions/emoji.json ]] || { warn "extension links missing under $CONFIG_HOME/omarchy/oxy"; ((problems++)); }
# The shell's own validator is the strongest check there is — it is what
# decides whether the plugin loads at all.
if command -v omarchy >/dev/null; then
  if omarchy plugin validate "$INSTALL_DIR/plugin" >/dev/null 2>&1; then
    ok "omarchy plugin validate passed"
  else
    warn "omarchy plugin validate failed:"
    omarchy plugin validate "$INSTALL_DIR/plugin" 2>&1 | sed 's/^/     /'
    ((problems++))
  fi
fi

if ((problems == 0)); then
  printf '\n%s\n' "$(green "$(bold 'Oxy is installed.')")"
else
  printf '\n%s\n' "$(yellow 'Installed with warnings — read the lines above.')"
fi

cat <<EOF

   $(bold 'Press') $(cyan 'Super+K')$(bold '.')
   Type $(cyan '?') to see every keyword your machine actually has.

   Keybind preset:  edit the one line at the top of $INSTALL_DIR/hypr/keys.lua
   Settings:        type $(cyan 'settings:') in the box, or edit ~/.config/omarchy/oxy.json
   Update later:    re-run this same command, or git -C $INSTALL_DIR pull
   Remove:          $INSTALL_DIR/install.sh --uninstall

EOF
