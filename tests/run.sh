#!/bin/bash
# The local check suite: everything CI runs, runnable before pushing.
#
#   tests/run.sh                  every layer
#   tests/behavior.test.sh        just the end-to-end script checks
#   python3 tests/cases.py [ext]  the extension behavior fixtures
set -uo pipefail
cd "$(dirname "$0")/.."
failed=0

say() { printf '\033[36m::\033[0m %s\n' "$1"; }
bad() { printf '\033[33m!!\033[0m %s\n' "$1"; failed=1; }

say "bash syntax"
bash -n install.sh || bad "install.sh"
for f in bin/*; do
  head -1 "$f" | grep -q bash || continue
  bash -n "$f" || bad "$f"
done

say "python syntax"
for f in bin/*; do
  head -1 "$f" | grep -q python || continue
  python3 -c "import sys; compile(open(sys.argv[1], encoding='utf-8').read(), sys.argv[1], 'exec')" "$f" \
    || bad "$f"
done

say "shellcheck"
if command -v shellcheck >/dev/null 2>&1; then
  files="install.sh"
  for f in bin/* tests/*.sh; do
    head -1 "$f" | grep -q bash && files="$files $f"
  done
  # A CRLF checkout reads as errors on every line; normalize for the check.
  if [[ ${OSTYPE:-} == msys* || ${OSTYPE:-} == cygwin* ]]; then
    tmp=$(mktemp -d)
    for f in $files; do mkdir -p "$tmp/$(dirname "$f")"; tr -d '\r' < "$f" > "$tmp/$f"; done
    (cd "$tmp" && shellcheck -S error -x $files) || bad "shellcheck"
    rm -rf "$tmp"
  else
    shellcheck -S error -x $files || bad "shellcheck"
  fi
else
  echo "   shellcheck not installed — CI runs it; skipping"
fi

say "executable bits"
# The scripts are exec'd by name, so a lost +x is a broken keyword.
git ls-files -s bin install.sh | awk '$1 != "100755" { print $4; bad=1 } END { exit bad }' \
  || bad "a script lost its executable bit"

say "extension JSON + case shape"
python3 - <<'PY' || bad "extension JSON or a cases file is malformed"
import glob, json, sys
bad = []
CASE_KEYS = {"why": str, "setup": str, "requires": str, "query": str, "minRows": int, "maxRows": int,
             "row": int, "view": str, "fields": list, "absent": list,
             "atMost": dict, "matches": dict}
seen_ids, seen_kw = {}, {}
for path in sorted(glob.glob("config/**/*.json", recursive=True)):
    try:
        data = json.load(open(path, encoding="utf-8"))
    except Exception as e:
        print(path, e)
        bad.append(path)
        continue
    if path.endswith(".cases.json"):
        if not isinstance(data, list) or not data:
            print(path, "cases file is not a non-empty array")
            bad.append(path)
            continue
        for i, case in enumerate(data):
            if not isinstance(case, dict):
                print(path, f"case {i} is not an object")
                bad.append(path)
                continue
            for k, v in case.items():
                if k not in CASE_KEYS:
                    print(path, f"case {i} uses unknown key {k!r}")
                    bad.append(path)
                elif not isinstance(v, CASE_KEYS[k]):
                    print(path, f"case {i} {k} is not {CASE_KEYS[k].__name__}")
                    bad.append(path)
        continue
    # an extension definition: unique id and keyword, a way to answer
    if isinstance(data, dict) and "extensions" not in path:
        eid = data.get("id")
        kw = data.get("keyword")
        if eid:
            if eid in seen_ids:
                print(path, f"duplicate id {eid!r} (also {seen_ids[eid]})")
                bad.append(path)
            seen_ids[eid] = path
        if kw:
            if kw in seen_kw:
                print(path, f"duplicate keyword {kw!r} (also {seen_kw[kw]})")
                bad.append(path)
            seen_kw[kw] = path
        if not data.get("search") and not data.get("socket"):
            print(path, "has neither search nor socket")
            bad.append(path)
sys.exit(1 if bad else 0)
PY

