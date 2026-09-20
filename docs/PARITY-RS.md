# Parity audit: the script build vs the Rust core

What is still missing between `Launcher.qml` + `ExtensionProvider.qml` + the
`.js` modules (the script build) and `oxyd` + `Shell.qml` + `oxy-core` (this
branch), what it costs, and what to port next.

Method: every module on both sides was read against its counterpart; the
engine was then driven directly (`oxyd` + `oxy send`/`oxy query`, and
`oxy test --cases`) from this checkout, so the numbers below are measured
rather than inferred. Line references are to the tree at the commit this file
was written against.

> **Status (post-fix pass):** every P0 and the cheap P1s are resolved and
> re-verified live against the daemon — `refreshMs` fires (`kill` re-asked
> 1.5s after its answer, `refresh:true`), `oxy.json` is watched (an edit
> re-logs `ext.load` in ~400ms and re-asks the open query), the log carries
> `sid` and rotates at 1MB, the login env is captured (`env` event, ~90 vars)
> and replayed onto every spawned command, `run:` browses all 24 commands and
> `apps:` browses, app icons resolve through the XDG index, publishes
> coalesce (~4 `rebuild` events per keystroke where 47 were measured), `bo`
> summons `${OXY_PLUGIN_ID:-oma.oxy}`, `frecency_prune` runs on every save,
> `engines` merge by id, the fuzzy band order matches `Score.js`, actions
> exec after `close`, `?` shows the Keywords chip, notices re-ask on the poll
> cadence, and the paste row carries its link glyph. Still open: preview
> revert (P1-15), the porting backlog, and the P2 harness items below.

## Verdict

The engine port is faithful where it was finished: parsing, filters and
sigils (`query.rs`), row building and passthrough (`row.rs`), tiers and merge
(`rank.rs`), cache with TTL + LRU + stale-while-revalidate (`cache.rs`),
availability (`availability.rs`), frecency/pins/recents maths (`state.rs`),
the worker state machine — debounce, epochs, timeout, stale rules, socket
pushes, native-then-fallback (`worker.rs`) — and the inline answerers (help,
recents, paste, settings, actions). The native ports that exist are careful,
case-shaped ports, not approximations.

What is missing is concentrated in four places:

1. **Engine plumbing that was documented but never wired** — `refreshMs`,
   the event log, the settings watcher, the login environment. These are
   small, self-contained fixes with outsized user-visible effect.
2. **One wire-level inefficiency** — the engine publishes once per provider
   answer instead of once per query pass, which the QML version explicitly
   engineered away.
3. **Two dozen extensions still answering through their scripts** — parity
   holds (the scripts are unchanged and linked), but the "less resource, faster
   responses" goal is only met for the 15 that are native.
4. **Integration edges** — app icons, the `bo:` return summon, bare `apps:`
   and `run:`, and the case/test harness.

Nothing found is architectural. The daemon owns the same state machine the
QML owned; the gaps are wiring, not design.

---

## P0 — documented behavior that is broken or absent

### 1. `refreshMs` never fires

`WorkerCmd::Showing` is declared and handled
(`oxy-core/src/provider/worker.rs:36`, `:535`) but **never sent by the
engine**, so the worker's `showing` flag stays `false` forever and
`arm_refresh!()` (`worker.rs:268`) can never arm. Every `refreshMs` in the
tree is dead:

| extension | refreshMs | effect |
|---|---|---|
| `gh`, `pr` | 900 | rows never refresh while on screen |
| `kill` | 1500 | CPU/cost readings freeze |
| `docker`, `herdr` | 2000 | container/agent state freezes |
| `alarm` | 15000 | countdown never ticks |
| `agent` (`do:`) | 600 | card relies on socket pushes, so mostly masked |

`docs/EXTENSIONS.md` documents `refreshMs` as working, and the QML's
`armRefresh()` (`ExtensionProvider.qml:382`) is the reference. Fix: in
`Engine::publish` (`engine.rs:877`), send `WorkerCmd::Showing(true)` to every
worker whose id appears in the merged `self.rows`, and `Showing(false)` to the
rest (or to all on `on_query`, then true for the visible ones). One `Arc<str>`
lookup per publish; no extra work for providers that never set `refreshMs`.

### 2. `oxy.json` is not watched

