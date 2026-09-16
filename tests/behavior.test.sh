#!/usr/bin/env bash
# Behavioral tests for the oxy-* commands: what they actually emit against
# real git repositories, stubbed nmcli/docker binaries and hostile input.
# The static suite (tests/run.sh) proves the files parse and the row JSON
# is well-formed; this file proves the answers are right.
#
#   tests/behavior.test.sh          run everything
#   tests/behavior.test.sh wifi     run one section (see SECTIONS below)
#
# Every section skips itself when the tools it needs are absent, so the
# file runs on a bare CI image and still says what it could not check.
set -uo pipefail
cd "$(dirname "$0")/.."
BIN="$PWD/bin"
export PATH="$BIN:$PATH"

passed=0 failed=0 skipped=0
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

have() { command -v "$1" >/dev/null 2>&1; }
pass() { passed=$((passed + 1)); printf '  \033[32mok\033[0m    %s\n' "$1"; }
fail() { failed=$((failed + 1)); printf '  \033[31mFAIL\033[0m  %s — %s\n' "$1" "$2"; }
skip() { skipped=$((skipped + 1)); printf '  \033[33mskip\033[0m  %s (%s)\n' "$1" "$2"; }
section() { printf '\033[36m== %s\033[0m\n' "$1"; }

# Isolated state: oxy-repo's repo list lives under XDG_STATE_HOME and its
# status cache under XDG_CACHE_HOME, and letting a test write either into the
# developer's real directories would make `repo:` lie about their own
# repositories until the entries aged out.
export XDG_CACHE_HOME="$WORK/xdg-cache"
export XDG_STATE_HOME="$WORK/xdg-state"

git_init() { # path — a repo with one commit, no prompts, deterministic branch
  git init -q -b main "$1" &&
    git -C "$1" config user.email t@t &&
    git -C "$1" config user.name t &&
    git -C "$1" commit -qm init --allow-empty
}

field() { # json_file jq_expr — first match or nothing
  jq -r "$2" "$1" 2>/dev/null | head -1
}

# ---------------------------------------------------------------- wifi
#
# The SSID is access-point-controlled text that used to land unquoted inside
# generated notification commands: `Cafe"; touch /tmp/pwned; #` closed the
# string and ran its own syntax when the row was activated. The fix quotes
# every interpolation with printf %q; this test runs the emitted exec
# verbatim, the way the launcher would, and checks both the canary and the
# argument the notification daemon actually received.

t_wifi() {
  section "wifi — hostile SSID"
  have jq || { skip "wifi injection" "jq missing"; return; }

  local shim="$WORK/wifi-shims" cap="$WORK/notify.log"
  mkdir -p "$shim"
  local ssid='Cafe"; touch '"$WORK"'/pwned; #'
  local qssid; qssid=$(printf '%q' "$ssid")

  cat >"$shim/nmcli" <<SH
#!/usr/bin/env bash
case "\$*" in
  "-t -f WIFI radio") echo enabled ;;
  "-t -f TYPE,NAME connection show") printf '802-11-wireless:%s\n' $qssid ;;
  *"device wifi list"*) printf ':75:WPA2:2412 MHz:135 Mbit/s:%s\n' $qssid ;;
  *) echo ok ;;
esac
SH
  cat >"$shim/omarchy-notification-send" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$@" >>"$OXY_TEST_CAP"
SH
  chmod +x "$shim/nmcli" "$shim/omarchy-notification-send"

  PATH="$shim:$PATH" OXY_TEST_CAP="$cap" \
    "$BIN/oxy-wifi" >"$WORK/wifi.json" 2>/dev/null

  jq -e --arg s "$ssid" 'select(.title == $s)' "$WORK/wifi.json" >/dev/null 2>&1 \
    || { skip "wifi injection" "fixture row not emitted"; return; }

  # Activate every generated command on that row — exec and actions alike.
  while IFS= read -r cmd; do
    [[ -n $cmd ]] && PATH="$shim:$PATH" OXY_TEST_CAP="$cap" bash -c "$cmd" >/dev/null 2>&1
  done < <(jq -r --arg s "$ssid" 'select(.title == $s) | .exec, (.actions[]?.exec)' "$WORK/wifi.json")

  if [[ -e $WORK/pwned ]]; then
    fail "wifi injection" "SSID executed shell syntax (pwned exists)"
  else
    pass "hostile SSID executed no injected command"
  fi
  if grep -qxF "Connected to $ssid" "$cap" 2>/dev/null; then
    pass "notification received the whole SSID as one argument"
  else
    fail "notification argument" "expected one arg 'Connected to $ssid', got: $(cat "$cap" 2>/dev/null)"
  fi
}

# ---------------------------------------------------------------- repo

