#!/usr/bin/env python3
"""Runs the shipped <name>.cases.json assertions against their extensions.

`bo test` is the reference implementation, and it lives outside this
repository. This runner reads the same case files the same way: the first
word of an extension's `search` is called with the case's `query`, the rows
it prints are parsed, and each assertion is checked against the row the
case names.

    tests/cases.py              every extension with a cases file
    tests/cases.py repo         one extension
    tests/cases.py --verbose    every assertion, not only the failures

Extensions whose `when` fails, whose command is not on PATH, or which have
no `search` are skipped rather than failed: a case file cannot run where
its extension cannot.

The git-family cases are written against a repository named `oxy-fixture`
with a `login` branch two ahead and one behind main, two stashes (the older
one named "half a refactor" and containing an untracked `sketch.txt`), and
a dirty tree. This file builds that fixture under a sandbox HOME so nothing
the tests do touches the machine's real repos, caches or shell.json.
"""

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
EXT_DIR = REPO / "config" / "omarchy" / "oxy" / "extensions"
BIN = REPO / "bin"

passed = failed = skipped = 0


def ok(msg):
    global passed
    passed += 1
    print(f"  ok    {msg}")


def broke(msg, why=""):
    global failed
    failed += 1
    print(f"  FAIL  {msg}")
    if why:
        print(f"        ({why})")


def skip(msg, why):
    global skipped
    skipped += 1
    print(f"  skip  {msg} ({why})")


def run(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True,
                          encoding="utf-8", errors="replace", timeout=30, **kw)


def git(*args, cwd=None):
    run(["git", *args], cwd=cwd, check=True)


# ---------------------------------------------------------------- fixture


def build_fixture(home: Path, env: dict):
    """Two repos under <home>/repos: `oxy-fixture` in the shape the git cases
    describe, and `omarchy` for the repo: cases (its name starts with 'omar',
    which is what they query)."""
    if not shutil.which("git"):
        return
    repos = home / "repos"
    repos.mkdir(parents=True, exist_ok=True)

    fx = repos / "oxy-fixture"
    git("init", "-q", "-b", "main", str(fx))
    git("config", "user.email", "t@t", cwd=fx)
    git("config", "user.name", "t", cwd=fx)
    git("remote", "add", "origin",
        "https://github.com/example/oxy-fixture.git", cwd=fx)
    (fx / "app.py").write_text("print('hi')\n")
    git("add", "app.py", cwd=fx)
    git("commit", "-qm", "init", cwd=fx)

    # login: two commits on top of init; main then moves one ahead, so login
    # reads trunkAhead=2 / trunkBehind=1 against the trunk.
    git("checkout", "-qb", "login", cwd=fx)
    for i in (1, 2):
        (fx / f"feat{i}.py").write_text(f"# {i}\n")
        git("add", f"feat{i}.py", cwd=fx)
        git("commit", "-qm", f"login work {i}", cwd=fx)
    git("checkout", "-q", "main", cwd=fx)
    (fx / "README.md").write_text("readme\n")
    git("add", "README.md", cwd=fx)
    git("commit", "-qm", "document", cwd=fx)
    git("branch", "other", cwd=fx)

    # Stash order is newest-first: the older one carries sketch.txt and is
    # named "half a refactor"; the newer one is "trying the other approach".
    (fx / "app.py").write_text("print('v2')\n")
    (fx / "sketch.txt").write_text("half drawn\n")
    git("stash", "push", "-qum", "half a refactor", cwd=fx)
    (fx / "app.py").write_text("print('v3')\n")
    git("stash", "push", "-qm", "trying the other approach", cwd=fx)

    # Leave the tree dirty: the panel must answer Enter with a diff, and the
    # branch view must warn a switch could be refused.
    (fx / "app.py").write_text("print('v4')\n")

    om = repos / "omarchy"
    git("init", "-q", "-b", "main", str(om))
    git("config", "user.email", "t@t", cwd=om)
    git("config", "user.name", "t", cwd=om)
    git("remote", "add", "origin", "https://github.com/example/omarchy.git",
        cwd=om)
    (om / "f").write_text("x\n")
    git("add", "f", cwd=om)
    git("commit", "-qm", "init", cwd=om)

    # The git and stash cases name `gum`: a clean repo holding no stashes.
    gum = repos / "gum"
    git("init", "-q", "-b", "main", str(gum))
    git("config", "user.email", "t@t", cwd=gum)
    git("config", "user.name", "t", cwd=gum)
    (gum / "f").write_text("x\n")
    git("add", "f", cwd=gum)
    git("commit", "-qm", "init", cwd=gum)

    env["OXY_REPO_ROOTS"] = str(repos)
    env["OXY_REPO"] = str(fx)


