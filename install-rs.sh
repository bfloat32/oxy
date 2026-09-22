#!/bin/bash
# Oxy (Rust core) installer — the experimental daemon-backed build.
#
#   git clone -b experimental/rust-core https://github.com/bfloat32/oxy.git ~/.local/share/oxy-rs
#   ~/.local/share/oxy-rs/install-rs.sh
#
# (The repo is private, so the curl-pipe form needs a token in scope:
#   curl -fsSL -H "Authorization: Bearer $GH_TOKEN" \
#     https://raw.githubusercontent.com/bfloat32/oxy/experimental/rust-core/install-rs.sh | bash)
#
# Installs the Rust-core launcher alongside the script one: a second checkout,
# a second plugin id, its own keybinding, and the oxyd daemon both frontends
# can share the machine with. The stock install at ~/.local/share/oxy keeps
# oma.oxy, Super+K and every oxy-* command exactly as they were.
#
# What goes where:
#
#   repo clone        ~/.local/share/oxy-rs            (or beside this script)
#   daemon + CLI      ~/.local/bin/oxyd, oxy           built by cargo
#   plugin            ~/.config/omarchy/plugins/oma.oxyrs/
#                     a real dir: generated manifest.json + a symlink per
#                     file in plugin/ — the id has to differ from oma.oxy,
#                     so the plugin dir cannot be a single link
#   keybinding        ~/.config/hypr/modules.d/oxyrs-keys.lua -> repo/hypr
#   extensions        shared with the main install: existing links win,
#                     whichever checkout made them serves both launchers
#
# Settings, pins, frecency and history are shared too — both builds read and
# write the same files, so switching between them loses nothing.
#
#   ./install-rs.sh --uninstall   takes out what this script put in
#   ./install-rs.sh --purge       uninstall and delete the clone too
#   ./install-rs.sh --fresh       delete the checkout and install clean
#   ./install-rs.sh --yes         answer yes to every question, for scripts

set -uo pipefail

REPO_URL="${OXY_REPO:-https://github.com/bfloat32/oxy.git}"
BRANCH="${OXY_BRANCH:-experimental/rust-core}"
INSTALL_DIR="${OXY_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/oxy-rs}"
PLUGIN_ID="oma.oxyrs"
# Where the main install lives: links pointing there belong to it, and an
# uninstall hands them back rather than leaving them dangling.
MAIN_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/oxy"

CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
HYPR_DIR="$CONFIG_HOME/hypr"
HYPR_MODULES="$HYPR_DIR/modules.d"
PLUGINS_DIR="$CONFIG_HOME/omarchy/plugins"
PLUGIN_DIR="$PLUGINS_DIR/$PLUGIN_ID"
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
    sed -n '2,34p' "$0"
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