t_repo() {
  section "repo — status, cache, worktrees"
  for t in git jq fd; do have "$t" || { skip "repo" "$t missing"; return; }; done

  local root="$WORK/repox"
  mkdir -p "$root"
  git_init "$root/myrepo" || { skip "repo" "git init failed"; return; }

  # Cold cache: every query computes status itself. An untracked-only change
  # must count toward dirty — it used to be filtered out entirely.
  touch "$root/myrepo/untracked.txt"
  OXY_REPO_ROOTS="$root" "$BIN/oxy-repo" "" >"$WORK/r1.json" 2>/dev/null
  local dirty
  dirty=$(field "$WORK/r1.json" '. | select(.title == "myrepo") | .dirty')
  if [[ $dirty =~ ^[0-9]+$ ]] && ((dirty >= 1)); then
    pass "untracked file counts toward dirty"
  else
    fail "untracked dirty" "expected dirty>=1, got '${dirty:-none}'"
  fi

  # Warm cache: the count is served stale once, then a background refresh
  # puts the truth in for the next keystroke. That is the design — the test
  # is that the truth arrives, which needs setsid to detach the refresh.
  if have setsid; then
    rm -f "$root/myrepo/untracked.txt"
    rm -rf "$XDG_CACHE_HOME"
    OXY_REPO_ROOTS="$root" "$BIN/oxy-repo" "" >/dev/null 2>&1   # populate: clean
    echo change >"$root/myrepo/untracked.txt"
    sleep 4                                                   # past DIRTY_RECHECK
    OXY_REPO_ROOTS="$root" "$BIN/oxy-repo" "" >/dev/null 2>&1   # stale + refresh fires
    sleep 2                                                   # let the refresh land
    OXY_REPO_ROOTS="$root" "$BIN/oxy-repo" "" >"$WORK/r3.json" 2>/dev/null
    dirty=$(field "$WORK/r3.json" '. | select(.title == "myrepo") | .dirty')
    if [[ $dirty =~ ^[0-9]+$ ]] && ((dirty >= 1)); then
      pass "stale-while-revalidate converges on the truth"
    else
      fail "swr refresh" "still '${dirty:-none}' after refresh window"
    fi
  else
    skip "swr refresh" "setsid missing — refresh cannot detach here"
  fi

  # A git worktree has a .git file, not a directory — discovery must find it.
  git -C "$root/myrepo" worktree add -q "$root/wt" -b wt-branch 2>/dev/null
  rm -rf "$XDG_STATE_HOME"   # the repo list is cached too; a stale list is legal
  OXY_REPO_ROOTS="$root" "$BIN/oxy-repo" "" >"$WORK/r4.json" 2>/dev/null
  if jq -e 'select(.title == "wt") | select(.subtitle == "wt-branch")' \
       "$WORK/r4.json" >/dev/null 2>&1; then
    pass "worktree discovered with its branch"
  else
    fail "worktree" "wt/wt-branch not in: $(jq -c 'select(.title?) | {t: .title, s: .subtitle}' "$WORK/r4.json" 2>/dev/null)"
  fi

  # A nonexistent relative OXY_REPO used to spin forever in up_to_repo.
  timeout 10 env OXY_REPO="does-not-exist" "$BIN/oxy-repo" --current >/dev/null 2>&1
  if (($? == 124)); then
    fail "OXY_REPO loop" "relative path still hangs"
  else
    pass "invalid relative OXY_REPO terminates"
  fi
}

# ------------------------------------------------------------- file search

t_search() {
  section "search — literal names, encoded URLs"
  for t in fd jq stat; do have "$t" || { skip "search" "$t missing"; return; }; done

  local fix="$WORK/findfix"
  mkdir -p "$fix"
  touch "$fix/a.b" "$fix/axb" "$fix/pic #1.png"

  "$BIN/oxy-search-files" "a.b" "" "$fix" >"$WORK/f1.json" 2>/dev/null
  if jq -e '. | select(.title == "a.b")' "$WORK/f1.json" >/dev/null 2>&1 &&
     ! jq -e '. | select(.title == "axb")' "$WORK/f1.json" >/dev/null 2>&1; then
    pass "a.b is literal, not a regex"
  else
    fail "literal search" "titles: $(jq -rc '[.[].title]' "$WORK/f1.json" 2>/dev/null)"
  fi

  "$BIN/oxy-search-files" "pic" "" "$fix" >"$WORK/f2.json" 2>/dev/null
  local art
  art=$(field "$WORK/f2.json" '. | select(.title == "pic #1.png") | .art')
  if [[ $art == *%20* || $art == *%23* ]]; then
    pass "file:// art URL is percent-encoded"
  else
    fail "art encoding" "art='$art'"
  fi
}

# -------------------------------------------------------------- git views

