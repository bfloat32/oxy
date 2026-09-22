# Ωᵡ₄Y — Rust core (experimental)

The same launcher, with the engine rebuilt in Rust. The card, the views, the
keywords and the settings files are unchanged — what changed is who answers.
The script build asks every extension itself, a process per keystroke, with
debounce and staleness managed in JavaScript. This branch keeps the QML and
replaces everything behind it with `oxyd`: one daemon that owns parsing,
providers, caching, frecency, pins, previews, forms and the `Ctrl+Enter`
stream, speaking one JSON object per line over a local socket.

`plugin/Launcher.qml` stays in the tree as the script-driven reference; the
installed frontend is `plugin/Shell.qml`, a client that sends what is typed
and draws what arrives.

## Install

On an Omarchy system, beside the main install — they coexist. The repo is
private, so raw URLs want a token; cloning with your usual git credentials and
running the script from the checkout does not:

```bash
git clone --depth 1 -b experimental/rust-core \
  https://github.com/bfloat32/oxy.git ~/.local/share/oxy-rs
~/.local/share/oxy-rs/install-rs.sh
```

The script detects it is running from the checkout and installs that directly —
no second clone. A one-liner still works where a token is in scope:

```bash
curl -fsSL -H "Authorization: Bearer $GH_TOKEN" \
  https://raw.githubusercontent.com/bfloat32/oxy/experimental/rust-core/install-rs.sh | bash
```

That clones this branch to `~/.local/share/oxy-rs` (through your git
credentials), builds `oxyd` and `oxy` with `cargo build --release` into
`~/.local/bin`, registers a second plugin as `oma.oxyrs`, and binds it to
`Super+R`. The stock install keeps `oma.oxy`, `Super+K`, and every `oxy-*`
command exactly as they were.

Shared on purpose: the extension manifests, `oxy.json`, pins, frecency and
history are the same files in the same formats, so both launchers behave
identically and switching between them loses nothing. Whoever linked a shared
file first keeps owning it; `--uninstall` hands anything this script owned
back to the main checkout rather than breaking it.

`cargo` is the one new dependency — the installer offers `pacman -S rust` when
it is missing, and picks up a `rustup` toolchain from `~/.cargo/env` on its
own.

- Re-run the same line to update: it pulls, rebuilds, relinks.
- `install-rs.sh --uninstall` removes only what it put in; `--purge` also
  deletes the clone; `--fresh` forces a clean slate.
- Keybind: edit the one line at the top of `hypr/keys-rs.lua`.

## What runs where

| Piece | What | Lives at |
|---|---|---|
| `oxyd` | the daemon: engine + socket | `~/.local/bin/oxyd` |
| `oxy` | the CLI client | `~/.local/bin/oxy` |
| `plugin/Shell.qml` | the frontend | `plugins/oma.oxyrs/` |
| socket | JSON lines | `$XDG_RUNTIME_DIR/oxyd.sock` |
| `plugin/Launcher.qml` | the reference frontend | unused here |

The daemon does not need to be started by hand — the frontend spawns it on
the first summon and reconnects if it goes away. `oxy send` is the raw
client: JSON commands on stdin, events on stdout, the same wire the QML
speaks (`docs/PROTOCOL.md` has the whole protocol).

```bash
oxy query "cal:"        # ask the daemon; falls back to an in-process engine
oxy query --local "2+2" # skip the daemon entirely
oxy test                # every extension's testQuery through the engine
oxy test file           # one extension's testQuery
oxy test --cases        # the shipped *.cases.json assertions, engine-side
oxy test --cases file   # one extension's cases
oxy extensions          # the loaded registry
printf '{"op":"ping"}\n' | oxy send   # talk to the socket directly
```

## What is native

Extensions that declare `"native": "<name>"` are answered by a provider
compiled into the daemon first — the built-ins apps, calc, commands,
quicklinks and web, plus alarm, branch, bri, bt, cal, calchist, ch, ci,
date, def, docker, emoji, file, gh, git, herdr, img, issue, kill, note,
omarchy, pass, pr, radio, recent, repo, shortcuts, snip, spotify,
spotify-library, ssh, stash, sys, theme, tz, unit, vol, wifi and win —
with their declared `search` or `socket` kept as the fallback when the
native provider declines. The four git-family providers share
`vcs/repos/` — repo discovery, the `--resolve` port, and the per-repo
state cache — so `git:omarchy` answers which repo you meant without a
`git status` per candidate. The four GitHub keywords share `vcs/gh/`: the
offline fast path still draws a row from `gh:owner/repo`, a pasted URL or
`owner/repo#123` before any request, and the GraphQL panels are warmed in
the background the way the script's `setsid` warming was. The media
providers share `media/` — radio-browser search and the mpv IPC socket,
the MPRIS/`busctl` now-playing fields, and the Spotify catalogue with its
token-refresh lock. `tz` resolves zone names and DST math in-process
through `jiff`, so `tz:9am tokyo in london` costs no `date` processes at
all. `cal`'s natural-language leg resolves through the same `date` parser
in-process, so queries like `cal:christmas` need no script at all.
The `do:` agent's socket server is a Rust binary too —
`core/crates/oxy/src/bin/oxy-agent.rs` — speaking the same
`{epoch, query}` → `{epoch, rows}` protocol to the same
`oxy-agent.sock`, so `ResultAgent.qml` and the engine see no difference.
Every shipped extension is Rust-answered; the `bin/` scripts remain as
the fallback path and the contract the ports were written against, so any
extension you wrote keeps working unchanged. A provider that falls back
loses only the speed, never the answer.

## Developing

```bash
cd core
cargo check --workspace     # fast syntax/type pass
cargo test --workspace      # the unit suite
cargo build --release -p oxyd -p oxy
```

`core/` is three crates: `oxy-core` (the engine — ports of the Query,
Extensions, Rank, Score, Cache, Settings and Frecency modules plus the native
providers), `oxyd` (the socket plumbing around it) and `oxy` (the CLI). The
workspace builds and tests on Windows too — the daemon answers over the
`oxyd` named pipe there — but the target is Omarchy, and the QML half is
verified on a running shell, not in CI.

## Caveats

- This is an experimental branch. The engine passes its tests and drives the
  wire correctly; the frontend is review-verified against every view's
  facade, but real-shell mileage is exactly what this branch is for.
- The daemon owns the state transitions. If the launcher looks dead, the
  question to ask is whether `oxyd` is up: `printf '{"op":"ping"}\n' | oxy send`.
- Crashes are isolated by design: the engine cannot take the shell down with
  it — the launcher fails, the bar stays.
- On Windows two natives answer where the scripts cannot: `kill:` and `sys:`
  read through `sysinfo` instead of `ps`/`free`, so a side-by-side against the
  script leg there is a deliberate extension, not a parity break.
