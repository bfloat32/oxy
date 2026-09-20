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
CASE_KEYS = {"why": str, "query": str, "minRows": int, "maxRows": int,
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

say "rust core"
# The same steps the workflow's rust job runs — skipped where cargo is not
# installed rather than failed, the way shellcheck degrades.
if command -v cargo >/dev/null 2>&1; then
  (cd core && cargo check --workspace) || bad "cargo check"
  (cd core && cargo test --workspace) || bad "cargo test"
else
  echo "   cargo not installed — CI runs it; skipping"
fi

say "logic tests"
node --test tests/logic.test.mjs || bad "logic tests"

say "behavior tests"
bash tests/behavior.test.sh || bad "behavior tests"

say "extension cases"
python3 tests/cases.py || bad "extension cases"

say "no legacy name"
# This script names the word to search for it, so its own directory is skipped;
# the workflow file in .github does the same.
grep -ri omacast --exclude-dir=.git --exclude-dir=.github --exclude-dir=tests . && bad "an omacast reference survives"

if ((failed)); then
  printf '\n\033[31m%s\033[0m\n\n' "checks failed"
  exit 1
fi
printf '\n\033[32m%s\033[0m\n\n' "all checks passed"