def stub_agent(home: Path, env: dict):
    """The `do:` cases describe the draft a configured agent produces. No
    real agent CLI exists in a test sandbox, so a `claude` that exists and
    answers is placed on PATH — search never runs it, it only has to be
    findable for the draft to build."""
    fake = home / "fakebin"
    fake.mkdir(exist_ok=True)
    # `claude` makes the draft build; `alacritty` lets `terminal` resolve;
    # `oxy-volume` is the script the volume sentence routes to.
    # The .cmd twins are for Windows, where shutil.which only sees PATHEXT.
    for name in ("claude", "alacritty", "oxy-volume"):
        for suffix in ("", ".cmd"):
            p = fake / (name + suffix)
            p.write_text("#!/usr/bin/env bash\necho stub-agent\n")
            p.chmod(0o755)
    env["PATH"] = str(fake) + os.pathsep + env["PATH"]


# ------------------------------------------------------------------ cases


def rows_from(stdout):
    """One extension answers as a JSON array or one object per line; a few
    prepend a chatter line, which is skipped like parseRows does."""
    stdout = (stdout or "").strip()
    if not stdout:
        return []
    try:
        data = json.loads(stdout)
        return data if isinstance(data, list) else [data]
    except Exception:
        pass
    rows = []
    for line in stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            rows.append(json.loads(line))
        except Exception:
            pass
    return rows


def present(v):
    """A field is absent when missing, "", [] or {}. 0 and False are real."""
    return v is not None and v != "" and v != [] and v != {}


def text(v):
    """matches patterns are written against Python's str(): a dict reads
    {'changed': 3}, a bool True, a number its digits."""
    return v if isinstance(v, str) else str(v)


def check_case(case, rows, label):
    """Returns a list of (problem, why) — empty means held."""
    problems = []
    why = case.get("why", "")
    n = len(rows)
    if "minRows" in case and n < case["minRows"]:
        problems.append((f"{n} rows, wanted at least {case['minRows']}", why))
        return problems
    if "maxRows" in case and n > case["maxRows"]:
        problems.append((f"{n} rows, wanted at most {case['maxRows']}", why))
        return problems

    idx = case.get("row", 0)
    # An assertion on a row is only checked when there is a row — that is the
    # documented contract, and why minRows exists.
    if idx >= n:
        return problems
    row = rows[idx]

    if "view" in case and row.get("view") != case["view"]:
        problems.append((f"row {idx} view is {row.get('view')!r}, "
                         f"wanted {case['view']!r}", why))
        return problems
    for f in case.get("fields", []):
        if not present(row.get(f)):
            problems.append((f"row {idx} {f} is empty", why))
    for f in case.get("absent", []):
        if present(row.get(f)):
            problems.append((f"row {idx} {f} is {row.get(f)!r}, wanted absent", why))
    for f, cap in case.get("atMost", {}).items():
        v = row.get(f)
        if v is not None and len(v) > cap:
            problems.append((f"row {idx} {f} has {len(v)}, wanted at most {cap}", why))
    for f, pat in case.get("matches", {}).items():
        v = text(row.get(f))
        if not re.search(pat, v):
            problems.append((f"row {idx} {f} is {v[:120]!r}, wanted /{pat}/", why))
    return problems