# For the shared files — extension manifests, snippet defaults — any existing
# link wins, whichever checkout made it: both launchers read through the same
# path, and the install that owns the link stays the one git pull updates.
# A real file in the way is still moved aside; only links are trusted.
link_shared() {
  local src="$1" dst="$2"
  [[ -L $dst ]] && return 0
  link_path "$src" "$dst"
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
  printf '\n%s %s\n' "$(cyan '::')" "$(bold 'Removing Oxy (Rust)')"

  sources=("$INSTALL_DIR")
  # Only a checkout that could have installed this — core/Cargo.toml is the
  # mark of the rust branch — may have its links removed. Without that check,
  # running --uninstall from a main checkout would strip the stable install's
  # own config links.
  if [[ -f $SELF_DIR/unit.toml && -f $SELF_DIR/plugin/manifest.json &&
    -f $SELF_DIR/core/Cargo.toml && $SELF_DIR != "$INSTALL_DIR" ]]; then
    sources+=("$SELF_DIR")
  fi

  for src_dir in "${sources[@]}"; do
    # The plugin dir is real, not a link: empty it of links that point into
    # this checkout and of the manifest this script generated — recognized by
    # its id, never by its name alone.
    if [[ -d $PLUGIN_DIR ]]; then
      while IFS= read -r link; do
        rm -f "$link" && ok "plugin/$(basename "$link") unlinked"
      done < <(find "$PLUGIN_DIR" -type l -lname "$src_dir/*" 2>/dev/null)
      if [[ -f $PLUGIN_DIR/manifest.json ]] &&
        grep -q "\"id\": \"$PLUGIN_ID\"" "$PLUGIN_DIR/manifest.json" 2>/dev/null; then
        rm -f "$PLUGIN_DIR/manifest.json" && ok "generated manifest removed"
      fi
      rmdir "$PLUGIN_DIR" 2>/dev/null && ok "$PLUGIN_ID removed" || true
    fi

    for bin in oxyd oxy oxy-agent; do
      unlink_path "$src_dir/core/target/release/$bin" "$BIN_DIR/$bin" && ok "$bin unlinked"
    done

    # The shared links leave only when they point into this checkout — ones
    # the main install made are its own to keep.
    (cd "$src_dir/config" 2>/dev/null && find . -type f -printf '%P\n') | while read -r rel; do
      unlink_path "$src_dir/config/$rel" "$CONFIG_HOME/$rel" && ok "config/$rel unlinked"
    done

    for file in "$src_dir"/bin/oxy-*; do
      [[ -e $file ]] || continue
      unlink_path "$file" "$BIN_DIR/$(basename "$file")" && ok "$(basename "$file") unlinked"
    done

    # Both spellings: this script links oxyrs-keys.lua, but a run of the main
    # installer from this checkout would have made oxy-keys-rs.lua — the same
    # file under the other name. Either way it is this checkout's link.
    for name in oxyrs-keys.lua oxy-keys-rs.lua; do
      unlink_path "$src_dir/hypr/keys-rs.lua" "$HYPR_MODULES/$name" &&
        ok "hypr/$name unlinked"
    done
  done

  # Same sweep as the link removal: anything pointing into a spelling of this
  # checkout that the walks above could not see.
  stray=0
  while IFS= read -r link; do
    rm -f "$link" && ((++stray))
  done < <(find "$PLUGIN_DIR" "$BIN_DIR" "$HYPR_MODULES" "$CONFIG_HOME/omarchy" \
    -type l \( -lname "$INSTALL_DIR/*" -o -lname "$SELF_DIR/*" \) 2>/dev/null)
  ((stray > 0)) && ok "$stray stale links removed"
  [[ -d $PLUGIN_DIR ]] && rmdir "$PLUGIN_DIR" 2>/dev/null

  # The shared files this install owned go back to the main checkout when one
  # exists — removing them outright would break the launcher that stays.
  if [[ -d $MAIN_DIR/config && $MAIN_DIR != "$INSTALL_DIR" ]]; then
    handed=0
    while IFS= read -r rel; do
      if [[ ! -e $CONFIG_HOME/$rel && ! -L $CONFIG_HOME/$rel ]]; then
        link_path "$MAIN_DIR/config/$rel" "$CONFIG_HOME/$rel" 2>/dev/null && ((++handed))
      fi
    done < <(cd "$MAIN_DIR/config" && find . -type f -printf '%P\n')
    ((handed > 0)) && ok "handed $handed shared config links back to the main install"
  fi
  if [[ -d $MAIN_DIR/bin && $MAIN_DIR != "$INSTALL_DIR" ]]; then
    for file in "$MAIN_DIR"/bin/oxy-*; do
      [[ -e $file ]] || continue
      [[ -e $BIN_DIR/$(basename "$file") || -L $BIN_DIR/$(basename "$file") ]] && continue
      link_path "$file" "$BIN_DIR/$(basename "$file")" 2>/dev/null
    done
  fi

  restored=0
  while IFS= read -r backup; do
    base="${backup%.before-oxy}"
    if [[ ! -e $base && ! -L $base ]]; then
      mv "$backup" "$base" 2>/dev/null && ((++restored))
    fi
  done < <(find "$PLUGINS_DIR" "$BIN_DIR" "$HYPR_MODULES" "$CONFIG_HOME/omarchy" \
    -name '*.before-oxy' \( -type f -o -type d -o -type l \) \
    ! -name '*.before-oxy.*' 2>/dev/null)
  ((restored > 0)) && ok "$restored backed-up file(s) restored"

  # plugins[] is what makes the shell load it; the layout entry renders
  # nothing once the files are gone, so only the list entry has to go.
  local_cfg="$CONFIG_HOME/omarchy/shell.json"
  if [[ -f $local_cfg ]] && command -v python3 >/dev/null &&
    grep -q "$PLUGIN_ID" "$local_cfg"; then
    cp -p "$local_cfg" "$local_cfg.before-oxy.$(date +%s)"
    OXY_FORGET_ID="$PLUGIN_ID" OXY_FORGET_CONFIG="$local_cfg" python3 - <<'PY' 2>/dev/null
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
  command -v omarchy-shell >/dev/null && {
    omarchy-shell shell rescanPlugins >/dev/null 2>&1
    omarchy-shell shell reloadConfig >/dev/null 2>&1
  }
  command -v hyprctl >/dev/null && hyprctl reload >/dev/null 2>&1

  printf '\n%s\n' "$(green 'Oxy (Rust) is off. The main install is untouched.')"

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

  leftovers=$(find "$CONFIG_HOME" "$BIN_DIR" "$HYPR_MODULES" \
    -name '*.before-oxy*' 2>/dev/null)
  [[ -n $leftovers ]] && {
    note "Files set aside along the way — restore or delete them by hand:"
    printf '%s\n' "$leftovers" | sed 's/^/      /'
  }

  note "Settings, pins and history are shared files — left in place:"
  note "  ~/.config/omarchy/oxy*.json   ~/.local/state/omarchy/oxy-*"
  echo
  exit 0
fi

# ------------------------------------------------------------------ install

# The whole install runs in a subshell so a failure inside it can be answered
# with a clean-slate retry by the driver below. Exit codes: 0 installed, 100
# installed but the verify step found problems, 1 hard stop, 130 interrupted.
install_body() {
  printf '\n%s %s\n' "$(cyan '::')" "$(bold 'Oxy (Rust) — the experimental core')"
  note "installs beside the main Oxy: same settings, same extensions, its own key"

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

  mkdir -p "$PLUGINS_DIR" "$BIN_DIR" "$CONFIG_HOME/omarchy" 2>/dev/null
  for dir in "$PLUGINS_DIR" "$BIN_DIR" "$CONFIG_HOME/omarchy"; do
    [[ -w $dir ]] || die "cannot write to $dir — check its permissions"
  done

  # -------------------------------------------------------- the repo itself

  if [[ -f $SELF_DIR/unit.toml && -f $SELF_DIR/plugin/manifest.json && -f $SELF_DIR/core/Cargo.toml ]]; then
    INSTALL_DIR="$SELF_DIR"
    step "Using this checkout"
    ok "$INSTALL_DIR"
    # A checkout that is not the experimental branch still installs — it is
    # the user's tree — but say so, because main has no Shell.qml to serve.
    head_branch=$(git -C "$INSTALL_DIR" rev-parse --abbrev-ref HEAD 2>/dev/null)
    [[ -n $head_branch && $head_branch != "$BRANCH" ]] &&
      warn "this checkout is on $head_branch, not $BRANCH"
  else
    step "Getting the source"
    if [[ -d $INSTALL_DIR/.git ]]; then
      origin=$(norm_url "$(git -C "$INSTALL_DIR" remote get-url origin 2>/dev/null)")
      if [[ -n $origin && $origin != "$(norm_url "$REPO_URL")" ]]; then
        die "$INSTALL_DIR is a clone of $(git -C "$INSTALL_DIR" remote get-url origin), not $REPO_URL — set OXY_DIR or OXY_REPO"
      fi
      if ! fetch_err=$(git -C "$INSTALL_DIR" fetch origin 2>&1); then
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
      elif ! git -C "$INSTALL_DIR" rev-parse --verify --quiet "origin/$BRANCH" >/dev/null; then
        die "branch $BRANCH is not on $REPO_URL — check OXY_BRANCH"
      elif git -C "$INSTALL_DIR" pull --quiet --rebase --autostash origin "$BRANCH" 2>/dev/null; then
        ok "updated $INSTALL_DIR"
      else
        warn "$INSTALL_DIR has changes that conflict with the update — left as it is."
        note "update it by hand:  git -C $INSTALL_DIR stash pop; git -C $INSTALL_DIR pull --rebase"
        note "or start clean:     $INSTALL_DIR/install-rs.sh --fresh"
      fi
    elif [[ -e $INSTALL_DIR ]]; then
      if checkout_looks_ours "$INSTALL_DIR"; then
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

  # The daemon build and the thin frontend are what make this the Rust
  # install — a checkout without them cannot serve this plugin id.
  if [[ ! -f $INSTALL_DIR/core/Cargo.toml || ! -f $INSTALL_DIR/plugin/Shell.qml ||
    ! -f $INSTALL_DIR/install-rs.sh ]]; then
    die "the checkout at $INSTALL_DIR is not the rust-core branch — check OXY_BRANCH"
  fi

  # ------------------------------------------------------------ dependencies

  step "Dependencies"
  # The daemon still execs the declared search commands, so the script
  # providers want what they always wanted — minus nothing, plus a compiler.
  declare -A PKG_FOR=(
    [qalc]=libqalculate [jq]=jq [curl]=curl [fd]=fd
    [mpv]=mpv [python3]=python [git]=git [wl-copy]=wl-clipboard
    [busctl]=systemd [cal]=util-linux
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

  optional=()
  for cmd in gh docker playerctl socat notify-send; do
    command -v "$cmd" >/dev/null || optional+=("$cmd")
  done
  ((${#optional[@]})) && note "optional, unlock more keywords: ${optional[*]}"

  # ------------------------------------------------------------------- build

  step "Building oxyd"
  # rustup keeps cargo under ~/.cargo/bin, which a non-login shell has not
  # necessarily heard of; its env file fixes PATH for the rest of the run.
  [[ -f $HOME/.cargo/env ]] && . "$HOME/.cargo/env"
  if ! command -v cargo >/dev/null; then
    warn "cargo not found — the daemon cannot build without it"
    if command -v pacman >/dev/null &&
      { ((ASSUME_YES)) || ask "Install rust with pacman?"; }; then
      sudo pacman -S --needed --noconfirm rust ||
        warn "pacman did not finish — install rust by hand"
    fi
  fi
  command -v cargo >/dev/null ||
    die "no cargo — install rust (pacman -S rust, or rustup) and re-run"

  note "first build is a cold compile — a minute or two, then incremental"
  (cd "$INSTALL_DIR/core" && cargo build --release -p oxyd -p oxy) ||
    die "cargo build failed — the error above is the reason"
  ok "oxyd + oxy built"

  # --------------------------------------------------------------------- bin

  step "Linking the binaries"
  link_path "$INSTALL_DIR/core/target/release/oxyd" "$BIN_DIR/oxyd" || die "could not link oxyd"
  link_path "$INSTALL_DIR/core/target/release/oxy" "$BIN_DIR/oxy" || die "could not link oxy"
  # oxy-agent is the native sibling of the bin/oxy-agent script — same socket,
  # same verbs — so this install claims the name. A stable install.sh run
  # force-links its script back; the last installer wins, same as oxy/oxyd.
  link_path "$INSTALL_DIR/core/target/release/oxy-agent" "$BIN_DIR/oxy-agent" ||
    die "could not link oxy-agent"
  ok "oxyd, oxy, oxy-agent -> $BIN_DIR"
  case :$PATH: in
  *":$BIN_DIR:"*) ;;
  *)
    warn "$BIN_DIR is not on PATH in this shell."
    note "Omarchy puts it there by default; if this shell disagrees, add it to your profile." ;;
  esac

  # The oxy-* commands the script providers call: shared names, shared job.
  # Links the main install made are left alone — its checkout updates them.
  count=0
  for file in "$INSTALL_DIR"/bin/oxy-*; do
    [[ -f $file ]] || continue
    chmod +x "$file" 2>/dev/null
    link_shared "$file" "$BIN_DIR/$(basename "$file")" && ((++count))
  done
  ok "$count helper commands -> $BIN_DIR (existing links kept)"

  # ------------------------------------------------------------------ config

  step "Linking the extension definitions"
  # Same sharing rule: the registry path is one directory for both launchers,
  # and whoever linked a file first stays its owner.
  count=0
  while IFS= read -r rel; do
    link_shared "$INSTALL_DIR/config/$rel" "$CONFIG_HOME/$rel" && ((++count))
  done < <(cd "$INSTALL_DIR/config" && find . -type f -printf '%P\n')
  ok "$count files -> $CONFIG_HOME (shared with the main install)"

  # ------------------------------------------------------------------ plugin

  step "Linking the plugin"
  # The id has to differ from oma.oxy, so the plugin dir is real: a generated
  # manifest.json plus one symlink per file in plugin/. New views land on the
  # next install run; files that vanished lose their link.
  mkdir -p "$PLUGIN_DIR" || die "cannot create $PLUGIN_DIR"
  sed -e 's/"id": "oma.oxy"/"id": "'"$PLUGIN_ID"'"/' \
      -e 's/"name": "Oxy"/"name": "Oxy (Rust)"/' \
      "$INSTALL_DIR/plugin/manifest.json" >"$PLUGIN_DIR/manifest.json" ||
    die "could not write $PLUGIN_DIR/manifest.json"
  grep -q "\"id\": \"$PLUGIN_ID\"" "$PLUGIN_DIR/manifest.json" ||
    die "the generated manifest did not take the id $PLUGIN_ID"

  count=0
  for file in "$INSTALL_DIR"/plugin/*; do
    [[ -f $file ]] || continue
    [[ $(basename "$file") == manifest.json ]] && continue
    link_path "$file" "$PLUGIN_DIR/$(basename "$file")" && ((++count))
  done
  # Links to files that are gone dangle; remove only ones pointing into this
  # checkout so a foreign file dropped in by hand is never swept.
  while IFS= read -r link; do
    [[ -e $link ]] || { rm -f "$link" && note "removed stale link $(basename "$link")"; }
  done < <(find "$PLUGIN_DIR" -type l -lname "$INSTALL_DIR/plugin/*" 2>/dev/null)
  ok "$PLUGIN_DIR — manifest + $count files"

  # ------------------------------------------------------------- keybinding

  step "The keybinding"
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
    # Only the rs binding — the stock keys.lua belongs to the main install,
    # and linking it here would put both launchers on the same key.
    link_path "$INSTALL_DIR/hypr/keys-rs.lua" "$HYPR_MODULES/oxyrs-keys.lua" ||
      warn "could not link the keybinding"
    ok "keys-rs.lua -> $HYPR_MODULES/oxyrs-keys.lua (Super+R)"
    hyprctl reload >/dev/null 2>&1 && note "hyprland reloaded — Super+R is live"
  else
    warn "$main not found — link skipped."
    note "Source $INSTALL_DIR/hypr/keys-rs.lua from your Hyprland config by hand."
  fi

  # ---------------------------------------------------------------- enable

  step "Turning it on"
  command -v omarchy-shell >/dev/null && {
    omarchy-shell shell rescanPlugins >/dev/null 2>&1
    sleep 1
  }
  if command -v omarchy >/dev/null; then
    omarchy plugin enable "$PLUGIN_ID" >/dev/null 2>&1 &&
      ok "enabled" ||
      warn "could not enable it — run: omarchy plugin enable $PLUGIN_ID"
  else
    note "no shell to talk to — it will be picked up when Omarchy starts"
  fi

  # ------------------------------------------------------------------ done

  step "Verify"
  problems=0
  [[ -f $PLUGIN_DIR/Shell.qml && -f $PLUGIN_DIR/manifest.json ]] ||
    { warn "plugin files not reachable under $PLUGIN_DIR"; ((++problems)); }
  command -v oxyd >/dev/null ||
    { warn "oxyd not on PATH (open a new shell)"; ((++problems)); }
  [[ -f $CONFIG_HOME/omarchy/oxy/extensions/emoji.json ]] ||
    { warn "extension links missing under $CONFIG_HOME/omarchy/oxy"; ((++problems)); }
  # The daemon answering over the socket is the check nothing else covers —
  # `oxy query` falls back to an in-process engine, so only `oxy send` proves
  # the wire works. If no daemon is up yet, start one and probe again. Both
  # ends are bounded: a verify step that can hang forever is not a check.
  if [[ -x $BIN_DIR/oxy && -x $BIN_DIR/oxyd ]]; then
    daemon_up() {
      printf '{"op":"ping"}\n' | timeout 5 "$BIN_DIR/oxy" send 2>/dev/null |
        grep -q '"op":"pong"'
    }
    probe_pid=""
    if ! daemon_up; then
      # </dev/null matters: the script's own stdin may be a still-open curl
      # pipe, and a daemon that inherits it holds the installer open.
      "$BIN_DIR/oxyd" >/dev/null 2>&1 </dev/null &
      probe_pid=$!
      sleep 1
    fi
    if daemon_up; then
      ok "daemon answered over the socket"
    else
      warn "oxyd did not answer — try: oxy send, then type {\"op\":\"ping\"}"
      ((++problems))
    fi
    [[ -n $probe_pid ]] && kill "$probe_pid" 2>/dev/null
  fi
  if command -v omarchy >/dev/null; then
    # Same two guards: a validator that waits on the piped stdin or takes a
    # shell call that never returns would otherwise stall the install at the
    # last step.
    if timeout 15 omarchy plugin validate "$PLUGIN_DIR" </dev/null >/dev/null 2>&1; then
      ok "omarchy plugin validate passed"
    else
      warn "omarchy plugin validate failed or timed out:"
      timeout 15 omarchy plugin validate "$PLUGIN_DIR" </dev/null 2>&1 | sed 's/^/     /'
      ((++problems))
    fi
  fi

  if ((problems == 0)); then
    printf '\n%s\n' "$(green "$(bold 'Oxy (Rust) is installed.')")"
  else
    printf '\n%s\n' "$(yellow 'Installed with warnings — read the lines above.')"
  fi

  cat <<EOF

   $(bold 'Press') $(cyan 'Super+R') $(bold 'for the Rust build,') $(cyan 'Super+K') $(bold 'for the script one.')
   They share settings, extensions, pins and history — compare away.

   Keybind:         edit the one line at the top of $INSTALL_DIR/hypr/keys-rs.lua
   Daemon:          oxyd serves ${XDG_RUNTIME_DIR:-~/.local/state/omarchy}/oxyd.sock; oxy query "cal:" talks to it
   Update later:    re-run this same command — pulls, rebuilds, relinks
   Remove:          $INSTALL_DIR/install-rs.sh --uninstall  (add --purge to delete the clone)

EOF

  ((problems == 0)) && return 0
  return 100
}

# ------------------------------------------------- the driver, with recovery

SELF_CHECKOUT=0
[[ -f $SELF_DIR/unit.toml && -f $SELF_DIR/plugin/manifest.json && -f $SELF_DIR/core/Cargo.toml ]] &&
  SELF_CHECKOUT=1
RECOVERABLE=$((!SELF_CHECKOUT))

if ((FRESH)); then
  if ((SELF_CHECKOUT)); then
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
  if ((RECOVERABLE && attempt == 1)) && [[ -e $INSTALL_DIR ]] &&
    { ((FRESH)) ||
      ask "Remove the checkout at $INSTALL_DIR and try again from a clean clone?" n; }; then
    if wipe_checkout "$INSTALL_DIR"; then
      note "trying again from a clean clone"
      continue
    fi
    warn "cannot remove $INSTALL_DIR — it does not look like a disposable clone"
  fi
  ((install_rc == 100)) && exit 0
  exit "$install_rc"
done
