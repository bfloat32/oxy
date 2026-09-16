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
#   plugin            ~/.config/omarchy/plugins/oma.oxy     -> repo/plugin
#   commands          ~/.local/bin/oxy-*                    -> repo/bin
#   extensions        ~/.config/omarchy/oxy/extensions/*    -> repo/config/...
#   keybinding        ~/.config/hypr/modules.d/oxy-keys.lua -> repo/hypr/keys.lua
#
# Everything is a symlink into the clone, so `git pull` in ~/.local/share/oxy
# updates the whole thing and nothing drifts out of sync. Anything already at a
# target path that this script did not put there is moved aside with a
# .before-oxy suffix, never deleted.
#
#   ./install.sh --uninstall   takes the links out (your settings stay)
#   ./install.sh --purge       uninstall and delete the clone too
#   ./install.sh --fresh       delete the checkout and install clean
#   ./install.sh --yes         answer yes to every question, for scripts

set -uo pipefail

REPO_URL="${OXY_REPO:-https://github.com/bfloat32/oxy.git}"
BRANCH="${OXY_BRANCH:-main}"
INSTALL_DIR="${OXY_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/oxy}"
PLUGIN_ID="oma.oxy"
# What the plugin answered to before the rename. An existing install keeps
# its layout place if the registration is renamed rather than re-added.
OLD_PLUGIN_ID="bo.oxy"

CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
HYPR_DIR="$CONFIG_HOME/hypr"
HYPR_MODULES="$HYPR_DIR/modules.d"
PLUGINS_DIR="$CONFIG_HOME/omarchy/plugins"
BIN_DIR="$HOME/.local/bin"

# Where this script itself lives — used to tell "install this checkout" apart
# from "clone one", and to make sure no delete path ever touches it.
SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-/dev/null}")" 2>/dev/null && pwd -P)"

ASSUME_YES=0
UNINSTALL=0
PURGE=0
FRESH=0
for arg in "$@"; do
  case "$arg" in
  -y | --yes) ASSUME_YES=1 ;;
  --uninstall) UNINSTALL=1 ;;
  --purge) UNINSTALL=1; PURGE=1 ;;
  --fresh | --reinstall) FRESH=1 ;;
  -h | --help)
    sed -n '2,27p' "$0"
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
  # Any tty failure — no terminal, or one that opens but will not read — takes
  # the default. The default is the answer, not just the display: a yes-defaulted
  # question passes, a no-defaulted one fails, so a destructive prompt can never
  # silently approve itself on a machine with no terminal.
  if ! { tty_available &&
    printf '   %s %s ' "$prompt" "$(dim "$([[ $default == y ]] && echo '[Y/n]' || echo '[y/N]')")" >/dev/tty &&
    read -r answer </dev/tty; } 2>/dev/null; then
    printf '%s' "$default"
    return "$([[ $default == y ]] && echo 0 || echo 1)"
  fi
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
    if mv "$dst" "$dst.before-oxy" 2>/dev/null; then
      warn "$dst exists — moved to $dst.before-oxy"
    else
      warn "$dst exists and could not be moved aside — link skipped"
      return 1
    fi
  fi
  mkdir -p "$(dirname "$dst")"
  ln -sfn "$src" "$dst" 2>/dev/null || { warn "could not link $dst"; return 1; }
}

# The mirror image for --uninstall: only remove links that point into this
# repo, so a file somebody replaced ours with is never taken with it.
unlink_path() {
  local src="$1" dst="$2"
  [[ -L $dst && $(readlink "$dst") == "$src" ]] || return 1
  rm -f "$dst"
}

trap 'printf "\n   %s\n\n" "$(yellow "interrupted — re-run the same command to pick up where it left off")"; exit 130' INT TERM

# Whether a directory is plausibly ours to delete. Refusing anything else means
# --fresh/--purge can never eat a directory a stranger pointed OXY_DIR at.
checkout_looks_ours() {
  [[ -d $1/.git || -f $1/unit.toml || -f $1/plugin/manifest.json ]] &&
    return 0
  # an empty directory is what a crashed clone leaves behind — safe to reuse
  [[ -d $1 && -z $(ls -A "$1" 2>/dev/null) ]]
}