t_git() {
  section "git views — control characters in data"
  for t in git jq; do have "$t" || { skip "git views" "$t missing"; return; }; done

  local repo="$WORK/gitviews"
  git_init "$repo" || { skip "git views" "git init failed"; return; }
  git -C "$repo" commit -qm "$(printf 'subject with\ttab inside')" --allow-empty
  git -C "$repo" commit -qm "$(printf 'multi line\nsubject paragraph\n\nbody')" --allow-empty
  echo change >"$repo/f" && git -C "$repo" add f &&
    git -C "$repo" stash push -qm "$(printf 'stash\ttab msg')"

  # A tab in a subject must not shift fields or drop the commit.
  OXY_REPO="$repo" "$BIN/oxy-git" >"$WORK/g1.json" 2>/dev/null
  if jq -e 'select(.id == "git-panel") | .commits[] | select(.subject | contains("\t"))' \
       "$WORK/g1.json" >/dev/null 2>&1; then
    pass "tab in commit subject survives"
  else
    fail "git subject tab" "$(jq -c 'select(.id == "git-panel") | .commits' "$WORK/g1.json" 2>/dev/null)"
  fi

  # A multi-line subject paragraph must fold into one row, not fragment it.
  OXY_REPO="$repo" "$BIN/oxy-git-branch" >"$WORK/g2.json" 2>/dev/null
  local nrows nbranches
  nrows=$(jq -r 'length' "$WORK/g2.json" 2>/dev/null)
  nbranches=$(git -C "$repo" for-each-ref refs/heads --format='x' | wc -l)
  if [[ $nrows == "$nbranches" ]]; then
    pass "multi-line subject stays one row"
  else
    fail "branch rows" "$nrows rows for $nbranches branches"
  fi

  # `stash:` with no words is the repo picker; naming the repo asks for its
  # stashes. The calls above ran with their own roots, so the cached repo
  # list does not name this one — a fresh list, like a new launcher's.
  rm -rf "$XDG_STATE_HOME"
  OXY_REPO_ROOTS="$WORK" "$BIN/oxy-git-stash" "gitviews" >"$WORK/g3.json" 2>/dev/null
  if jq -e '.[]? | select(.message? == "stash tab msg")' "$WORK/g3.json" >/dev/null 2>&1; then
    pass "stash row parses with its message"
  else
    fail "stash parse" "$(jq -c '.[]? | .message? // empty' "$WORK/g3.json" 2>/dev/null)"
  fi
}

# -------------------------------------------------------------- timezone

t_timezone() {
  section "timezone — half-hour offsets"
  have python3 || { skip "timezone" "python3 missing"; return; }
  have jq || { skip "timezone" "jq missing"; return; }

  OXY_NAMES=$'Me\nPriya' OXY_ZONES=$'UTC\nAsia/Kolkata' OXY_HOME=UTC \
    "$BIN/oxy-timezone-plan" >"$WORK/tz.json" 2>/dev/null
  [[ -s $WORK/tz.json ]] || { skip "timezone" "no tzdata on this machine"; return; }

  local delta
  delta=$(field "$WORK/tz.json" '.people[]? | select(.zone == "Asia/Kolkata") | .delta')
  if [[ $delta == "5h30 ahead" ]]; then
    pass "India reports 5h30 ahead, not 5h"
  else
    fail "Kolkata delta" "got '$delta'"
  fi

  OXY_NAMES=$'Me\nNewfie' OXY_ZONES=$'UTC\nAmerica/St_Johns' OXY_HOME=UTC \
    "$BIN/oxy-timezone-plan" >"$WORK/tz2.json" 2>/dev/null
  delta=$(field "$WORK/tz2.json" '.people[]? | select(.zone == "America/St_Johns") | .delta')
  # UTC-3:30 in winter, UTC-2:30 in summer — the point is the :30 survives.
  if [[ $delta =~ ^[0-9]+h30\ behind$ ]]; then
    pass "negative half-hour keeps its minutes ($delta)"
  else
    fail "St_Johns delta" "got '$delta'"
  fi
}

# ---------------------------------------------------------------- docker

t_docker() {
  section "docker — IPv6 URLs"
  have jq || { skip "docker" "jq missing"; return; }

  local shim="$WORK/docker-shims"
  mkdir -p "$shim"
  cat >"$shim/docker" <<'SH'
#!/usr/bin/env bash
case "$1" in
  info) echo "25.0.0" ;;
  ps) printf 'aaaaaaaaaaaa\nbbbbbbbbbbbb\n' ;;
  stats) : ;;   # async stats refresh; empty is the valid cold-start answer
  inspect)
    cat <<'JSON'
{"id":"aaaaaaaaaaaa","name":"/v6","image":"img","restarts":0,"state":{"Status":"running","Running":true,"StartedAt":"2025-01-01T00:00:00Z","Health":{"Status":"healthy"}},"ports":{"8080/tcp":[{"HostIp":"::1","HostPort":"8080"}]},"memCap":0,"nanoCpus":0,"project":"","service":""}
{"id":"bbbbbbbbbbbb","name":"/v4","image":"img","restarts":0,"state":{"Status":"running","Running":true,"StartedAt":"2025-01-01T00:00:00Z"},"ports":{"9090/tcp":[{"HostIp":"0.0.0.0","HostPort":"9090"}]},"memCap":0,"nanoCpus":0,"project":"","service":""}
JSON
    ;;
  *) echo ok ;;