def preflight(name, env):
    """The cases describe the extension as it behaves where its data lives.
    Some data is a service: the dictionary, or GitHub's search API. With no
    route to it the honest answer is a skip, not thirty identical failures."""
    checks = {
        "define": "curl -sf --max-time 6 -o /dev/null "
                  "https://api.dictionaryapi.dev/api/v2/entries/en/ping",
        "issue": "gh auth status",
        "pr": "gh auth status",
        # Names like saopaulo resolve through the IANA list, which lives in
        # tzdata — not in the shipped label table.
        "timezone": "timedatectl list-timezones >/dev/null 2>&1 "
                    "|| test -d /usr/share/zoneinfo",
    }
    probe = checks.get(name)
    if probe and run(["bash", "-c", probe], env=env).returncode != 0:
        return False
    return True


def run_extension(ext_file: Path, env: dict, verbose: bool):
    name = ext_file.stem
    cases_file = ext_file.with_name(ext_file.stem + ".cases.json")
    if not cases_file.exists():
        return

    try:
        ext = json.loads(ext_file.read_text(encoding="utf-8"))
        cases = json.loads(cases_file.read_text(encoding="utf-8"))
    except Exception as e:
        broke(f"{name} cases file does not parse: {e}")
        return

    when = (ext.get("when") or "").strip()
    if when:
        r = run(["bash", "-c", when], env=env)
        if r.returncode != 0:
            skip(f"{name} cases", "when fails here")
            return

    search = (ext.get("search") or "").strip()
    if not search:
        skip(f"{name} cases", "no search command")
        return
    cmd = search.split()[0]
    # Extensionless bash scripts are not found by shutil.which nor spawned
    # directly on Windows, so everything goes through bash -c.
    if run(["bash", "-c", f"command -v {cmd}"], env=env).returncode != 0:
        skip(f"{name} cases", f"{cmd} not on PATH")
        return
    if not preflight(name, env):
        skip(f"{name} cases", "its data service is unreachable here")
        return

    held = 0
    problems = []
    for case in cases:
        before = len(problems)
        query = str(case.get("query", ""))
        # MSYS rewrites a leading-slash argument into a Windows path; a
        # doubled slash collapses back to one in the child, so `/policy`
        # reaches the extension the way it was typed.
        if os.name == "nt" and query.startswith("/"):
            query = "/" + query
        case_env = dict(env, OXY_CASE_QUERY=query)
        try:
            r = run(["bash", "-c", f'exec {cmd} "$OXY_CASE_QUERY"'],
                    env=case_env)
        except subprocess.TimeoutExpired:
            problems.append((f"query {case.get('query')!r} timed out",
                             case.get("why", "")))
            continue
        rows = rows_from(r.stdout)
        problems.extend(check_case(case, rows, name))
        if len(problems) == before:
            held += 1
    if problems:
        broke(f"{name} cases", f"{len(problems)} broke")
        for prob, why in problems:
            print(f"        {prob}")
            if why:
                print(f"        ({why})")
    else:
        ok(f"{name} cases  {held} held")


def main():
    global verbose
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass
    args = sys.argv[1:]
    verbose = "--verbose" in args or "-v" in args
    args = [a for a in args if not a.startswith("-")]

    sandbox = Path(tempfile.mkdtemp(prefix="oxy-cases-"))
    env = os.environ.copy()
    env["PATH"] = str(BIN) + os.pathsep + env["PATH"]
    env["HOME"] = str(sandbox)
    env["XDG_CACHE_HOME"] = str(sandbox / "cache")
    env["XDG_STATE_HOME"] = str(sandbox / "state")
    env["XDG_CONFIG_HOME"] = str(sandbox / "config")
    try:
        build_fixture(sandbox, env)
    except Exception as e:
        print(f"fixture build failed: {e} — git-family cases will fail")
    stub_agent(sandbox, env)

    only = args[0] if args else None
    any_cases = False
    for ext_file in sorted(EXT_DIR.glob("*.json")):
        if ext_file.stem.endswith(".cases"):
            continue
        if only and ext_file.stem != only:
            continue
        before = (passed, failed, skipped)
        run_extension(ext_file, env, verbose)
        if (passed, failed, skipped) != before:
            any_cases = True

    if not any_cases:
        print("no case files ran")
    print(f"\n{passed} held, {failed} broke, {skipped} skipped")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