say "view list, core vs frontend"
# `KNOWN_VIEWS` is what `oxy test --only manifest` checks a manifest against,
# and the frontends are what actually draws a view. A name in one and not the
# other is a manifest that passes a check and falls back to `list` on screen.
python3 - <<'PY' || bad "KNOWN_VIEWS and plugin/Shell.qml disagree"
import pathlib, re, sys
core = pathlib.Path("core/crates/oxy-core/src/registry/def.rs").read_text(encoding="utf-8")
m = re.search(r"pub const KNOWN_VIEWS: &\[&str\] = &\[(.*?)\];", core, re.S)
core_views = set(re.findall(r'"([a-z]+)"', m.group(1)))
qml = pathlib.Path("plugin/Shell.qml").read_text(encoding="utf-8")
m2 = re.search(r"readonly property var knownViews: \[(.*?)\]", qml, re.S)
# `answer` and `loading` are the launcher's own views, never an extension's.
front = set(re.findall(r'"([a-z]+)"', m2.group(1))) - {"answer", "loading"}
if core_views != front:
    print("in the core only:", sorted(core_views - front))
    print("in the frontend only:", sorted(front - core_views))
    sys.exit(1)
PY

say "qml syntax"
# No qmllint on the box (and none in CI either), so the frontend gets the
# next-best static pass: every .qml must hold balanced ()[]{} once comments,
# strings, `...${}`...` templates and /regex/ literals are stepped over. A
# mismatched brace is a load error Quickshell reports at startup, which is
# the worst place to meet it. `/` is a regex after an operator and division
# after a value — the rule JS itself uses.
python3 - <<'PY' || bad "a .qml file is unbalanced or unterminated"
import glob, sys
PAIRS = {')': '(', ']': '[', '}': '{'}
# After these a `/` opens a regex; after anything else it divides. The words
# cover `return /re/` and `typeof /re/` — positions an operator char misses.
OPS = set("(,=:[!&|?{};+-*%^~<>")
KEYWORDS = {"return", "case", "typeof", "in", "of", "new", "delete",
            "void", "do", "else", "instanceof", "yield", "await"}
bad = []
for path in sorted(glob.glob("plugin/**/*.qml", recursive=True)):
    src = open(path, encoding="utf-8").read()
    stack, i, n = [], 0, len(src)
    # prev: last significant char; word: identifier chars since then — the
    # two things the regex/division decision needs.
    state, tmpl, prev, word = "code", 0, None, ""
    while i < n:
        c = src[i]
        if state == "code":
            if src.startswith("//", i):
                j = src.find("\n", i); i = n if j < 0 else j
            elif src.startswith("/*", i):
                j = src.find("*/", i + 2)
                if j < 0: bad.append((path, "unterminated /* comment")); break
                i = j + 2
            elif c in "\"'": state = c; i += 1
            elif c == '`': state, tmpl = "tmpl", 0; i += 1
            elif c == '/' and (prev is None or prev in OPS or word in KEYWORDS):
                state = "regex"; i += 1
            elif c in "([{": stack.append((c, i)); prev, word = c, ""; i += 1
            elif c in ")]}":
                if not stack or stack[-1][0] != PAIRS[c]:
                    bad.append((path, f"stray {c!r} at offset {i}")); break
                stack.pop(); prev, word = c, ""; i += 1
            elif c.isalnum() or c in "_$":
                word += c; prev = c; i += 1
            elif not c.isspace():
                prev, word = c, ""; i += 1
            else: i += 1
        elif state == "regex":
            if c == '\\': i += 2
            elif c == '[': state = "class"; i += 1
            elif c == '/': state = "code"; prev, word = '/', ""; i += 1
            elif c == '\n': bad.append((path, f"unterminated regex at offset {i}")); break
            else: i += 1
        elif state == "class":
            if c == '\\': i += 2
            elif c == ']': state = "regex"; i += 1
            else: i += 1
        elif state == "tmpl":
            if c == '\\': i += 2
            elif c == '`' and tmpl == 0:
                state = "code"; prev, word = '`', ""; i += 1
            elif src.startswith("${", i): tmpl += 1; i += 2
            elif c == '}' and tmpl > 0: tmpl -= 1; i += 1
            else: i += 1
        else:  # inside ' or "
            if c == '\\': i += 2
            elif c == state:
                state = "code"; prev, word = c, ""; i += 1
            else: i += 1
    else:
        if state in ("regex", "class"):
            bad.append((path, "unterminated regex literal"))
        elif state != "code":
            bad.append((path, f"unterminated {state} string"))
        elif stack:
            bad.append((path, f"unclosed {stack[-1][0]!r} at offset {stack[-1][1]}"))
for path, why in bad: print(path, why)
sys.exit(1 if bad else 0)
PY