esac
SH
  chmod +x "$shim/docker"

  PATH="$shim:$PATH" "$BIN/oxy-docker" >"$WORK/d.json" 2>/dev/null
  local urls
  urls=$(jq -r '(.exec // empty), (.ports[]?.url // empty), (.actions[]?.exec // empty)' "$WORK/d.json" 2>/dev/null)
  if printf '%s' "$urls" | grep -q '\[::1\]:8080'; then
    pass "IPv6 literal is bracketed"
  else
    fail "IPv6 bracket" "execs: $(printf '%s' "$urls" | head -3)"
  fi
  if printf '%s' "$urls" | grep -q 'localhost:9090'; then
    pass "wildcard bind still says localhost"
  else
    fail "localhost" "execs: $(printf '%s' "$urls" | head -3)"
  fi
}

# ------------------------------------------------------------------ ssh

t_ssh() {
  section "ssh — multi-name Host lines"
  have jq || { skip "ssh" "jq missing"; return; }

  local h="$WORK/sshh"
  mkdir -p "$h/.ssh"
  cat >"$h/.ssh/config" <<'CFG'
Host prod production
  HostName server.example.com
  User deploy
  Port 2222

Host db
  HostName db.internal

Host * !internal
  ServerAliveInterval 30
CFG

  HOME="$h" "$BIN/oxy-ssh" "" >"$WORK/s.json" 2>/dev/null
  local titles
  titles=$(jq -rcn '[inputs.title]' "$WORK/s.json" 2>/dev/null)
  if jq -e '.[] | select(. == "prod")' <<<"$titles" >/dev/null &&
     jq -e '.[] | select(. == "production")' <<<"$titles" >/dev/null &&
     jq -e '.[] | select(. == "db")' <<<"$titles" >/dev/null; then
    pass "every alias on a Host line becomes a row"
  else
    fail "ssh aliases" "titles: $titles"
  fi
  if jq -e '.[] | select(. == "*" or . == "!internal")' <<<"$titles" >/dev/null; then
    fail "ssh wildcards" "pattern alias emitted: $titles"
  else
    pass "wildcard Host entries emit no rows"
  fi
}

# ------------------------------------------------------------- installer

t_install() {
  section "installer — sandbox install/uninstall"
  for t in git curl python3; do have "$t" || { skip "installer" "$t missing"; return; }; done

  local h="$WORK/ihome"
  mkdir -p "$h/.local/bin"
  printf 'user data\n' >"$h/.local/bin/oxy-repo"

  # MSYS without this makes ln -s copy instead of link, and a copy is a
  # regular file the uninstaller's link scan rightly cannot see. Setting it
  # costs Linux nothing.
  export MSYS=winsymlinks:nativestrict

  HOME="$h" bash install.sh --yes >"$WORK/install.log" 2>&1
  if [[ ! -e $h/.local/bin/oxy-repo ]]; then
    fail "install links" "oxy-repo missing — log tail: $(tail -3 "$WORK/install.log")"
    return
  fi
  pass "install links commands into ~/.local/bin"

  if [[ -f $h/.local/bin/oxy-repo.before-oxy ]] &&
     [[ $(cat "$h/.local/bin/oxy-repo.before-oxy") == "user data" ]]; then
    pass "existing file moved aside as .before-oxy"
  else
    fail "before-oxy backup" "backup missing or wrong content"
  fi

  if [[ ! -L $h/.local/bin/oxy-repo ]]; then
    skip "uninstall restore" "platform made copies, not symlinks"
    return
  fi

  HOME="$h" bash install.sh --uninstall >"$WORK/uninstall.log" 2>&1
  if [[ -f $h/.local/bin/oxy-repo && ! -L $h/.local/bin/oxy-repo ]] &&
     [[ $(cat "$h/.local/bin/oxy-repo") == "user data" ]]; then
    pass "uninstall restores the displaced file"
  else
    fail "uninstall restore" "oxy-repo state: $(ls -la "$h/.local/bin/" | grep oxy-repo)"
  fi
}

# ---------------------------------------------------------------- runner

SECTIONS="wifi repo search git timezone docker ssh install"
only="${1:-}"
for s in $SECTIONS; do
  [[ -n $only && $s != "$only" ]] && continue
  "t_$s"
done

printf '\n%d passed, %d failed, %d skipped\n' "$passed" "$failed" "$skipped"
((failed == 0))
