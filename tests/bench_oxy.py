#!/usr/bin/env python3
"""Measure what this launcher claims: cold start, per-keystroke latency, RSS.

Everything runs in a throwaway sandbox (`XDG_CONFIG_HOME`, `XDG_STATE_HOME`,
`XDG_RUNTIME_DIR`, `HOME`) with this repo's own extensions, so it never
touches a real daemon, a real state file or a real socket. The queries are
`apps:` prefixes — a native provider, so no external tool has to be
installed for the numbers to mean something.

    python3 tests/bench_oxy.py                    # debug build, 20 keystrokes
    python3 tests/bench_oxy.py --bin core/target/release/oxyd --queries 50
    python3 tests/bench_oxy.py --max-p50-ms 25    # exit 1 if it regresses

What is measured, and why these three:

* **cold start** — spawn to the daemon answering `ping`. It is the number a
  user feels when they press the summon key after a boot.
* **per-keystroke latency** — query written to the daemon to the first
  `results` event coming back, p50 and p95 over N queries. The first paint is
  what the eye judges; a provider that finishes later only fills in rows.
* **RSS** — resident memory of the daemon once it is warm, read from the OS
  rather than guessed. On a platform where it cannot be read, it says so
  instead of printing a zero.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
EXT_SRC = REPO / "config" / "omarchy" / "oxy" / "extensions"


def default_bin() -> Path:
    for profile in ("release", "debug"):
        for name in ("oxyd.exe", "oxyd"):
            p = REPO / "core" / "target" / profile / name
            if p.exists():
                return p
    return REPO / "core" / "target" / "debug" / "oxyd"


def client_bin(daemon: Path) -> Path:
    for name in ("oxy.exe", "oxy"):
        p = daemon.parent / name
        if p.exists():
            return p
    return daemon.parent / "oxy"


def rss_kb(pid: int) -> int | None:
    """Resident memory, or None where this platform does not offer it."""
    try:
        if platform.system() == "Linux":
            with open(f"/proc/{pid}/status", encoding="utf-8") as f:
                for line in f:
                    if line.startswith("VmRSS:"):
                        return int(line.split()[1])
            return None
        if platform.system() == "Darwin":
            out = subprocess.run(
                ["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True
            )
            return int(out.stdout.strip() or 0) or None
        if platform.system() == "Windows":
            import csv
            import io

            out = subprocess.run(
                ["tasklist", "/FI", f"PID eq {pid}", "/FO", "CSV", "/NH"],
                capture_output=True,
                text=True,
            )
            # "oxyd.exe","1234","Console","1","12,345 K" — the memory column
            # carries a thousands separator, so it has to be parsed as CSV
            # rather than split on commas (which read 22,812 K as 812).
            for row in csv.reader(io.StringIO(out.stdout)):
                if row and row[-1].strip().endswith(" K"):
                    return int(row[-1].replace(" K", "").replace(",", "").strip())
            return None
    except Exception:
        return None
    return None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default=None, help="the daemon to measure")
    ap.add_argument("--queries", type=int, default=20)
    ap.add_argument("--max-start-ms", type=float, default=None)
    ap.add_argument("--max-p50-ms", type=float, default=None)
    args = ap.parse_args()

    daemon = Path(args.bin) if args.bin else default_bin()
    if not daemon.exists():
        print(f"no daemon at {daemon} — build it first (cargo build -p oxyd)", file=sys.stderr)
        return 2
    oxy = client_bin(daemon)
    if not oxy.exists():
        print(f"no client at {oxy} — build it too (cargo build -p oxy)", file=sys.stderr)
        return 2

    sandbox = Path(tempfile.mkdtemp(prefix="oxy-bench-"))
    env = os.environ.copy()
    env.update(
        {
            "XDG_CONFIG_HOME": str(sandbox / "config"),
            "XDG_STATE_HOME": str(sandbox / "state"),
            "XDG_RUNTIME_DIR": str(sandbox / "run"),
            "HOME": str(sandbox / "home"),
        }
    )
    (sandbox / "config" / "omarchy" / "oxy").mkdir(parents=True)
    for d in ("state", "run", "home"):
        (sandbox / d).mkdir(parents=True, exist_ok=True)
    shutil.copytree(EXT_SRC, sandbox / "config" / "omarchy" / "oxy" / "extensions")
    (sandbox / "config" / "omarchy" / "oxy.json").write_text("{}", encoding="utf-8")

    started = time.perf_counter()
    daemon_proc = subprocess.Popen(
        [str(daemon)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
    )
    try:
        # ---- cold start: spawn to the first pong
        start_ms = None
        with subprocess.Popen(
            [str(oxy), "send"],
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            errors="replace",
        ) as client:
            assert client.stdin and client.stdout
            deadline = time.time() + 15
            while time.time() < deadline:
                try:
                    client.stdin.write('{"op":"ping"}\n')
                    client.stdin.flush()
                except BrokenPipeError:
                    time.sleep(0.05)
                    continue
                line = client.stdout.readline()
                if '"pong"' in line:
                    start_ms = (time.perf_counter() - started) * 1000
                    break
                time.sleep(0.05)
            if start_ms is None:
                print("the daemon never answered ping", file=sys.stderr)
                return 1

            # ---- per-keystroke latency: query to the first results event
            #
            # The epoch is what keeps this honest: every query bumps it by one
            # and the engine stamps each `results` with the epoch it belongs
            # to. Reading "the next results line" counted a straggler from the
            # previous keystroke as an instant answer — which is exactly the
            # zero this measured before the epoch check.
            latencies: list[float] = []
            for i in range(args.queries):
                query = f"apps:app {i}"
                epoch = i + 1
                t0 = time.perf_counter()
                client.stdin.write(
                    json.dumps({"op": "query", "text": query, "opened": True}) + "\n"
                )
                client.stdin.flush()
                deadline = time.time() + 5
                while time.time() < deadline:
                    line = client.stdout.readline()
                    if not line:
                        break
                    if '"op":"results"' not in line:
                        continue
                    try:
                        event = json.loads(line)
                    except ValueError:
                        continue
                    if event.get("epoch") == epoch:
                        latencies.append((time.perf_counter() - t0) * 1000)
                        break

            # The client streams events until the daemon closes, so it does
            # not exit when its own stdin does — it is a throwaway, so it is
            # ended rather than waited for.
            client.stdin.close()
            client.terminate()
            try:
                client.wait(timeout=5)
            except subprocess.TimeoutExpired:
                client.kill()

        rss = rss_kb(daemon_proc.pid)
    finally:
        daemon_proc.terminate()
        try:
            daemon_proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            daemon_proc.kill()
        shutil.rmtree(sandbox, ignore_errors=True)

    if not latencies:
        print("no results events came back — nothing to report", file=sys.stderr)
        return 1

    p50 = statistics.median(latencies)
    p95 = sorted(latencies)[max(0, int(len(latencies) * 0.95) - 1)]
    mem = f"{rss / 1024:.1f} MB" if rss else "not readable here"

    print(f"daemon      {daemon}")
    print(f"cold start  {start_ms:.0f} ms")
    print(f"keystroke   p50 {p50:.1f} ms · p95 {p95:.1f} ms · {len(latencies)} queries")
    print(f"rss         {mem}")

    failed = False
    if args.max_start_ms is not None and start_ms > args.max_start_ms:
        print(f"cold start over budget ({start_ms:.0f} > {args.max_start_ms:.0f} ms)")
        failed = True
    if args.max_p50_ms is not None and p50 > args.max_p50_ms:
        print(f"p50 over budget ({p50:.1f} > {args.max_p50_ms:.1f} ms)")
        failed = True
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