say "rust core"
# The same steps the workflow's rust job runs — skipped where cargo is not
# installed rather than failed, the way shellcheck degrades.
if command -v cargo >/dev/null 2>&1; then
  (cd core && cargo check --workspace) || bad "cargo check"
  (cd core && cargo test --workspace) || bad "cargo test"
  (cd core && cargo clippy --workspace --all-targets -- -D warnings) || bad "cargo clippy"
  # rustfmt rides its own component — a toolchain can carry cargo without it.
  if cargo fmt --version >/dev/null 2>&1; then
    (cd core && cargo fmt --check) || bad "cargo fmt"
  else
    echo "   rustfmt not installed — skipping"
  fi
  # The launcher reads a manifest leniently — an unknown view falls back to
  # `list`, a typo'd `native` name to the script — so the strict checks live
  # in the CLI. It reads XDG paths, so it gets a sandbox holding this repo's
  # extensions rather than whatever the machine has installed.
  sandbox=$(mktemp -d)
  mkdir -p "$sandbox/omarchy/oxy"
  cp -r config/omarchy/oxy/extensions "$sandbox/omarchy/oxy/"
  echo '{}' > "$sandbox/omarchy/oxy.json"
  cfg="$sandbox"
  command -v cygpath >/dev/null 2>&1 && cfg=$(cygpath -w "$sandbox")
  (cd core && XDG_CONFIG_HOME="$cfg" cargo run -q -p oxy -- test --only manifest) \
    || bad "oxy test --only manifest"
  # The same cases through the *engine*, which is the only check that the
  # native ports return what the cases describe — cases.py always runs the
  # script. The repo's bin/ goes on PATH so a native that declines can still
  # reach its script, and the same three stubs cases.py plants (`claude`
  # makes the agent's draft build, `alacritty` lets `terminal` resolve,
  # `oxy-volume` is what the volume sentence routes to). Everything whose
  # data, fixture or compositor is missing here skips rather than fails.
  mkdir -p "$sandbox/fakebin"
  for name in claude alacritty oxy-volume; do
    printf '#!/usr/bin/env bash\necho stub-agent\n' > "$sandbox/fakebin/$name"
    chmod +x "$sandbox/fakebin/$name"
  done
  (cd core && PATH="$(cd .. && pwd)/bin:$sandbox/fakebin:$PATH" XDG_CONFIG_HOME="$cfg" \
     XDG_STATE_HOME="$sandbox/state" XDG_RUNTIME_DIR="$sandbox/run" \
     HOME="$sandbox/home" cargo run -q -p oxy -- test --cases) \
    || bad "oxy test --cases (the native legs)"
  rm -rf "$sandbox"
else
  echo "   cargo not installed — CI runs it; skipping"
fi

say "rust file budget + layering"
# A file is a noun: 800 target, 1200 hard cap, data tables exempt via
# core/.loc-allow. And model/ is the wire vocabulary — no IO may live there.
python3 - <<'PY' || bad "a rust file is over budget, or model/ does IO"
import glob, os, sys
allow = {}
af = "core/.loc-allow"
if os.path.exists(af):
    for line in open(af, encoding="utf-8"):
        line = line.strip()
        if line and not line.startswith("#"):
            allow[line.split()[0]] = True
bad = False
for path in sorted(glob.glob("core/crates/**/*.rs", recursive=True)):
    rel = os.path.relpath(path, "core").replace(os.sep, "/")
    n = sum(1 for _ in open(path, encoding="utf-8"))
    if rel in allow:
        continue
    if n > 1200:
        print(f"{rel}: {n} lines — over the 1200 cap"); bad = True
    elif n > 800:
        print(f"{rel}: {n} lines — over the 800 target")
for path in glob.glob("core/crates/oxy-core/src/model/*.rs"):
    src = open(path, encoding="utf-8").read()
    for banned in ("std::fs", "std::process", "tokio::process"):
        if banned in src:
            print(f"{path}: model/ may not use {banned}"); bad = True
sys.exit(1 if bad else 0)
PY

say "logic tests"
node --test tests/logic.test.mjs || bad "logic tests"

say "behavior tests"
bash tests/behavior.test.sh || bad "behavior tests"

say "extension cases"
python3 tests/cases.py || bad "extension cases"

say "no legacy name"
# This script names the word to search for it, so its own directory is skipped;
# the workflow file in .github does the same. Build outputs are skipped too:
# every artifact under core/target embeds the checkout path, so a clone in a
# directory still called `omacast` would match its own binaries and fail a
# check that is about source files. The same goes for `.worktrees/`: each
# worktree's `.git` file records the path it was created from, and the
# worktrees are other checkouts of this same repo besides.
grep -ri omacast --exclude-dir=.git --exclude-dir=.github --exclude-dir=tests --exclude-dir=target --exclude-dir=.worktrees . && bad "an omacast reference survives"

if ((failed)); then
  printf '\n\033[31m%s\033[0m\n\n' "checks failed"
  exit 1
fi
printf '\n\033[32m%s\033[0m\n\n' "all checks passed"