The script build watches the file and re-reads it on every edit
(`Launcher.qml:2192`; README: "watched, so an edit takes effect on the next
keystroke"). The daemon loads settings at start and on `EngineCmd::Reload`
(`engine.rs:1504`), and the only thing that triggers a reload is the
extensions-dir watcher (`oxyd/src/main.rs:258`). So on this branch:

- a quicklink added to `oxy.json` does not appear,
- `engines`, `engineActions`, `defaultEngine`, `recents`, `frecency`,
  `askProvider(s)` and `"extensions": {...}` edits do nothing,
- `/reload` or a daemon restart is the only way in.

Fix: watch `dirs::settings_file()` in the same signature poll (or a second
one) and send `EngineCmd::Reload` on change — `on_reload` already re-reads
settings and re-emits the registry. Note the write path must not fight it:
`on_save_settings` (`engine.rs:1464`) rewrites the file, so the watcher needs
the same "signature changed" guard it already has (content hash or
size+mtime) to avoid a reload loop — a reload there is harmless anyway.

### 3. The event log is a fraction of the documented one

`Logger.qml` wrote `{ts, sid, ev, …}` with a per-shell `sid`, a 1MB rotation
into `.old`, drop accounting, and the event vocabulary `query`, `prov.start`,
`prov.done`, `prov.fail`, `prov.timeout`, `prov.stale`, `prov.drop`,
`sock.bad`, `avail`, `act`, `action`, `drop`, `rebuild`, `env`. The daemon
writes `{ts, ev, …}` only (`oxyd/src/main.rs:84`) and the engine emits
`drop`, `act`, `action`, `ext.bad` plus `prov.stale`/`prov.drop` from the
worker — no `sid`, no `prov.start`/`prov.done`/`prov.fail`/`prov.timeout`/
`sock.bad`/`avail`, no `ms` durations, and **no rotation**: `append_log`
grows `oxy-log.jsonl` forever, against README's "never past 2MB".

This is the diagnostic surface the README tells people to hand to an agent
("it can tell a slow extension from a dead one"), so it should come back
whole. Fix: add `sid` (one per daemon, e.g. `Date.now().toString(36)`
equivalent) in `log_line`; emit `prov.start`/`prov.done`/`prov.fail` around
the run in `worker.rs` (the timings already exist as `Instant`s); emit
`sock.bad` where `socket.rs` drops a garbage line; rotate in `append_log`
when `size > 1MB` → rename to `.old`.

### 4. The login environment is not captured

Every provider command runs as `bash -c` under the daemon's own environment
(`provider/process.rs:17`), and the daemon is spawned by the QML shell
(`Shell.qml:218`), so it inherits the *session's* environment — not the login
shell's. The script build solved exactly this: one `bash -lc env -0` at
startup, replayed onto a bare `bash -c` for every query
(`Launcher.qml:177-206`), so profile exports (mise/nix/cargo PATH entries,
`OXY_REPO_ROOTS`, anything a dotfile exports) reach the scripts.

Consequence on this branch: any extension that depends on a profile-provided
binary or variable silently answers nothing when the daemon was started by
the shell rather than from a terminal. `repo:` is the documented case
(`$OXY_REPO_ROOTS`), but a mise/nix user loses far more.

Fix: capture `bash -lc env -0` once in `Engine::start` (or `oxyd` main),
parse the `K=V` entries with the same filter the QML used, and have
`process::run`/`check`/`spawn_stream`/`run_detached` pass that environment
(`Command::envs`) instead of inheriting. Keep the current behaviour as the
fallback when the probe fails.

### 5. Bare `apps:` and `run:` answer nothing, and `run:` is capped

The built-ins are synthesized with `min_chars: 1` (`engine.rs:1721-1791`).
In the script build, `apps:` and `run:` with nothing after them are *browse
modes*: `queryApps`/`queryCommands` only require a scope, an empty argument
scores 0 and every entry matches, so `apps:` lists the first 20 apps
alphabetically and `run:` lists all 24 commands. Here the worker drops them
on `arg.chars().count() < ext.min_chars` (`worker.rs:425`). Measured:
`oxy send` for `run:` returns 0 rows.

Two more in the same area:

- `commands` is synthesized with `max_rows: 20` while `Commands.COMMANDS` has
  24 entries (`native/commands.rs`, `plugin/Commands.js`), so the last four
  are unreachable; the script build caps only at the merge limit of 60.
- `quicklinks` is synthesized with `max_rows: 8`; the script build has no
  per-provider cap.

Fix: `min_chars: 0` for `apps`/`commands`/`quicklinks`, `max_rows: 60` for
`commands` and `quicklinks` (the merge limit already trims the final list).
`web` keeps `min_chars: 1` — a bare `web:` answers nothing in both builds.

### 6. App icons are unresolved names

`native/apps.rs:107` sets `icon_source` to the raw `Icon=` value from the
.desktop file (`"firefox"`, `"org.gnome.Terminal"`). The script build passed
it through the host app library's `iconSource()`, which resolves a name to a
file URL (`Quickshell.iconPath`, or the fallback's own icon index,
`AppLibraryFallback.qml:93`). The views use the field as an `Image.source`
(`ResultList.qml:164`, `ResultGrid.qml:146`, `ResultCards.qml:69`), so a bare
name resolves to nothing: **no app icons in any view**, and the icon slot
still takes its space.

Fix (either side, pick one):
- daemon: resolve names to `file://` paths with an icon-theme walk
  (`~/.icons`, `~/.local/share/icons`, `$XDG_DATA_DIRS/icons`, pixmaps, SVG
  before PNG) — the fallback's `iconScanCommand` is the reference; or
- frontend: `Shell.qml` exposes `iconSource(row)` that resolves a
  non-URL/non-path value through `Quickshell.iconPath` / the app library
  before the views read it, and the three views call it.

The daemon route is better for the wire (rows stay self-contained) and gives
the CLI usable output too.

### 7. One results event per provider answer

The QML version collected answers inside a query pass and rebuilt once
(`Launcher.qml:96`, `:626-670` — "Fifty providers meant fifty of those for
one typed character, and only the last one was ever drawn"). The engine
publishes on *every* worker message (`engine.rs:840-871` → `publish`).
Measured on this checkout: **47 `results` events for one keystroke**, each a
full merge + serialize + broadcast to every client, and the frontend re-tags
sources, re-measures the chip column and re-sorts the list for each one.

Fix: coalesce. In `Engine::run` (`engine.rs:347`), when a `WorkerMsg` arrives,
drain `worker_rx` with `try_recv()` into a batch (bounded, e.g. 256), apply
them all, and publish once; or set a `dirty` flag and publish at the end of
the select loop with a `yield_now`. Late answers still land individually,
which is the behaviour the design wants; the synchronous burst becomes one
event.

### 8. `bo:` unit toggles come back to the wrong launcher

`bin/oxy-bo` hardcodes `PLUGIN_ID="oma.oxy"` (`bin/oxy-bo:121`) and summons
that id back after a plugin toggle. The rust install registers `oma.oxyrs`
(`install-rs.sh:41`, manifest id rewritten at `:545`), so on a machine with
both builds the marketplace toggle returns to the *script* launcher; on a
rust-only machine nothing comes back and `summon_back` times out after its
20s budget, ending in a "the launcher did not come back" notification.

Fix: make the id discoverable rather than hardcoded — the cleanest is for
`install-rs.sh` to write the id into a file the script reads (or export
`OXY_PLUGIN_ID` into the extension's environment via the settings prefix
mechanism), with `oma.oxy` as the fallback. `wait_gone` should look at the
same id.

### 9. The frecency file is never pruned

`state::frecency_prune` (`state.rs:96`) exists and is never called; the QML
pruned on every debounced save (`Launcher.qml:2156`). The file therefore keeps
every key ever launched, including ones decayed to zero, which contradicts
the "drop what has decayed to nothing" contract and grows without bound.

Fix: call `frecency_prune(&mut self.state.frecency, now_ms())` in
`Engine::save_state` (or on record). Cheap; `decayed()` is a `powf` per key.

---

## P1 — behaviour that differs in visible ways

### 10. `engines` replaces instead of merging by id

`Settings::merge` overwrites the whole list when the user's `engines` parses
non-empty (`settings.rs:240-258`); `Settings.js:133-147` merges the user's
entries **over** the built-ins by id, which is what the README documents
("adding one does not mean restating Google, DuckDuckGo, …"). A user who adds
one engine on this branch loses the other five.

Fix: port the merge — index the defaults by id, replace matches, append the
rest.

### 11. Fuzzy band order differs from `Score.js`

`Score.js:104-110` checks, in order: name prefix → **id prefix** → name infix
→ id infix. `score.rs:129-140` checks `name.find()` and returns on infix
*before* consulting the id, so an entry whose name contains the query and
whose id starts with it scores 8000-band here and 9500-band in the script
build. Same tier in most cases, different `local`, so ordering can differ.
Also `name.len()` counts bytes here vs UTF-16 units there, which only matters
for non-ASCII names.

Fix: restructure `fuzzy` to test `name.starts_with`, then `id.starts_with`,
then the infix cases — a ten-line change.

### 12. Rows run before the overlay is down

`Launcher.qml:1637-1642` dismisses *before* running the row's action, on
purpose: "Launching while an exclusive-focus layer surface is still mapped
puts the new window behind it, and Omarchy's launch OSD would render
underneath this overlay." The engine runs `run_detached(&row.exec)` and then
emits `Close` (`engine.rs:1130-1133`), so the new process starts while the
overlay is still mapped, and the frontend hides only when the event arrives.
Fix: emit `Close` first, then spawn (the socket write is already ordered);
the process start is then at worst one frame behind the unmap, matching the
QML's `Qt.callLater`.

### 13. `?` loses the "Keywords" chip

`scope_label` (`engine.rs:972`) has no help-mode case; the QML returned
"Keywords" whenever `helpMode` was set (`Launcher.qml:288`). One line in
`scope_label` (or in `Shell.qml`'s `activeView`/chip binding, which already
has `helpMode` on the wire).

### 14. `notify()` no longer re-asks

`Launcher.qml:1248` ran a 4-pass `requery` after a notice; `Shell.qml:99`
only shows the text. The engine re-asks for the actions that change state
(`/clear`, `/reload`, `savesettings`), so this only matters for notices from
rows/actions whose effect lands later — `Ctrl+C`'s "Copied" is the visible
one. Low priority; wire it by sending `{op:"query"}` with the poll cadence if
it is wanted.

### 15. Preview revert is the last row's, not the first's

`Launcher.qml:565-588` remembers the *first* `revertExec` seen and uses it
when leaving; the engine stores each row's own revert and runs the previous
one on every move (`engine.rs:1301-1324`). For `theme:` (one shared revert
command) the result is identical; for a provider whose `revertExec` differs
per row it would restore the wrong state. Worth aligning when the porting
wave reaches the next previewing extension.

### 16. Minor wire/doc drift

- `answerError` says "Check askProviders in oxy.json" (`engine.rs:1390`)
  where the QML said `ask.command`; the Rust text is the correct one, so
  just note it in the changelog.
- `EngineCmd::Set`/`CommitPreview` are dead wire surface: `ResultSlider`
  runs `setExec` itself (`ResultSlider.qml:99`) and Shell never sends
  `commitpreview`. Harmless, but `docs/PROTOCOL.md` should say the frontend
  owns the slider's exec.
- `paste` row's glyph is empty where the QML had one
  (`engine.rs:614` vs `Launcher.qml:1053`).
- `Shell.qml`'s `knownViews`/`activeView` and `Launcher.qml`'s agree, but the
  list is still hand-written in both — a view added on one side is invisible
  on the other.

---

## P2 — verification, tests, hygiene

- **Native ports without case coverage.** 12 of the 15 native providers have
  no `.cases.json`: `apps`, `calc`, `calchist`, `ch`, `commands`, `file`,
  `kill`, `quicklinks`, `recent`, `ssh`, `sys`, `web`. The repo's own doctrine
  ("write one for the thing that would break quietly") applies to a port as
  much as to a script, and these are exactly the rows the views read by field
  name. `calendar`, `date` and `emoji` have cases — the first two pass here
  (`calendar` 30 held, `date` 1 held over three runs); `emoji`'s data file
  only exists on an Omarchy install.
- **`tests/run.sh` does not run the Rust workspace.** README-RS tells you to
  `cargo test --workspace` separately; the one command CI runs
  (`.github/workflows/check.yml`) has a `rust` job, but the workflow only
  triggers on pushes to `main` and on PRs — pushes to
  `experimental/rust-core` are unchecked. Add the branch to the push trigger
  and a `cargo` step to `tests/run.sh` for local parity.
- **`oxy test` has no manifest/answer/actions layers.** `bo test`'s four
  checks (manifest shape, command on PATH, rows parse and validate, actions
  resolve) are only partly reproduced by `oxy test` (testQuery rows) and
  `oxy test --cases`. A `--only manifest` pass would catch a JSON file that
  names a `view` or `tier` the launcher does not know — the check that would
  have caught a typo'd `native` name silently falling back.
- **Docs drift.** `docs/EXTENSIONS.md` documents `refreshMs` and `cacheMs`
  as working on this branch (cacheMs does; refreshMs does not until P0-1).
  `docs/PROTOCOL.md` does not mention `remember`, which is Rust-only and
  documented in EXTENSIONS.md.
- **Windows build.** `cargo check/test` pass here, and the daemon runs over
  a named pipe; extension sockets are Unix-only by design
  (`provider/socket.rs:33`). Nothing to fix — just keep it compiling, which
  the CI job does.

---

## Porting backlog: the 31 script extensions

Parity does not require porting these — they run unchanged, and the
native-first-then-fallback contract means a port that declines is never
worse than today. The goal is per-keystroke cost and response time, so the
order below is by what a keystroke actually pays.

| wave | extensions | why first |
|---|---|---|
| 1. local state, one spawn per keystroke | `vol`, `bri`, `win`, `bt`, `wifi`, `theme`, `alarm` | `pactl`/`brightnessctl`/`hyprctl`/`bluetoothctl`/`nmcli` per keystroke; sliders and grids that redraw |
| 2. git family | `repo`, `git`, `branch`, `stash` | several `git` spawns per query, plus a cached repo walk |
| 3. GitHub family | `gh`, `pr`, `issue`, `ci` | network + `gh` + `jq`; refreshMs makes them worse until P0-1 lands |
| 4. session/system views | `docker`, `shortcuts`, `omarchy`, `herdr`, `img` | `docker ps`, `hyprctl binds`, Omarchy's menu scan, image metadata |
| 5. text and data | `unit`, `tz`, `def`, `snip`, `note`, `pass`, `radio`, `spotify`, `spotify-library` | `qalc`/python/`curl` per query; `def` and `omarchy` already cache |
| 6. the agent | `agent` (`do:`) | 2k lines of Python behind its own socket daemon; already long-lived, so the win is startup and packaging rather than per-keystroke |
| 7. marketplace | `bo` | `bo` itself is a third-party binary; the port would be the view contract, not the data |

Rules that have held so far and should hold for each: the script stays as the
fallback; the native provider returns `Fallback`/`Empty` exactly where the
script would print nothing; the row fields the view reads are the contract;
a case file lands with the port (see P2).

---

## What is already better than the script build

Worth keeping in mind while closing the gaps — these are not regressions to
defend but wins to preserve:

- One daemon instead of a `bash -lc` per keystroke; ~46 providers answer from
  one process, and the cache, availability store and registry survive the
  launcher closing.
- Rows are `Arc`-shared end to end (`row.rs:173`, `worker.rs:723`), merges
  are handle copies, and the cache stores built rows stamped against the
  extension definition (`cache.rs:34`).
- Several clients can attach to one engine (broadcast in `oxyd/src/main.rs`),
  which the QML could not do.
- `oxy query --local`, `oxy test`, `oxy test --cases`, `oxy extensions` and
  `oxy send` make the engine testable and scriptable without a shell — the
  case runner now attributes answers by epoch (`oxy/src/main.rs`), which the
  QML suite never had.
- 15 extensions answer in-process with the same rows, and `cal`'s
  natural-language leg resolves through the shared date parser instead of a
  second `bash` per query.

---

## Suggested sequence

1. **P0-1, P0-2, P0-3, P0-5, P0-9** — engine wiring, each a contained change
   in `engine.rs`/`worker.rs`/`settings.rs`/`state.rs`, each with a unit test
   that fails before and passes after.
2. **P0-7** (publish coalescing) — measured before/after with the same
   `oxy send` harness used here; it is the one change that moves every
   keystroke.
3. **P0-4** (login env) — touches the process layer once, benefits every
   remaining script extension.
4. **P0-6** (icons) and **P0-8** (`bo` id) — the two integration edges that
   make a fresh install look right.
5. **P1-10…P1-15** — small correctness and polish items.
6. **Porting waves 1–4**, each port carrying its case file, then 5–7.

Everything above is additive: the fallback contract means each step can land
alone, and the script build stays the reference the whole way.