# rm -rf with the guards that make it safe to offer interactively.
wipe_checkout() {
  local dir="$1" real
  [[ -n $dir && $dir != / && $dir != "$HOME" ]] || return 1
  # Never delete the repository this very script was run from — that is the
  # user's own working copy, not a disposable clone.
  real=$(cd "$dir" 2>/dev/null && pwd -P) || return 1
  [[ -n $SELF_DIR && $real == "$SELF_DIR" ]] && return 1
  checkout_looks_ours "$dir" || return 1
  rm -rf "$dir"
}

# Same remote, spelled the same way. URLs keep their form; a local path is
# canonicalized so C:/x and /c/x do not read as different remotes.
norm_url() {
  local u="${1%.git}"; u="${u%/}"
  if [[ $u == *://* || $u == *@*:* ]]; then
    printf '%s' "$u"
  else
    (cd "$u" 2>/dev/null && pwd -P) || printf '%s' "$u"
  fi
}

# Clone into a sibling temp dir, then move into place: a failed clone leaves
# $tmp to be cleaned instead of a half-repo at $INSTALL_DIR that every later
# run would trip over.
fresh_clone() {
  local tmp="$INSTALL_DIR.tmp.$$"
  # A clone killed mid-write leaves a sibling husk behind; ours are named so
  # we can sweep them without touching anything else.
  for stale in "$INSTALL_DIR".tmp.*; do
    [[ -e $stale ]] && rm -rf "$stale"
  done
  rm -rf "$tmp"
  if ! git clone --quiet --depth 1 --branch "$BRANCH" "$REPO_URL" "$tmp"; then
    rm -rf "$tmp"
    die "clone failed — check the URL ($REPO_URL) and your network"
  fi
  mv "$tmp" "$INSTALL_DIR" || {
    rm -rf "$tmp"
    die "could not move the new clone into place at $INSTALL_DIR"
  }
}

# ------------------------------------------------------------------ uninstall

if ((UNINSTALL)); then
  printf '\n%s %s\n' "$(cyan '::')" "$(bold 'Removing Oxy')"

  # Links may point at the piped clone or at this checkout — cover both
  # spellings so an uninstall run from the repo still finds links a cloned
  # install made, and vice versa.
  sources=("$INSTALL_DIR")
  if [[ -f $SELF_DIR/unit.toml && -f $SELF_DIR/plugin/manifest.json && $SELF_DIR != "$INSTALL_DIR" ]]; then
    sources+=("$SELF_DIR")
  fi

  for src_dir in "${sources[@]}"; do
    unlink_path "$src_dir/plugin" "$PLUGINS_DIR/$PLUGIN_ID" && ok "plugin unlinked"

    for file in "$src_dir"/bin/*; do
      unlink_path "$file" "$BIN_DIR/$(basename "$file")" && ok "$(basename "$file") unlinked"
    done

    (cd "$src_dir/config" 2>/dev/null && find . -type f -printf '%P\n') | while read -r rel; do
      unlink_path "$src_dir/config/$rel" "$CONFIG_HOME/$rel" && ok "config/$rel unlinked"
    done

    for file in "$src_dir"/hypr/*.lua; do
      [[ -e $file ]] || continue
      unlink_path "$file" "$HYPR_MODULES/oxy-$(basename "$file")" && ok "hypr/$(basename "$file") unlinked"
    done
  done

  # The walks above ask each source what it owns, which finds nothing once the
  # clone itself is gone — and the links it made are exactly what remains.
  # The sweep removes any link pointing into either spelling whether the
  # checkout is there or not, so a deleted clone still uninstalls clean.
  stray=0
  while IFS= read -r link; do
    rm -f "$link" && ((++stray))
  done < <(find "$PLUGINS_DIR" "$BIN_DIR" "$HYPR_MODULES" "$CONFIG_HOME/omarchy" \
    -type l \( -lname "$INSTALL_DIR/*" -o -lname "$SELF_DIR/*" \) 2>/dev/null)
  ((stray > 0)) && ok "$stray stale links removed"

  # Deliberately not `omarchy plugin disable`: that deletes the layout entry,
  # taking its position and settings with it, so a later install would not come
  # back where it was. The stale entry renders nothing while the files are
  # gone. What does have to go is the id in plugins[] — that list is what makes
  # the shell load it, and leaving it means loading a plugin with no files.
  local_cfg="$CONFIG_HOME/omarchy/shell.json"
  cfg_state=9
  if [[ -f $local_cfg ]] && command -v python3 >/dev/null; then
    python3 - "$local_cfg" "$PLUGIN_ID" "$OLD_PLUGIN_ID" <<'PY' 2>/dev/null
import json, sys
try:
    plugins = json.load(open(sys.argv[1])).get("plugins") or []
except Exception:
    sys.exit(2)
def name(e):
    return e.get("id") if isinstance(e, dict) else e
sys.exit(0 if any(name(e) in (sys.argv[2], sys.argv[3]) for e in plugins) else 1)
PY
    cfg_state=$?
  fi
  if ((cfg_state == 2)); then
    warn "could not read $local_cfg — $PLUGIN_ID may still be in its plugin list"
  elif ((cfg_state == 0)); then
    cp -p "$local_cfg" "$local_cfg.before-oxy.$(date +%s)"
    OXY_FORGET_ID="$PLUGIN_ID $OLD_PLUGIN_ID" OXY_FORGET_CONFIG="$local_cfg" python3 - <<'PY' 2>/dev/null
import json, os
path = os.environ["OXY_FORGET_CONFIG"]
wanted = os.environ["OXY_FORGET_ID"].split()
with open(path) as handle:
    config = json.load(handle)
plugins = config.get("plugins")
if not isinstance(plugins, list):
    raise SystemExit(0)
def name(e):
    return e.get("id") if isinstance(e, dict) else e
kept = [e for e in plugins if name(e) not in wanted]
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

  # Deleting the clone is a separate question from unlinking: it is where your
  # keybind preset edit lives, so --purge asks the filesystem, not a flag alone.
  # Never offered when the checkout is this script's own repo — that one is the
  # user's working copy, not a disposable clone.
  self_real=$(cd "$SELF_DIR" 2>/dev/null && pwd -P)
  inst_real=$(cd "$INSTALL_DIR" 2>/dev/null && pwd -P)
  if [[ -d $INSTALL_DIR && ( -z $inst_real || $inst_real != "$self_real" ) ]]; then
    if ((PURGE)) || ask "Also delete the clone at $INSTALL_DIR?" n; then
      if wipe_checkout "$INSTALL_DIR"; then
        ok "deleted $INSTALL_DIR"
      else
        warn "kept $INSTALL_DIR — it does not look like a disposable clone"
        note "Delete it by hand if you really mean it."
      fi
    else
      note "clone kept at $INSTALL_DIR"
    fi
  elif [[ -d $INSTALL_DIR ]]; then
    note "the checkout is this script's own repo — left alone"
  fi

  # Report anything the install ever set aside, so a clean removal actually
  # tells you what remains rather than leaving you to discover it.
  leftovers=$(find "$CONFIG_HOME" "$BIN_DIR" "$HYPR_MODULES" \
    -name '*.before-oxy*' 2>/dev/null)
  [[ -n $leftovers ]] && {
    note "Files set aside along the way — restore or delete them by hand:"
    printf '%s\n' "$leftovers" | sed 's/^/      /'
  }
  for old in "$INSTALL_DIR".broken.*; do
    [[ -e $old ]] && note "old checkout kept at $old"
  done

  note "Your settings, pins and history were left in place:"
  note "  ~/.config/omarchy/oxy*.json   ~/.local/state/omarchy/oxy-*"
  echo
  exit 0
fi

# ------------------------------------------------------------------ install

# The whole install runs in a subshell so a failure inside it can be answered
# with a clean-slate retry by the driver below. Exit codes: 0 installed, 100
# installed but the verify step found problems, 1 hard stop, 130 interrupted.
install_body() {
  printf '\n%s %s\n' "$(cyan '::')" "$(bold 'Oxy — a launcher for Omarchy')"
  note "one box: apps, arithmetic, files, git, music, notes, the web"

  # ------------------------------------------------------------- preflight

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

  # Everywhere the links land must be writable before any work starts — finding
  # out halfway through leaves a half-linked install to reason about.
  mkdir -p "$PLUGINS_DIR" "$BIN_DIR" "$CONFIG_HOME/omarchy" 2>/dev/null
  for dir in "$PLUGINS_DIR" "$BIN_DIR" "$CONFIG_HOME/omarchy"; do
    [[ -w $dir ]] || die "cannot write to $dir — check its permissions"
  done

  # -------------------------------------------------------- the repo itself

  if [[ -f $SELF_DIR/unit.toml && -f $SELF_DIR/plugin/manifest.json ]]; then
    INSTALL_DIR="$SELF_DIR"
    step "Using this checkout"
    ok "$INSTALL_DIR"
  else
    step "Getting the source"
    if [[ -d $INSTALL_DIR/.git ]]; then
      # A checkout that points somewhere else is somebody else's — never
      # fetch into it or move it. Local paths get canonicalized first: git
      # may record a different spelling of the same directory.
      origin=$(norm_url "$(git -C "$INSTALL_DIR" remote get-url origin 2>/dev/null)")
      if [[ -n $origin && $origin != "$(norm_url "$REPO_URL")" ]]; then
        die "$INSTALL_DIR is a clone of $(git -C "$INSTALL_DIR" remote get-url origin), not $REPO_URL — set OXY_DIR or OXY_REPO"
      fi
      # The status is the test, not the output: a fetch that pulls new commits
      # prints its progress on stderr even when everything went fine.
      if ! fetch_err=$(git -C "$INSTALL_DIR" fetch origin 2>&1); then
        # A fetch that fails naming an object or a pack is not a network
        # problem and retrying is not a fix: an interrupted clone leaves
        # empty files in .git/objects that nothing will rewrite. Move the
        # checkout aside — never delete it — and clone again. Links point
        # at the same path, so the moment the new clone lands everything
        # resolves again.
        if [[ $fetch_err == *object* || $fetch_err == *pack* || $fetch_err == *corrupt* ]]; then
          broken_dir="$INSTALL_DIR.broken.$(date +%s)"
          warn "the checkout at $INSTALL_DIR is corrupt:"
          printf '%s\n' "$fetch_err" | head -3 | sed 's/^/      /'
          mv "$INSTALL_DIR" "$broken_dir" ||
            die "could not move $INSTALL_DIR aside — do it by hand and re-run"
          note "moved to $broken_dir — delete it once you know you do not need it"
          fresh_clone
          ok "fresh clone at $INSTALL_DIR"
        else
          die "could not reach the remote — $fetch_err"
        fi
      # A branch that is not on the remote fails the pull with git's own
      # wording; naming it here says what to fix instead.
      elif ! git -C "$INSTALL_DIR" rev-parse --verify --quiet "origin/$BRANCH" >/dev/null; then
        die "branch $BRANCH is not on $REPO_URL — check OXY_BRANCH"
      # Rebase with autostash: editing keys.lua in place is the supported way
      # to set the keybind, so a dirty tree is the expected state on every
      # update, not an error — stash, fast-forward, put the edits back. A
      # genuine conflict fails the pull and leaves the stash to pop by hand.
      elif git -C "$INSTALL_DIR" pull --quiet --rebase --autostash origin "$BRANCH" 2>/dev/null; then
        ok "updated $INSTALL_DIR"
      else
        warn "$INSTALL_DIR has changes that conflict with the update — left as it is."
        note "update it by hand:  git -C $INSTALL_DIR stash pop; git -C $INSTALL_DIR pull --rebase"
        note "or start clean:     $INSTALL_DIR/install.sh --fresh"
      fi
    elif [[ -e $INSTALL_DIR ]]; then
      if checkout_looks_ours "$INSTALL_DIR"; then
        # A checkout whose .git vanished, or the empty husk of a crashed
        # clone: same fix as a corrupt one.
        broken_dir="$INSTALL_DIR.broken.$(date +%s)"
        warn "$INSTALL_DIR exists but is not a working clone — moving it aside"
        mv "$INSTALL_DIR" "$broken_dir" ||
          die "could not move $INSTALL_DIR aside — do it by hand and re-run"
        fresh_clone
        ok "fresh clone at $INSTALL_DIR"
      else
        die "$INSTALL_DIR exists and is not an Oxy checkout. Move it aside, or set OXY_DIR."
      fi
    else
      mkdir -p "$(dirname "$INSTALL_DIR")"
      fresh_clone
      ok "cloned to $INSTALL_DIR"
    fi
  fi

  # A checkout that git calls healthy but is missing pieces is just as
  # broken — the driver will offer to start over.
  if [[ ! -f $INSTALL_DIR/plugin/manifest.json || ! -f $INSTALL_DIR/install.sh ||
    ! -d $INSTALL_DIR/bin || ! -d $INSTALL_DIR/config || ! -d $INSTALL_DIR/plugin ]]; then
    die "the checkout at $INSTALL_DIR is missing files — it cannot be installed from"
  fi

  # ------------------------------------------------------------ dependencies

  step "Dependencies"
  # What the core extensions cannot answer without. busctl comes with systemd
  # and the rest of the 'needs' list is already on any Omarchy install.
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

  # Worth having, never required: an extension whose 'when' fails simply does
  # not appear, so these are a suggestion rather than an install.
  optional=()
  for cmd in gh docker playerctl socat notify-send; do
    command -v "$cmd" >/dev/null || optional+=("$cmd")
  done
  ((${#optional[@]})) && note "optional, unlock more keywords: ${optional[*]}"

  # ------------------------------------------------------------------ plugin

  step "Linking the plugin"
  link_path "$INSTALL_DIR/plugin" "$PLUGINS_DIR/$PLUGIN_ID" || die "could not link the plugin"
  ok "$PLUGINS_DIR/$PLUGIN_ID -> $INSTALL_DIR/plugin"

  # --------------------------------------------------------------------- bin

  step "Linking the commands"
  count=0
  for file in "$INSTALL_DIR"/bin/*; do
    [[ -f $file ]] || continue
    chmod +x "$file" 2>/dev/null
    link_path "$file" "$BIN_DIR/$(basename "$file")" && ((++count))
  done
  ok "$count commands -> $BIN_DIR"
  case :$PATH: in
  *":$BIN_DIR:"*) ;;
  *)
    warn "$BIN_DIR is not on PATH in this shell."
    note "Omarchy puts it there by default; if this shell disagrees, add it to your profile." ;;
  esac

  # ------------------------------------------------------------------ config

  step "Linking the extension definitions"
  # Each file under config/ lands at the same relative path under ~/.config,
  # as its own link — the directories stay real, so extensions you write
  # yourself sit beside the shipped ones and are never touched.
  count=0
  while IFS= read -r rel; do
    link_path "$INSTALL_DIR/config/$rel" "$CONFIG_HOME/$rel" && ((++count))
  done < <(cd "$INSTALL_DIR/config" && find . -type f -printf '%P\n')
  ok "$count files -> $CONFIG_HOME (extensions, snippets defaults)"

  # ------------------------------------------------------------- keybinding

  step "The keybinding"
  # Omarchy reads hyprland.lua; the modules.d loader means adding a module
  # never edits a config file again. If better-omarchy already installed its
  # loader it does exactly this job — the file is left alone and only the
  # link is ours.
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
      if printf '\n-- load every linked module from ~/.config/hypr/modules.d\nrequire("hypr.modules")\n' >>"$main"; then
        ok "added require to hyprland.lua"
      else
        warn "could not write to $main — add require(\"hypr.modules\") by hand"
      fi
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

  # ---------------------------------------------------------------- enable

  step "Turning it on"
  command -v omarchy-shell >/dev/null && {
    omarchy-shell shell rescanPlugins >/dev/null 2>&1
    sleep 1
  }

  # Adopt the old registration: the plugin used to answer to bo.oxy, and
  # renaming its entries keeps its place in the layout where a fresh enable
  # would land it at the default spot.
  old_link="$PLUGINS_DIR/$OLD_PLUGIN_ID"
  # Remove the old id's link when it is ours (points into this checkout) or
  # dead (points at a location that no longer exists). A real file stays.
  if [[ -L $old_link ]] &&
    { [[ $(readlink "$old_link") == "$INSTALL_DIR"/* ]] || [[ ! -e $old_link ]]; }; then
    rm -f "$old_link" && ok "removed the old $OLD_PLUGIN_ID link"
  fi
  local_cfg="$CONFIG_HOME/omarchy/shell.json"
  if [[ -f $local_cfg ]] && grep -q "$OLD_PLUGIN_ID" "$local_cfg" &&
    command -v python3 >/dev/null; then
    cp -p "$local_cfg" "$local_cfg.before-oxy.$(date +%s)"
    OXY_OLD_ID="$OLD_PLUGIN_ID" OXY_NEW_ID="$PLUGIN_ID" OXY_CFG="$local_cfg" \
      python3 - <<'PY' 2>/dev/null &&
import json, os
path, old, new = os.environ["OXY_CFG"], os.environ["OXY_OLD_ID"], os.environ["OXY_NEW_ID"]
try:
    config = json.load(open(path))
except Exception:
    raise SystemExit(1)
changed = False
def fix(e):
    global changed
    if isinstance(e, dict):
        if e.get("id") == old:
            e["id"] = new
            changed = True
    elif e == old:
        changed = True
        return new
    return e
def name(e):
    return e.get("id") if isinstance(e, dict) else e
def dedupe(items):
    global changed
    # The old id may coexist with the new one if the new build was enabled
    # once already; the first occurrence keeps the position.
    seen, out = set(), []
    for e in items:
        if name(e) == new and new in seen:
            changed = True
            continue
        seen.add(name(e))
        out.append(e)
    return out
if isinstance(config.get("plugins"), list):
    config["plugins"] = dedupe([fix(e) for e in config["plugins"]])
layout = (config.get("bar") or {}).get("layout")
if isinstance(layout, dict):
    for section_name, section in layout.items():
        if isinstance(section, list):
            layout[section_name] = dedupe([fix(w) for w in section])
if not changed:
    raise SystemExit(1)
with open(path, "w") as handle:
    json.dump(config, handle, indent=2)
    handle.write("\n")
PY
      ok "the old $OLD_PLUGIN_ID registration now answers to $PLUGIN_ID"
  fi

  # `plugin enable` writes shell.json — the running shell needs reloadConfig to
  # see it, and a not-yet-running one picks it up on next start. So enable only
  # needs `omarchy`; the live reload needs `omarchy-shell`.
  if command -v omarchy >/dev/null; then
    if OXY_CFG_PATH="$local_cfg" python3 - "$PLUGIN_ID" <<'PY' 2>/dev/null
import json, os, sys
# Skipping `plugin enable` is only safe when both halves are already right:
# the layout keeps its place, and plugins[] is what makes the shell load it.
# A layout entry without the list entry renders nothing.
try:
    config = json.load(open(os.environ["OXY_CFG_PATH"]))
except Exception:
    sys.exit(1)
layout = (config.get("bar") or {}).get("layout") or {}
plugins = config.get("plugins") or []
def name(e):
    return e.get("id") if isinstance(e, dict) else e
in_layout = any(w.get("id") == sys.argv[1] for s in layout.values() for w in s)
in_plugins = any(name(e) == sys.argv[1] for e in plugins)
sys.exit(0 if (in_layout and in_plugins) else 1)
PY
    then
      omarchy-shell shell reloadConfig >/dev/null 2>&1
      ok "kept its position in the shell"
    else
      omarchy plugin enable "$PLUGIN_ID" >/dev/null 2>&1 &&
        ok "enabled" ||
        warn "could not enable it — run: omarchy plugin enable $PLUGIN_ID"
    fi
  else
    note "no shell to talk to — it will be picked up when Omarchy starts"
  fi

  # ------------------------------------------------------------------ done

  step "Verify"
  problems=0
  [[ -f $PLUGINS_DIR/$PLUGIN_ID/Launcher.qml ]] ||
    { warn "plugin/Launcher.qml not reachable through the link"; ((++problems)); }
  command -v oxy-search-files >/dev/null ||
    warn "oxy-* commands not on PATH yet (open a new shell)"
  [[ -f $CONFIG_HOME/omarchy/oxy/extensions/emoji.json ]] ||
    { warn "extension links missing under $CONFIG_HOME/omarchy/oxy"; ((++problems)); }
  # The shell's own validator is the strongest check there is — it is what
  # decides whether the plugin loads at all.
  if command -v omarchy >/dev/null; then
    if omarchy plugin validate "$INSTALL_DIR/plugin" >/dev/null 2>&1; then
      ok "omarchy plugin validate passed"
    else
      warn "omarchy plugin validate failed:"
      omarchy plugin validate "$INSTALL_DIR/plugin" 2>&1 | sed 's/^/     /'
      ((++problems))
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
   Remove:          $INSTALL_DIR/install.sh --uninstall  (add --purge to delete the clone)

EOF

  # 100, not the problem count: the driver must tell "finished but flawed"
  # apart from a hard stop, and a count of 1 would collide with die's exit 1.
  ((problems == 0)) && return 0
  return 100
}

# ------------------------------------------------- the driver, with recovery

# A checkout this script cloned is disposable; the repo this script was run
# from is not — no failure path may delete the user's own working copy.
SELF_CHECKOUT=0
[[ -f $SELF_DIR/unit.toml && -f $SELF_DIR/plugin/manifest.json ]] && SELF_CHECKOUT=1
RECOVERABLE=$((!SELF_CHECKOUT))

if ((FRESH)); then
  if ((SELF_CHECKOUT)); then
    # --fresh means delete the checkout, and the checkout is this script's own
    # repo — the one delete nothing here may do. Copy the script somewhere
    # neutral and run the copy, so the clone is just a directory again.
    tmp=$(mktemp 2>/dev/null)
    if [[ -n $tmp && -f ${BASH_SOURCE[0]} ]] && cp "${BASH_SOURCE[0]}" "$tmp"; then
      exec bash "$tmp" "$@"
    fi
    die "--fresh needs to delete this checkout, and this script lives inside it — run the curl | bash -s -- --fresh form instead"
  elif [[ -e $INSTALL_DIR ]]; then
    wipe_checkout "$INSTALL_DIR" &&
      ok "removed the old checkout at $INSTALL_DIR" ||
      die "--fresh could not remove $INSTALL_DIR — it does not look like a disposable clone"
  fi
fi

attempt=0
while :; do
  install_rc=0
  (install_body) || install_rc=$?
  ((install_rc == 0)) && exit 0
  ((install_rc == 130)) && exit 130
  ((++attempt))
  # A failed install where the checkout may be at fault earns one offer of a
  # clean slate — delete the clone and run the whole thing again. Declining,
  # running out of retries, or a checkout we may not delete all end here.
  if ((RECOVERABLE && attempt == 1)) && [[ -e $INSTALL_DIR ]] &&
    { ((FRESH)) ||
      ask "Remove the checkout at $INSTALL_DIR and try again from a clean clone?" n; }; then
    if wipe_checkout "$INSTALL_DIR"; then
      note "trying again from a clean clone"
      continue
    fi
    warn "cannot remove $INSTALL_DIR — it does not look like a disposable clone"
  fi
  # Warnings still produced a working install — that is a success exit.
  ((install_rc == 100)) && exit 0
  exit "$install_rc"
done
