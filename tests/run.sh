#!/bin/bash
# The local check suite: everything CI runs, runnable before pushing.
#
#   tests/run.sh
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

say "extension JSON"
python3 - <<'PY' || bad "a .json file does not parse"
import glob, json, sys
bad = []
for path in glob.glob("config/**/*.json", recursive=True):
    try:
        json.load(open(path, encoding="utf-8"))
    except Exception as e:
        print(path, e)
        bad.append(path)
sys.exit(1 if bad else 0)
PY

say "logic tests"
node --test tests/logic.test.mjs || bad "logic tests"

say "no legacy name"
# This script names the word to search for it, so its own directory is skipped;
# the workflow file in .github does the same.
grep -ri omacast --exclude-dir=.git --exclude-dir=.github --exclude-dir=tests . && bad "an omacast reference survives"

if ((failed)); then
  printf '\n\033[31m%s\033[0m\n\n' "checks failed"
  exit 1
fi
printf '\n\033[32m%s\033[0m\n\n' "all checks passed"
