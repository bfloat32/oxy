# The native-porting backlog: what is left to port, reimplement and create

The restructure is done (see §0), so this is the full remaining surface
between the script build and the Rust core: batch by batch, with what each
port must reproduce, what it implies, and the traps that were found by testing
and are easy to lose. Nothing here is a code change.

**Revision 5.** Revision 4 audited itself against the tree; this one records
what changed since: the **marketplace removal** (`bo:` and its 2 518 lines are
gone — `docs/MARKETPLACE-REMOVAL.md`), the **first native LLM slice** (§4.4:
a local model endpoint answered in-process, 620 lines and 13 tests), and a
**re-measured baseline** (§0: 92 files, 16 316 lines, 60 tests). Every count
in this document was re-derived from the tree after both changes.

The corrections from the earlier passes, worth knowing up front:

- `tz` draws **four** views, not two: `hero` (a city), `zones` (a bare query,
  `noon`, a Discord timestamp), `timegrid` (people/places), `list` (fail rows).
- `oxy-repo` exposes **four** subcommands to other scripts (`--resolve`,
  `--current`, `--paths`, `--slug`), not one.
- `gh` draws its panels with `ghrepo`/`ghpr`; `stash` can also emit `list`
  rows; `spotify` emits `cards` as well as `player`.
- §6.2 was rebuilt from each view's own header comment, which is where the
  contracts actually live: `docker` owes `cid`/`image`/`status`/`ports[]`/
  `cpuFull`/`memPct` and friends, `processes` owes `cpuLive`/`winTitle`/
  `winClass`/`cmd`, `vault` owes `store`/`tool`/`clearSeconds`, `radios` owes
  `kind`/`joined`, and `radioplayer`'s `danger`/`glyph`/`primary` turned out
  to be the view's own button descriptors, not row fields at all.
- `def` also caps its cache at 500 entries; `oxy-date --iso` is called by
  `oxy-calendar` (the native `cal` already absorbed it in-process); and
  `oxy-emoji --used` has no callers left.
- Fields nobody reads: `dayline` on `tz` rows and `width`/`height` on `win`
  rows; the `win` intermediates (`wsRaw`, `focusOrder`, `address`) never
  reach the wire at all (§4.8).

---

## 0. The baseline this backlog starts from

The restructure is complete (all five splits merged), so the porting wave
starts from a measured state:

| fact | value |
|---|---|
| Rust files | 132 (37 before the restructure) |
| total lines | 28 774 |
| largest file | 719 lines (`provider/native/time/alarm/clock.rs`); nothing above the 800 target |
| tests | 325 passing (`cargo test --workspace`) |
| lints | `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --check` clean |
| guards | the file budget, the `model/` layering check and the **view-list sync** in `tests/run.sh`; `oxy test --only manifest` against this repo's extensions (it checks that every `native:` name is an arm in `construct`); clippy in CI; `core/.loc-allow` names the three exempt data tables; `core/README.md` is the crate map |
| extensions | 40 (27 native — 22 declaring `"native"` plus the five built-ins; 18 script-backed) — the marketplace was removed |
| case suites | 27 files, 443 assertions; the extensions without one are listed below |
| unchanged by the restructure | the wire, the row shapes, the state files, the script contract |

The restructure was a move, not a behaviour change, so every claim below —
budgets, gates, view contracts, state files — describes the code as it
stands. Two things have changed since: the marketplace is gone (§1), and the
LLM slice landed (§4.4).

---

## 1. Where we stand

**40 extensions ship. 27 answer through a native provider. 18 remain
script-backed** — 15 distinct scripts plus `pr`, `issue` and `ci`, which are
nine-line wrappers that set `OXY_GH_MODE` for `oxy-gh`. (`bo:` and its
marketplace were removed — see `docs/MARKETPLACE-REMOVAL.md`.)

| already native | area |
|---|---|
| `apps`, `commands`, `quicklinks`, `web` | desktop entry points (built-ins — no `"native"` field needed) |
| `calc` (+ `calchist`) | arithmetic, history |
| `date`, `cal` | dates, months |
| `emoji` | picker |
| `file`, `recent` | files |
| `kill`, `sys`, `ssh`, `ch` | system |
| `vol`, `bri`, `win`, `bt`, `wifi`, `theme`, `alarm` | batch A — local state |
| `herdr`, `img`, `pass`, `snip`, `note` | batch D/E — the cheap jq removals |

| batch | extensions | script lines | why they belong together |
|---|---|---|---|
| ~~**A. local state**~~ ✅ | `vol`, `bri`, `win`, `bt`, `wifi`, `theme`, `alarm` | 1 691 | done — merged `fc76de2`…`a112f2e`, case files landed |
| **B. git family** | `repo`, `git`, `branch`, `stash` | 1 713 | one shared resolver (`oxy-repo --resolve`) plus the same `git` plumbing — **decided: spawn+parse via `vcs/run.rs` (scaffolded), zero new deps** |
| **C. GitHub family** | `gh`, `pr`, `issue`, `ci` | 1 074 | one script in four modes (`OXY_GH_MODE`), GraphQL over `gh`, network |
| **D. session & system views** | `docker`, `shortcuts`, `omarchy`, ~~`herdr`~~, ~~`img`~~ | 1 371 | `herdr`+`img` done; `docker`, `shortcuts`, `omarchy` remain |
| **E. text & data** | `unit`, `tz`, `def`, ~~`snip`~~, ~~`note`~~, ~~`pass`~~ | 2 861 | `snip`/`note`/`pass` done; `unit`, `tz`, `def` remain — **`tz` decided: `jiff`** (chrono-tz is winding down; a table fails the 45-case contract) |
| **F. media** | `radio`, `spotify`, `spotify-library` | 1 019 | a player (mpv/MPRIS) plus a keyless or keyed catalogue lookup |
| **G. the long-lived one** | `agent` | 2 167 | a process with its own protocol; a rewrite, not a port |

### 1.1 Two rules for every batch

- **The script stays as the fallback.** A native provider answers first and
  returns `Fallback` where it declines, so a wrong port is a slower answer,
  never a missing one.
- **A port is not done without a case file.** 13 of the 40 have none
  (`calchist`, `ch`, `docker`, `file`, `kill`, `music`, `omarchy`, `radio`,
  `recent`, `shortcuts`, `spotify-library`, `ssh`, `sys`).

### 1.2 The budgets the scripts measured (the port's acceptance criteria)

The original author wrote the cost of each answer into the scripts. These are
what a native port must beat, and the numbers to re-measure against:

| keyword | the script's own measurement |
|---|---|
| `git:` | 227ms per keystroke before `--resolve` (a `git status` per candidate repo just to interpret a word) |
| `herdr:` | ~25ms per keystroke for two sessions: 1.5ms per `herdr api snapshot`, **~20ms of it jq** |
| `docker:` | `ps` 10ms + `inspect` 21ms for seven containers; `docker stats --no-stream` costs ~1s, hence the runtime cache |
| `theme:` | preview 44ms vs `omarchy theme set` 800ms; 322 themes = 387KB and 2.1s per bare `theme:`; ~2.2KB per row |
| `shortcuts:` | `omarchy-menu-keybindings --print` answers in ~10ms warm (it caches against a hash of `hyprctl binds`) |
| `gh:` | one GraphQL point per answer (of 5000/hour); panels are single documents so a panel is never half-drawn |
| `ch:` (native) | the port's comment records the target it removed: `ch:the` cost 537ms with a 1.2MB entry |
| `file:` (native) | `fd --max-results 400`, ranked in-process (the script took a wide slice because traversal order is not relevance) |

### 1.3 The `when` gates — and why a native provider must reimplement them

The worker's rule (`provider/worker/state.rs:326`): *"`when` guards the
command and socket legs … A native provider is its own answer: it runs
regardless and declines through Fallback, at which point the check matters
again."* So a native provider is asked even when its `when` fails — if it does
not re-check for itself, the keyword starts answering on machines where the
script stayed silent. Every port must reproduce its gate internally — and the
helper for it exists: `native/util.rs`'s `on_path` walks PATH instead of
spawning a shell, so a port writes `if !util::on_path("nmcli") { return
NativeOutcome::Fallback }` and pays a `stat` per PATH entry rather than a
process per keystroke.

| extension | gate (the script's `when`) | what the native must check |
|---|---|---|
| `alarm`, `omarchy`, `theme` | `command -v omarchy` | PATH lookup for `omarchy` |
| `bri` | `command -v omarchy-brightness-display` | PATH lookup |
| `bt` | `command -v bluetoothctl` | PATH lookup (or the D-Bus service) |
| `vol` | `command -v pactl` | PATH lookup (or the PipeWire socket) |
| `wifi` | `command -v nmcli` | PATH lookup (or the NetworkManager D-Bus name) |
| `unit` | `command -v qalc` | PATH lookup (the port still calls qalc) |
| `tz` | `command -v jq` | **nothing** once the port drops jq — the gate disappears with the dependency (keep it if the script leg remains the only answer) |
| `def` | `command -v curl` | PATH lookup, or nothing if the port grows its own HTTP |
| `repo` | `command -v git && command -v fd` | PATH lookup for `git`; `fd` goes away if the port walks the tree itself |
| `git`, `branch`, `stash` | `command -v git` | PATH lookup |
| `gh`, `pr`, `issue`, `ci` | `command -v gh && command -v jq` | PATH lookup for `gh` (jq goes away); a *titled row* when unauthenticated, never an empty list |
| `docker` | `docker info --format …` | **ping the daemon**, not the binary — a stopped daemon must hide the keyword |
| `shortcuts` | `command -v hyprctl` | PATH lookup, or the Hyprland IPC socket |
| `herdr` | `command -v herdr` | PATH lookup |
| `pass` | `command -v pass \|\| (command -v op && op account list)` | PATH lookup plus the `op` account check |
| `snip` | `test -f …/oxy-snippets.json` | the file test |
| `spotify-library` | `test -s …/oxy-spotify.json` | the token-file test |
| `spotify` | `command -v spotify` | PATH lookup |
| `img`, `note`, `radio`, `win` | *(none)* | nothing — always loaded |

### 1.4 The acceptance suites that already exist

**387 assertions in 16 case files** — the porting wave's free acceptance
suite, because `oxy test --cases <id>` runs them through the engine (native
provider first):

| batch | case files | assertions |
|---|---|---|
| A | `alarm` | 45 |
| B | `repo` 4, `git` 11, `branch` 10, `stash` 9 | 34 |
| C | `gh` 6, `issue` 4, `pr` 4, `ci` 2 | 16 |
| D | *none* | 0 |
| E | `unit` 72, `timezone` 45, `define` 30 | 147 |
| F | *none* | 0 |
| G | `agent` 18 | 18 |
| already native | `date` 56, `calendar` 30, `emoji` 41 | 127 |

Two consequences worth stating plainly:

- **Batches D and F have no coverage at all.** Their ports must write cases
  *first*, or the only thing proving the port works is the script it replaced.
- The behaviour suite runs nine scripts end to end (`oxy-docker`, the four git
  ones, `oxy-search-files`, `oxy-ssh`, `oxy-timezone-plan`, `oxy-wifi`). A
  native provider answers before the script, so that coverage stops being the
  guard the moment the port lands.

---

## 2. The batches

### 2.0 Where a port lands

The refactor's folders are real now, so each port has a home before it starts.
The rule the layout follows: **a folder is the subsystem a provider reads or
drives.**

```
provider/native/
  desktop/    apps, apps_icons, commands, emoji, emoji_data, quicklinks, web   (exists)
              + theme.rs, omarchy.rs, shortcuts.rs            (A, D — they read the desktop's config)
  calc/       mod, money, units, numbers, answer                               (exists)
              + unit.rs                                        (E — qalc's other face, same tables)
  time/       calendar, days, date/{mod,holidays,words,parse,render,grammar}   (exists)
              + alarm.rs, tz.rs   (tz may split: tz/{mod,names,zones,grid})    (A, E)
  system/     clipboard, file, kill, kill_windows, recent, ssh, ssh_config, sys (exists)
              + vol.rs, bri.rs, win.rs, bt.rs, wifi.rs, docker.rs, img.rs, pass.rs  (A, D, E)
  text/       calchist                                                          (exists)
              + snip.rs, note.rs, def.rs                       (E)
  vcs/        repo.rs, git.rs, branch.rs, stash.rs             (B — new; git.rs holds the shared plumbing)
  remote/     gh.rs (four modes), ci.rs, issue.rs, pr.rs       (C — new)
  media/      radio.rs, spotify.rs, spotify_library.rs         (F — new)
  agent/      mod.rs + its submodules                          (G — new; the largest)
```

The rest of the crate, for orientation (all of it exists today):

```
oxy-core/src/
  model/      action, event, query, row            — the wire shapes, no IO
  registry/   def, command, mod                    — extension files on disk
  settings/   defaults, paths, mod                 — oxy.json and every path we touch
  state/      frecency, pins, recents, mru, mod    — what the launcher remembers
  support/    availability, cache, quote, rank, score
  provider/   process, socket, worker/{mod,route,state}, llm/{mod,http,stream}, native/…
  engine/     activate, ask, builtins, inline, persist, pipeline, workers, tests
oxyd/src/     main, server, wire, logfile, watch, clipboard
oxy/src/      main, engine_local, cli/{query,send,test,extensions}, cases/{mod,check,view}
```

**The scaffold pattern** (commit `41b319a` for batch A, `081d9dc` for batch
B + `tz`): stub files, the `native/util.rs` gate helpers (`on_path`, `shq`),
and the `construct` arms are committed ahead of each wave, and each stub
declines every question — so a port lands file-local, in its own file, with
the manifest's script answering until the last line is written. Each port
is done in its own git worktree under `.worktrees/`, which is why nothing
collides. **Batch A and the cheap D/E removals have landed this way**; the
`vcs/` stubs and `time/tz.rs` are in place and waiting.

Two notes on the shape:

- `unit` goes beside `calc`, not in `text/`: it is the same qalc and the same
  unit/money tables, and `calc/units.rs` already exists to share.
- `tz` is the one provider likely to need submodules (`names.rs` for the loose
  matching, `zones.rs` for the column, `grid.rs` for the timegrid rows) —
  it draws four views and 45 cases pin the shapes.

Each extension gets: where the data comes from, what the row carries, what
`Enter`/the slider runs, the state it keeps, and the trap the script's own
comments call out. Line counts are the script's.

### Batch A — local state (`vol`, `bri`, `win`, `bt`, `wifi`, `theme`, `alarm`)

**`vol`** (144) — `slider`, two rows.

- Reads: `omarchy-audio-output-sink` (the *chosen* sink — not the default, so
  a speaker tuning or an EasyEffects sink in front of the hardware does not
  swallow the change) → `pactl get-sink-volume` / `get-sink-mute`; input via
  `wpctl get-volume @DEFAULT_AUDIO_SOURCE@` (what `omarchy-audio-input-mute`
  drives, so both agree on which mic is default).
- Rows: `Output Volume` (score 90000, subtitle = the sink's short name) and
  `Input Volume` (89000, subtitle "Microphone"), `view: slider`, `min 0`,
  `max 100`, `step 5`, accessory `Muted` or `N%`, group `Volume`.
- Enter: `omarchy-audio-output-volume mute-toggle` / `omarchy-audio-input-mute`.
- Slider: `oxy-volume set output|input {value}` — **a call back into the
  script**, which unmutes, sets the level and draws the OSD
  (`omarchy-osd -i <icon> -p <value>`). A port either keeps that call or
  reimplements the set leg *including* the OSD.
- Actions: Toggle Mute, Set to 100/50/0%, Switch Output/Input Device.
- Trap: the value is validated (`^[0-9]+$`, clamped to 100) because a
  hand-written exec could otherwise hand pactl `50%%`.

**`bri`** (52) — `slider`, one row.

- Reads: `omarchy-brightness-display` with no argument prints the current
  percentage (it owns the three dimming paths: `brightnessctl` for an internal
  panel, DDC for an external one, the Apple protocol for a Studio Display).
  Subtitle is the focused monitor (`omarchy-hyprland-monitor-focused`).
- Row: `Screen Brightness`, `min 1` (zero is a black screen with no way back),
  `max 100`, `step 5`, accessory `N%`.
- Enter: `omarchy-brightness-display 100%`; slider: `… {value}%`; actions:
  Set to 100/50/10%, Turn Display Off.
- Silence when the reading is not numeric — a desktop whose monitor answers no
  control channel has no reading, which is not an error and not a row saying so.

**`win`** (156) — `windows`.

- Reads: `hyprctl clients -j` **and** `hyprctl monitors -j` (the active
  workspace of every monitor, because a second screen has its own and nothing
  focused on it).
- The row carries: `id` (the window address), `title` (the class when the
  window has no title), `subtitle` (class · workspace), `cls`, `wsId`,
  `wsName`, `wsActive`, `wsWindows`, `special`, `monitor`, `focused`,
  `floating`, `fullscreen` (0/1/2 — maximised-in-gaps vs
  covering-the-monitor are told apart), `xwayland`, `pinned`, `grouped` (a
  tab group draws as one window and answers as several), `width`/`height`,
  `session {windows, matched, workspaces, monitors}` and the actions (Focus,
  Go to Workspace unless the window is on a special one, Close Window).
  `iconGlyph` is read by the view but never emitted by the script — window
  icons come from the frontend's app library (§4.6).
- Order: workspace ascending, then screen position (left→right, top→bottom),
  floating after the tiled windows they sit over. Focus history is *not* the
  order, because the top row is the one Enter runs.
- Unmapped windows are dropped; special-workspace windows are kept (a
  scratchpad terminal is exactly what someone types `win:` to find).
- Focus/close: the **Lua dispatcher** form, `hl.dsp.focus({window =
  "address:…"})` — `dispatch focuswindow` returns `ok` and does nothing under
  a Lua config.
- Query matches class, title **and workspace name** (`win:general` is a
  workspace filter without a second syntax).

**`bt`** (259) — `radios`.

- Reads: **one** `busctl` call to BlueZ `GetManagedObjects` — every device with
  every property in a single message, which is where battery, kind and RSSI
  come from. `bluetoothctl` is the *fallback* for a machine without busctl or
  the BlueZ object manager, with the extra fields simply absent.
- Rows: grouped connected → paired → nearby; `joined`, `known`, `mark`,
  `deviceKind`, `meta` (the address), `radioOn`/`radioLabel`, and
  `signal`/`signalLabel`/`battery` **only when measured** — an absent field
  and a field set to null read the same to the view, but only the absent one
  leaves the meter and the battery cell undrawn, so nothing unmeasured gets a
  zero drawn for it.
- Acting: `omarchy-bluetooth-device` (connect/disconnect must survive the
  rfkill soft block Omarchy uses as the real on/off state — a bare
  `bluetoothctl connect` fails outright while the block is set) and
  `omarchy-bluetooth-power on|off`.
- Adapter-off is not one row among the devices: it is the *only* row, and the
  view draws it as the whole answer.

**`wifi`** (236) — `radios`.

- Reads: `nmcli -t -f WIFI radio`, `nmcli -t -f TYPE,NAME connection show`
  (saved profiles), `nmcli -t -f DEVICE,TYPE device`, and the device wifi list
  with an explicit field set (one unknown field name makes nmcli print
  nothing, so the field list is version-sensitive).
- Three row kinds, because Enter means three different things: active →
  disconnect; saved → bring it up (no password); new → **open Omarchy's
  network panel**, which owns the passphrase prompt, the retry and the
  enterprise fields. The script's comment says this is because "there is no
  form view yet" — the form view exists now, so a port (or a small script
  change) can make this one row with a field in it.
- `--rescan no` on purpose: a rescan takes seconds and empties the list while
  it runs. `Rescan` is an action instead.
- Actions include `Speed Test` (`omarchy-network-speedtest`), `Copy Password`,
  `Turn Wi-Fi Off`, `Restart Wi-Fi`.

**`theme`** (232) — `themes`.

- Reads: `omarchy theme list` (the order it gives), `omarchy theme current`,
  each theme's `colors.toml` for the swatches, `omarchy theme dir` for paths.
- Preview: `omarchy-shell shell applyTheme <colors.toml base64> <shell.toml
  base64>` — the one thing `omarchy theme set` calls to retint the shell, at
  44ms instead of 800ms, plus `oxy-theme-preview <theme>` which repaints every
  running **foot** terminal via OSC. Revert is the same call with the theme you
  arrived on, so leaving without choosing leaves no trace.
- Enter: the real `omarchy theme set`.
- Rows: `previewExec`/`revertExec` (~2.2KB each, mostly the base64 colour
  files), `swatches`, `current`, `total`, `shown` — and the script **caps the
  row count** to the launcher's screenful because the launcher holds every row
  it is given and re-sorts on every keystroke (322 themes was 387KB and 2.1s).
- `theme:light` / `theme:dark` match the names *and* the themes' own
  brightness (`mode`, read with the shell rather than a fork, and only for
  those two queries).

**`alarm`** (612) — `hero` + `list`.

- The reminders are Omarchy's: `omarchy reminder <minutes> <message>` arms a
  transient systemd user timer. A reminder set here is the same object as one
  set from the bar and cancels the same way.
- The port's work is the **parser**: `30 minutes and 45 seconds`, `1h30m`,
  `in 2 hours`, `half an hour`, fractions — rounded **up** (an alarm that
  fires early is a broken alarm), and the row says what it rounded to.
- A time with no words after it is refused (`omarchy reminder 25` stores
  "25-min reminder", which tells you nothing when it fires).
- Rows: pending reminders with the time left, each with
  `oxy-alarm --cancel <unit>`; a `clear all` row (`omarchy reminder clear`).

### Batch B — git family (`repo`, `git`, `branch`, `stash`)

**`repo`** (776) — `repos`.

- Discovery: `fd --hidden --type d --max-depth 6` for `.git` directories **and
  `--type f` for `.git` files** (worktrees), over the roots, cached in
  `$XDG_STATE_HOME/omarchy/oxy-repos.list` with a **120s TTL** — the only
  number that decides whether typing costs a filesystem walk.
- Roots: `$OXY_REPO_ROOTS`, else `$OXY_ROOTS` (what `settings:` saves; the
  explicit export wins), else `~/localhost ~/Projects ~/projects ~/Work
  ~/work ~/src ~/code ~/dev ~/repos ~/git ~/Developer` — whichever exist.
- **Four subcommands are an API** (§3.12): `--resolve` (git/branch/stash),
  `--current` (the agent), `--paths` (stash), `--slug` (for handing to `gh`).
- Per-repo state (branch, dirty, ahead/behind, last-commit age) is read only
  for the rows that survive to the answer — proportional to what is shown, not
  to what exists. `branch:` is the one filter needing a fact about every
  candidate, and it gets it by reading `.git/HEAD` in bash rather than forking
  git per repo.
- `MAX_ROWS=12`; the pin file is `$XDG_STATE_HOME/omarchy/oxy-repo`; the view
  reads `ahead behind dirty drifted index path repo selected slug upstream`.

**`git`** (293) — `list` → row-level `gitrepo`.

- Everything is one pass of cheap plumbing over the one resolved repo:
  `git -C <repo> --no-optional-locks status --porcelain=v2 --branch`,
  `remote get-url origin`, `stash list | wc -l`,
  `log -6 --format=%h\x1f%s\x1f%an\x1f%ct` (unit separators, so a tab or
  newline in a subject cannot corrupt the row).
- Sections in the panel: branch/upstream/drift, uncommitted, stashes, recent
  commits; `omarchy-launch-tui … git diff` for the diff action.
- `--no-optional-locks` everywhere: the launcher must never take a lock that
  makes a commit in another terminal wait.

**`branch`** (348) — `gitbranches`: ahead/behind against upstream **and**
against trunk, current/gone marks, subject per branch.
**`stash`** (296) — `gitstashes` (+ `list`): the selected stash expanded into
its files (`stash show --stat`), added/deleted/message/ref per row; there is
no `stash drop` anywhere, on purpose.

### Batch C — GitHub family (`gh`, `pr`, `issue`, `ci`)

**`oxy-gh`** (1047) in four modes (`OXY_GH_MODE=repos|prs|issues|runs`; the
three nine-line wrappers exist so the cases runner can reach each mode by
naming a keyword).

- **The network rules are the design**: when the typed text already says
  something true — `gh:owner/repo`, a pasted URL, `owner/repo#123` — the row
  is drawn and pressable *before* any request, and the request is warmed in a
  detached process (`setsid --fork`); `refreshMs` re-asks a second later and
  the warmed answer is on disk. When the text does not (a bare `pr:`), the
  skeleton is better than an invented row.
- A stale cache entry is served while its replacement is fetched.
- The panels are single **GraphQL documents** (`gh api graphql`), not a REST
  call per fact — 1 point of 5000/hour, and a panel is never half-drawn. The
  views are `ghrepo`/`ghpr`; the list modes use `list`.
- Cache: `$XDG_STATE_HOME/omarchy/oxy-gh/*.json`, `CACHE_KEEP=200` with
  age-based pruning, **a lock beside every file** (that is what stops
  `refreshMs` from firing a second warm) and a `tried_recently` gate.
- `OXY_GH_OFFLINE` exists as a test switch (the cases use it).
- Without auth the answer is a titled row saying so, not an empty list; a bare
  `ci:` shows the shape rather than guessing.

Port decision: keep `gh` (auth, pagination, GraphQL) and parse in Rust — the
jq and the shell go, no new dependency, no second auth story. The GraphQL
documents port as strings. A direct-REST port is a separate project.

### Batch D — session and system views (`docker`, `shortcuts`, `omarchy`, `herdr`, `img`)

**`docker`** (340) — `docker`.

- **Two calls to the daemon per answer, no more**: `docker ps -aq` for the ids
  and one `docker inspect` for all of them. Inspect carries health, restart
  count, real start time and the port map as a structure.
- `docker stats --no-stream` costs a full second, so stats live in
  `$XDG_RUNTIME_DIR/oxy-docker-stats.tsv` behind a lock with an age check.
- `when` asks the daemon rather than the binary, so a stopped daemon hides the
  keyword instead of showing an error; the script re-checks anyway, because
  `when` is evaluated once at load — and a native provider must do the same
  (§1.3).
- Bands: running → restarting → stopped (`band` drives the tile's edge
  colour); the view reads `band cpu fraction health hostCores hostMem label
  loud memBytes quiet reading scale value`.
- Actions: logs, shell (`foot`/`setsid`), start/stop/restart.

**`shortcuts`** (302) — `shortcuts`.

- Primary source: `omarchy-menu-keybindings --print` — it re-reads
  `~/.config/hypr/hyprland.lua` for the keys Hyprland drops and resolves
  keycodes through the compiled XKB keymap, cached against a hash of
  `hyprctl binds` (~10ms warm). This matters: 59 of 235 binds come back from
  `hyprctl` with an **empty key** (most workspace switching) because Omarchy
  configures Hyprland from Lua (`dispatcher __lua` + a number).
- Fallback: `hyprctl binds -j`, honest but poorer (only binds that still carry
  a key; modmask translated by hand).
- The launcher's own keys are in neither source; they are listed and marked
  Oxy. A native port should list the *Rust* frontend's keys (`Shell.qml`).
- Enter copies the combination and never fires it.

**`omarchy`** (268) — `menutree`, and it is **Python**.

- Reads the same file Omarchy's menu reads, so a route that exists in the menu
  exists here. The tree is flattened: the path becomes context on one row
  ("Theme · Style"), and a submenu row is kept as well as its children.
- `trail` (the path as its own list), `kind` (`menu`/`action`/`link`),
  `node`, `depth`, `children` and `mode` (`browse` vs `search`) travel as
  their own fields because the view draws them; joining the path into a
  sentence here and splitting it there would make a separator a wire format.
- A bare `omarchy:` is browsing: the menu's root, in the menu's own order.
- `cacheMs: 600000`.

**`herdr`** (368) — `herdr`.

- One `herdr api snapshot` per **running** session (only sessions the CLI
  reports as running; naming a stopped one starts a server, and a launcher
  must never start something nobody asked for), then one jq.
- Reading is genuinely read-only: herdr marks a tab seen when focused and
  demotes `done` → `idle`, and its CLI reads do not mark anything seen — so a
  keystroke cannot erase the `done` state the keyword exists to report.
- Bands: blocked 0 → done 1 → working 2 → idle 3 → else 4, spent on colour and
  size rather than a status column.

**`img`** (93) — `grid`.

- Roots: `~/Pictures ~/Downloads ~/Desktop ~/Documents` (whichever exist) or
  `in:~/work`; `fd` for the extension list; dimensions from `identify` when
  ImageMagick is present and simply left out when it is not — "a missing chip
  is better than a slow search".

### Batch E — text and data (`unit`, `tz`, `def`, `snip`, `note`, `pass`)

**`unit`** (838) — `list`.

- qalc for the arithmetic (27 calls), but the port's substance is the
  **family gate**: both sides of a conversion must be a unit the script has a
  name for and both must be in the same family, or the answer is silence —
  because qalc makes a unit out of any letters ("how many feet in a mile" →
  `0.0000189394ny a·B·mi`, `5 KM IN MILES` → `5 K`, "1 cup of flour in grams"
  → `3.98529E−40 g·B²·L²`, all exit 0).
- Permissive about the *sentence*: `20 miles in km`, `180f in c`,
  `how many feet in a mile`, `6ft2 in cm`, `1 cup of flour in g` (density),
  and `20 miles` alone → km/metres/feet without being asked.
- `cacheMs: 60000`; 72 cases.

**`tz`** (838 + `oxy-timezone-plan` 189) — **four views**:

| query | view | what it draws |
|---|---|---|
| `tz:tokyo`, `tz:3pm in tokyo`, `tz:gmt+2` | `hero` | one clock, large, with the day and the shift |
| bare `tz:`, `tz:noon`, `tz:<t:…:F>` | `zones` | the column: your zone first, then the configured ones |
| `tz:john:tokyo maria:spain`, `tz:tokyo vs london`, `tz:me tokyo` | `timegrid` | the Python helper's day grid |
| a refusal (`tz:march`, `tz:20261325`) | `list` | the fail row |

- Zones from `timezones` in `oxy.json` ({label, zone}); the label is what you
  type and read, and the configured order is kept.
- Local zone: `timedatectl show -p Timezone` → `/etc/localtime` symlink → UTC.
- Forms: `tz:`, `tz:tokyo`, `tz:3pm tokyo`, `tz:9am tokyo in london`,
  `tz:<t:1735689600:F>` (Discord timestamp), `tz:1735689600`.
- Rows carry `clock`, `dayline`, `zoneid` and copy actions: Copy Time,
  Copy Date and Time, the nine Discord styles, Copy Unix Seconds.
- Loose matching is the part with cases (45): `tokyo` → `Asia/Tokyo`, `sp` →
  São Paulo, `la` → Los Angeles (initials beat substrings), a country → its
  main city, and a zone not in `timedatectl list-timezones` is dropped rather
  than read as UTC.
- Dependency: a tz database (`jiff` bundles one; `chrono-tz` is the
  conservative choice). This is the batch's real decision.

**`def`** (378) — `split`.

- `api.dictionaryapi.dev` (keyless; there is no offline dictionary on an
  Omarchy box — `dict` is not installed and `/usr/share/dict/words` is a
  cracklib spellcheck list with no definitions).
- A miss is not the end: the word is stemmed, the phrase is tried whole then
  by its first word, and a spelling no stem rescues gets **one Datamuse
  guess**, shown only after the guess itself has been looked up and found
  real. Whatever answers, the row says which spelling answered it.
- Cache: `~/.cache/oxy/define`, 30 days (`cacheDays`, i.e. `$OXY_CACHEDAYS`),
  **capped at 500 entries** with expiry-first eviction; `cacheMs: 600000`.

**`snip`** (68) — `snippets`: `oxy-snippets.json`, rows carry the text plus
line/character counts; `when` keeps it out of the launcher until the file
exists; Enter copies, Ctrl+K types it through `wtype`.

**`note`** (599) — `notes`: one markdown file per note; bare `note:` is a mode
(notes + count + the write row once there is something to name it after);
four instructions behind `--` (`--save`, `--open KEY "text"`, `--edit KEY
PATH`, `--trash PATH`). Writing is a mode of the script rather than a shell
line in a row's `exec`, because that is what makes the write atomic and the
index consistent.

**`pass`** (140) — `vault`: rows are name + folder + which store answered;
**no secret ever appears in a row, a subtitle, a detail or an argument** — the
value is read inside `oxy-pass copy` and piped to `wl-copy`, and the clipboard
is cleared after `CLEAR_SECONDS` *only if it still holds the secret*. Nothing
is cached (an entry list is cheap; a stale one is a lie). `pass` wins over
`op` when both are installed (no network, no unlock prompt).

### Batch F — media (`radio`, `spotify`, `spotify-library`)

**`radio`** (446) — `radioplayer`.

- radio-browser.info (keyless, name search), mpv for playback, and three
  subcommands behind the same script so rows can control the player without a
  second binary: `play <url> <name> <subtitle> <art> <homepage>`,
  `ctl toggle|stop|mute|volume up|volume down`, and `status` — what is
  playing and nothing else, for a bar widget, which must never touch the
  network (nothing playing prints nothing, which is how the widget knows to
  be absent).
- **One radio**: `play` stops whatever it finds before it starts anything —
  pressing Enter twice used to play two stations over each other with no way
  back. The mpv IPC socket path is also how "the radio" is recognised.

**`spotify`** (189) — `player` (+ `cards`).

- Control over **MPRIS** (`busctl`, `/org/mpris/MediaPlayer2`), search over
  **Deezer** (`api.deezer.com/search`, keyless, cover art), playback of a
  result via MPRIS `OpenUri`; the exact-track path lives in `oxy-music-play`
  (MusicBrainz ISRC → Spotify URL) and is what the README's chain describes.
  The script's own header still says "iTunes' public endpoint" — stale, the
  code uses Deezer.
- Three things found by testing that a port must keep: shuffle is turned off
  first (`OpenUri` otherwise plays a random track from the context), success
  is the **track id moving** rather than the call returning, and only D-Bus
  works (`xdg-open` claims the handler and does nothing).

**`spotify-library`** (384) — `cards`.

- `oxy-spotify search {query} {type}` → tracks, then albums, then artists;
  rows call back `oxy-spotify play <uri>` / `queue <uri>`.
- Credentials live in `~/.local/state/omarchy/oxy-spotify.json` (written by
  `oxy-spotify-auth`), deliberately **not** in `oxy.json`, which is meant to
  be committed and shared.
- Token refresh against `accounts.spotify.com`; a 401 tells the user to run
  the auth helper again.

### Batch G — the long-lived one (`agent`)

- **`agent` (`do:`)** — 2167 lines of Python behind
  `~/.local/state/omarchy/oxy-agent.sock`: process supervision of
  claude/codex/gemini, the event stream → transcript model, the policy engine
  (allow/deny/confirm), the `desk` helpers, the previews file
  (`oxy-agent-previews.json`), the idle expiry. The socket contract
  (`{epoch, query}` in, `{epoch, rows}` out, pushes between answers) does not
  change, so `ResultAgent.qml` and the engine stay as they are. It is the
  largest single item and the one with the richest existing suite (18 cases).
---

## 3. Cross-cutting decisions the batches force

1. **CLI-parse helper** (`provider/cli.rs`): run one command with a timeout,
   capture stdout, parse a documented format. Every batch A–D port needs it,
   and the natives that already spawn hand-roll it — `tokio::process::Command`
   in `calc/mod.rs`, `std::process::Command` in `system/kill_windows.rs`,
   `system/sys.rs`, `time/calendar.rs` and `time/date/parse.rs` — while
   `provider/process.rs` exists for *scripts*. One helper with the login env,
   the timeout and the `OXY_PLUGIN_ID` export would cover both.
2. **HTTP**: `def`, `radio`, `spotify-library`, `gh` (if direct) need a client.
   Keep `curl` as a subprocess (zero new deps, one spawn) or add `ureq`
   (`reqwest` is a large tree for four call sites). Decide once.
3. **D-Bus** (`zbus`): the real interface for `bt`, `wifi`, MPRIS. Optional for
   a first port — the CLI path is already written and tested — but it is where
   the spawns disappear.
4. **Timezone database**: only `tz` needs one.
5. **Git**: spawn `git` (the source of truth, and every edge case was found by
   testing it) vs `gix`. Recommendation: spawn, parse in Rust.
6. **Hyprland**: `hyprctl` spawn vs the IPC socket. `win`, `shortcuts`,
   `kill` (native), `herdr`, `docker`'s shell action all touch it; one
   `provider/hyprland.rs` pays for itself — including the Lua dispatcher forms
   (`hl.dsp.*`), which `kill.rs` already emits.
7. **MPRIS / mpv IPC**: the player family; one module, two callers.
8. **systemd user timers**: `alarm` only; keep `omarchy reminder`.
9. **Native providers and `extensionSettings`**: `repo`, `tz`, `def` must read
   `ctx.settings.settings_for(id)` and honour the same key names the scripts
   read from `$OXY_*` (`roots`, `zones`, `cacheDays`). No native does this yet
   — `repo` would be the first. Only six scripts read `OXY_*` at all:
   `define` (`OXY_CACHEDAYS`), `repo` (`OXY_REPO_ROOTS`, `OXY_ROOTS`,
   `OXY_REPO`), `timezone` (`OXY_ZONES`, plus `OXY_NAMES`/`OXY_HOME` in the
   Python helper), `gh` (`OXY_GH_MODE`, `OXY_GH_OFFLINE`; the three
   wrappers only set the mode), `theme` (`OXY_THEME_LIMIT`), and the agent
   (Python: `OXY_DESK_DRY`).
10. **A port must not change the wire**: same row fields the view reads
    (§6.2), same `exec`/`setExec` strings, same state files (§6.4).
11. **A port must reproduce its `when`** (§1.3) or it will answer where the
    script stayed silent.
12. **The internal API surface is part of the contract** (§6.1).

### 3.13 Dependency ledger

The crate's dependency set is deliberately small — `serde`, `serde_json`
(`preserve_order`), `tokio`, `interprocess`, `fancy-regex`, `sysinfo`,
`ignore` — and most of the porting wave adds nothing to it. The LLM slice
added no crate either: it enabled tokio's `net` feature and wrote the HTTP
client by hand (it was already coming in transitively through
`interprocess`, but relying on someone else's feature list is how a build
breaks on a patch release):

| batch | Cargo.toml delta |
|---|---|
| A | **none** if the ports spawn the CLIs they already call; optional `zbus` if `bt`/`wifi` move to D-Bus |
| B | **none** (spawn `git`) |
| C | **none** (spawn `gh`) — an HTTP client only if the direct-REST route is chosen |
| D | **none** (spawn `docker`/`hyprctl`/`herdr`) |
| E | **one**: a tz database for `tz` (`jiff` or `chrono-tz`). `def`/`unit`/`note`/`pass`/`snip` add nothing if `curl`/`qalc` stay the engines |
| F | **none** if `curl` + `mpv` stay; optional `zbus` for MPRIS |
| G | **none** (`tokio` is already a process supervisor); the agent's protocol is ours |

That is the argument for the CLI-parse helper over per-tool crates: the
wave's latency wins come from removing *bash, jq and awk*, not from removing
the tools.

---

## 4. Not extensions: what else is left to port, reimplement or create

### 4.1 Helper scripts (4) the launcher or a script calls

| helper | lines | called by | a native port means |
|---|---|---|---|
| `oxy-calc-history record` | 105 | the calculator's accept action (the native `calc` still emits it) | a native append to `oxy-calc-history.json`; the read leg is already native, so this closes the pair |
| `oxy-theme-preview` | 29 | the `theme:` rows' `previewExec` (repaints foot terminals) | folded into the native `theme` port, or kept as the one-line helper it is |
| `oxy-timezone-plan` | 189 | `oxy-timezone` for the day grid | folded into the native `tz` port — it emits the `timegrid` rows |
| `oxy-music-play` | 91 | `oxy-search-music`'s row exec (MusicBrainz → `OpenUri`) | folded into the native `spotify` port |

### 4.2 The test surface: `oxy test` is missing three of the four check layers

`oxy test` today runs each extension's `testQuery` through the engine, and
`--cases` runs the `.cases.json` assertions. Missing:

- **manifest**: the JSON read the way the launcher reads it — `id` and
  `search` present, `view`/`tier` names the launcher knows, numbers are
  numbers. This is the check that catches a typo'd `native:` name (which
  silently falls back to the script today).
- **actions**: every action on every row names a program on `PATH`, run
  through `testQuery`.
- **flags**: `--only manifest|answer|actions|cases`, `--fast`, `--jobs n`,
  `--quiet`.
- **fixtures**: `tests/cases.py` builds a throwaway `HOME` with a git
  playground, an `omarchy`/`gum` stub and a stub `claude`; `oxy test --cases`
  runs against the real `HOME` and skips what is missing. Porting the fixture
  playground is what makes the suite meaningful off an Omarchy box and in CI.

### 4.3 Two shipped features with no shipped user

- **Extension `actions`** — the engine implements them (namespaced
  `/spotify auth`, `confirm`, keywords), the docs show `oxy-spotify-auth` as
  *the* example, and no extension declares an `actions` block. The auth helper
  is therefore unreachable from the launcher; declaring it is one JSON block
  and gives the feature its first test.
- **The form view** — `ResultForm.qml` and the engine's form rows exist and
  the only user is the `settings:` form. `wifi:`'s own comment predates it
  ("there is no form view yet … when a form view exists this becomes one row
  with a field in it"), and the README already promises `wifi:` can "enter a
  password". A native `wifi` port is the natural place to cash that in.

### 4.4 The `ask:` chat surface — the first native slice is in

`docs/LLM-INTEGRATION.md` is the full design (provider registry, session
model, streaming, budgets). The **api-kind, local-first slice is
implemented**, in Rust, and it is the only part of this backlog that is not
about porting a script:

| done | where |
|---|---|
| `ask` in `oxy.json`: `endpoint`, `model`, `system`, `maxTokens` (800), `temperature` (0.4) | `settings/mod.rs`, `settings/defaults.rs` |
| a minimal HTTP/1.1 client — loopback `http://` only, chunked-aware, streaming lines as they arrive | `provider/llm/http.rs` (274) |
| the delta parser — OpenAI SSE, ollama NDJSON, llama.cpp `content`, in-stream errors, tolerant of the rest | `provider/llm/stream.rs` (173) |
| the request builder — the `messages[]` shape, with history replay already in the signature | `provider/llm/mod.rs` (173) |
| `Ctrl+Enter` answered in-process when an endpoint is configured; the CLI list is not probed | `engine/ask.rs` |
| the registry chip naming the model (`Local · llama3.2`) | `engine/ask.rs` |
| 13 tests: URL strictness, both body framings against a stub server, every delta shape, the settings merge | in the three modules |

Verified live against a stub streaming server: `answerstart` with provider
`Local · stub-1`, the deltas arriving as `answer` lines, `answerdone` with no
error; a dead endpoint lands in the card as *"Could not reach
127.0.0.1:19999 (…). Is the model server running?"*.

**Since that slice** (each verified against a stub, not by reading):

| landed | where |
|---|---|
| `ask.key` — a literal, or `env:NAME` so the value never lives in a committed file; sent as `Authorization: Bearer …`, and no header at all when unset | `provider/llm/mod.rs`, `http.rs` |
| retry: only the transient statuses, `Retry-After` honoured **capped at 60 s**, saturating parse, our own backoff otherwise; never re-sends once text has arrived | `provider/llm/retry.rs` |
| one streamed turn shared by the card and the CLI (`Piece::Text`/`Notice`/`Error`) | `provider/llm/turn.rs` |
| `oxy ask "question"` — the same client without the card, notices on stderr | `cli/ask.rs` |
| `oxy ask doctor --tier offline\|catalog [--json]` — checkpoints, verdict, next step, non-zero exit; `/v1/models` read tolerantly (OpenAI and ollama shapes, tag-insensitive) | `cli/doctor.rs`, `provider/llm/models.rs` |
| an endpoint that is *not listening* falls back to the CLI list, saying so in the card; one that answers badly does not | `engine/ask.rs` |
| soft interrupt — a question typed mid-stream queues and runs when the current one ends | `engine/ask.rs`, `pipeline.rs` |
| the model ledger — count + last-used per model, reported by `/stats` | `state/usage.rs` |
| the chip's `hint` when nothing is configured at all | `engine/ask.rs`, `Shell.qml` |

**What is deliberately not done yet**, in the order the design asks for it:

1. **Autodetect** — offer a local model when no endpoint is configured. The
   probe is called now (the doctor's catalog tier, and the boot-time Ollama
   hint in `?`), but nothing yet *configures* the endpoint for you.
2. **Token framing** — deltas are buffered into whole lines because the
   wire's `answer` event is one line per event and the card appends a
   newline between them (a token per event would render one word per line).
   §4 of the design's 120ms flush needs the frontend to change with it.
3. **The session file and multi-turn** — `ask:`/`chat` as a scope, turns
   replayed as `messages[]`, idle expiry (design §6, §8). The request
   builder already takes history.
4. **The provider registry** (design §5) — key'd endpoints and the CLI-kind
   transports as first-class rows: `ask.key` covers one endpoint, and the CLI
   fallback is a chain rather than a registry.
5. **`/` commands and markdown rendering** (§9). The `oxy ask` verb is in.

It depends on nothing in the porting batches, and the batches depend on
nothing in it — it is a parallel track, not a batch.

### 4.5 Windows gaps

- ~~`read_clipboard` is `#[cfg(unix)]`~~ — closed: the Windows half reads
  through PowerShell's `Get-Clipboard`, bounded twice (512 chars, two
  seconds), and the URL test is shared by both platforms. Verified live: a URL
  on the clipboard became the first row of an empty box.
- Extension sockets are Unix-only by design (`provider/socket.rs`).
- Icon/art URLs are canonical now (`file_url`), which was the other
  platform-shaped gap.

### 4.6 Window-class icons (see Batch A, `win`)

The daemon has no window-class → icon map; `ResultWindows.qml` resolves it
through the frontend's app library. Decide whether that stays the contract
(recommended) before porting `win:`.

### 4.7 Case files for the 12 native providers that have none

`apps`, `calc`, `calchist`, `ch`, `commands`, `file`, `kill`, `quicklinks`,
`recent`, `ssh`, `sys`, `web` — plus the 17 script extensions in §1.1. A
`quicklinks` case would have caught the routing bug; a `/clear-all` case the
confirm bug.

### 4.8 Dead wire fields (drop them in a port, they have no reader)

- `dayline` on every `tz` row (no QML reads it; the tz cases do not assert it).
- On `win` rows the intermediates (`wsRaw`, `focusOrder`, `address`, `w`/`h`/
  `x`/`y`) never reach the wire at all — they are jq-local, and the row carries
  `id`, `wsName`, `focused`, `width`/`height` instead. Of those, `width` and
  `height` are emitted but no view reads them; `wsName` and `special` are read
  by `ResultWindows` (the workspace heading and the SPECIAL marker).
- `oxy-emoji --used` has no caller left (the native emoji provider records
  through the engine's `remember`), so the subcommand is dead weight in the
  script, not a contract.

---

## 5. Suggested sequence, with a definition of done

Where the wave has got to, against the plan it started with:

1. **Start from the finished tree** (§0) — **done**: the refactor is merged,
   so every port lands straight into the folder §2.0 names for it, and the
   guards (file budget, layering, view-list sync, `oxy test --only manifest`,
   clippy) hold the line while it does.
2. **Batch A first** — **done** (`bri`, `vol`, `win`, `theme`, `bt`, `wifi`,
   `alarm`), the most-typed family, and it paid for the shared helpers
   (`util::on_path`, `util::shq`).
3. **The cheap jq-removals** — **done**: `herdr`, `img` (D), `snip`, `note`,
   `pass` (E).
4. **The two dependency decisions** — **in flight**: git (B) is scaffolded
   (`native/vcs/run.rs`, the shared `git --no-optional-locks` runner) with its
   providers landing; tz (E) is a stub, so the script still answers.
5. **The network family** (C, F) — **next**, once the HTTP decision is made.
6. **`docker`/`shortcuts`/`omarchy`** (D) — **next**; they are self-contained.
7. **`agent`** (G) — **last**, as its own project with the 18 cases as the
   acceptance suite.
8. **In parallel, not in a batch** — `oxy test`'s `manifest` layer landed
   (§4.2's cheapest third); the case files (§4.7) are 27 of 40.
9. **The LLM track runs alongside all of it** (§4.4) and touches nothing the
   batches touch: retry, the key, the doctor, the `oxy ask` verb, the fallback
   chain, the soft interrupt and the usage ledger have landed; autodetect,
   token framing with the frontend change it needs, and the session file are
   next.

### 5.1 Definition of done for one port

1. `oxy test --cases <id>` is green (all existing assertions).
2. **The manifest declares it**: `"native": "<name>"` in the extension's
   JSON, and that name is an arm in `native::construct`. Nothing else makes
   the provider load — a typo here is a port that silently never runs, and
   the script answers exactly as before, which is the one failure this wave
   can hide from every other check.
3. The `when` gate is reproduced inside the provider (§1.3) — with
   `util::on_path` where the gate is a `command -v`.
4. The rows carry exactly the fields the view reads (§6.2), and nothing the
   view needs is missing.
5. The `exec`/`setExec` strings are unchanged, or the callbacks they name are
   implemented (§6.1).
6. The state files it shares stay byte-compatible (§6.4).
7. It declines (`Fallback`/`Empty`) exactly where the script printed nothing —
   the "silent" cases are the spec for this.
8. New cases cover the behaviours the port adds, and the 13 extensions with no
   cases get their first ones.
9. The budget in §1.2 is measured and met.

---

## 6. Appendices

### 6.1 The internal API surface a port must provide or keep

| API | who calls it | what it is |
|---|---|---|
| `oxy-repo --resolve <q>` | `oxy-git`, `oxy-git-branch`, `oxy-git-stash` | repo path + unit separator + leftover text, with no git calls |
| `oxy-repo --current` | `oxy-agent` (where a run happens), `oxy-git`'s docs | the one repo `git:`/`pr:` should answer for |
| `oxy-repo --paths` | `oxy-git-stash` | the candidate repo paths |
| `oxy-repo --slug [path]` | (for handing to `gh`) | `owner/name` |
| `oxy-date --iso <words>` | `oxy-calendar` | the day the words mean — the native `cal` already absorbed this in-process |
| `oxy-timezone-plan` | `oxy-timezone` | the `timegrid` rows |
| `oxy-music-play <isrc> <phrase>` | `oxy-search-music` | MusicBrainz → `OpenUri` |
| `oxy-theme-preview <theme>` | `oxy-theme`'s `previewExec` | the foot-terminal repaint |
| `oxy-calc-history record <expr> <answer>` | the **native** `calc` | the history write |
| `oxy-volume set output\|input <n>` | its own rows | set + OSD |
| `oxy-alarm --cancel <unit>` | its own rows | stop a reminder |
| `oxy-note --save\|--open\|--edit\|--trash` | its own rows | the note operations |
| `oxy-spotify play\|queue <uri>` | its own rows | playback |
| `oxy-search-radio play\|ctl\|status` | its own rows; `status` has no in-repo caller (a bar widget) | the one-radio control |
| `oxy-gh` modes via `OXY_GH_MODE` | the three wrappers, the cases runner | repos/prs/issues/runs |
| `oxy-agent send\|serve\|plan\|stop\|new\|desk` | its own rows and the model's tool calls | the agent protocol |

### 6.2 Row contracts: what each view reads

These are the fields each view documents and reads (its own header comment is the source); `*` marks a field carried on the **first row** — the header object (`machine` for `processes`, `counts`/`offline` for `herdr`, `mode` for `menutree`, `repo` for `gitbranches`/`gitstashes`). Putting such a field on every row would be the same object a dozen times; a port must put it on row zero. Where a view names a nested shape (`repo { … }`, `turns[] { … }`), the port owes that shape, not just the leaf names.

| view | row fields (the view's own contract) |
|---|---|
| `list` | accent detail iconGlyph iconSource pending pinned source subtitle title |
| `hero` | clock subtitle title |
| `cards` | accent art detail iconGlyph iconSource source subtitle title |
| `split` | art detail mono preview title |
| `grid` | accessory art detail iconSource subtitle title |
| `dashboard` | accessory detail progress subtitle title |
| `calendar` | accessory marks month subtitle title today weekStart year |
| `player` | art controls lengthSeconds player progress seek status subtitle title |
| `slider` | accent accessory max min setExec step title value |
| `form` | exec ext fields label name onSubmit placeholder query readonly secret subtitle title value |
| `zones` | accessory clock detail subtitle title zoneid |
| `timegrid` | band best cells city delta detail label name ok people startsAt title |
| `gitrepo` | repo { name, path, branch, upstream, ahead, behind } · commits[] { hash, subject, author, age } |
| `gitbranches` | accessory ahead author behind current gone name subject trunk trunkAhead trunkBehind upstream · repo\* { name, path, branch, trunk, staged, changed, conflicted, count } |
| `gitstashes` | added age branch deleted files[] { path, added, deleted } message ref · repo\* { name, path, count } |
| `ghrepo` | repo { slug, description, language, stars, forks, private, archived } · prs[] { number, title, author, draft, mark, review, age } |
| `ghpr` | pr { repo, number, title, author, head, base, draft, state } · checks[] { name, mark, state, took } · reviews[] { who, state, mark, age } |
| `agent` | allows denies direct draft hints plan state · turns[] { you, state, now, did[], earlier, steps, answer, blocked[] } |
| `docker` | band cid cpu cpuFull cpuFullText exitCode full health hostCores hostMem image mem memBytes memCap memCapText memPct name oom ports[] project restarts service since status |
| `notes` | detail excerpt fresh kind tally words |
| `processes` | age cmd count cpu cpuLive mem memShare own pid stopped user winClass winTitle windowed windows workspace · machine\* { cpu, memUsed, memTotal, procs, matched } |
| `emoji` | iconGlyph subtitle title |
| `themes` | accent bg current dim fg mode surface swatches title total shown |
| `windows` | cls floating focused fullscreen grouped iconGlyph monitor pinned session special title width height wsActive wsId wsName wsWindows xwayland |
| `hosts` | alias hostName identity known port proxyJump sourceFile title user |
| `radios` | battery deviceKind iface joined kind known mark meta radioOn radioLabel secure signal signalLabel title |
| `radioplayer` | art controls elapsedSeconds kind muted station status subtitle title volume |
| `files` | age art dir ext kind size title |
| `repos` | age ahead behind branch dirty name path repo slug title upstream |
| `menutree` | children depth kind mode node title trail |
| `snippets` | chars lines preview text title |
| `vault` | clearSeconds folder name store title tool |
| `shortcuts` | accessory group keys title |
| `herdr` | band counts here kind name note offline paneId path session since status tabCount tabLabel what wsLabel |

### 6.3 Extension → view(s)

Script-backed extensions (the porting targets):

| extension | view(s) |
|---|---|
| `agent` | agent |
| `alarm` | hero, list |
| `branch` | gitbranches |
| `bri`, `vol` | slider |
| `bt`, `wifi` | radios |
| `ci`, `issue`, `pr` | list |
| `docker` | docker |
| `gh` | list, ghrepo, ghpr |
| `git` | list → gitrepo (row-level) |
| `herdr` | herdr |
| `img` | grid |
| `note` | notes |
| `omarchy` | menutree |
| `pass` | vault |
| `radio` | radioplayer |
| `repo` | repos |
| `shortcuts` | shortcuts |
| `snip` | snippets |
| `spotify` | player, cards |
| `spotify-library` | cards |
| `stash` | gitstashes, list |
| `theme` | themes |
| `tz` | hero, zones, timegrid, list |
| `unit` | list |
| `win` | windows |
| `def` | split |

Already native, for reference: `date` → hero, `ch` → split, `cal` → calendar,
`emoji` → emoji, `file`/`recent` → files, `kill` → processes, `sys` →
dashboard, `ssh` → hosts, `apps`/`commands`/`quicklinks`/`web`/`calchist` →
list.

### 6.4 State a port must keep compatible

| path | owner | what it is |
|---|---|---|
| `$XDG_STATE_HOME/omarchy/oxy-gh/` | `gh`/`pr`/`issue`/`ci` | the answer cache: `CACHE_KEEP=200`, a `.lock` beside each file, `FP-<hash>.json` naming |
| `$XDG_STATE_HOME/omarchy/oxy-repos.list` | `repo` (and its callers) | the discovery cache, 120s TTL |
| `$XDG_STATE_HOME/omarchy/oxy-repo` | `repo` | the pinned repo |
| `$XDG_STATE_HOME/omarchy/oxy-spotify.json` | `spotify-library` | the OAuth token (deliberately not in `oxy.json`) |
| `$XDG_STATE_HOME/omarchy/oxy-agent.sock`, `oxy-agent-previews.json` | `agent` | the socket and the preview registry |
| `$XDG_STATE_HOME/omarchy/oxy-calc-history.json` | `calc`/`calchist` (native) | the accepted answers |
| `$XDG_STATE_HOME/omarchy/oxy-emoji-recent` | `emoji` (native) | the picker's MRU, written through `remember` |
| `$XDG_RUNTIME_DIR/oxy-docker-stats.tsv` (+`.lock`) | `docker` | the 1s `stats` reading, cached with an age check |
| `$XDG_RUNTIME_DIR/oxy-note-saved` | `note` | the write stamp |
| `$XDG_RUNTIME_DIR/oxy-herdr-seen.json` | `herdr` | the seen-tab bookkeeping (runtime, so a reboot clears it) |
| `$XDG_RUNTIME_DIR/omarchy-reminders/` | `alarm` | the message files behind the systemd timers |
| `~/.cache/oxy/define` | `def` | 500 entries, 30 days (`cacheDays`) |
| `~/.cache/oxy/zones` | `tz` | the resolved zone-name cache |

`oxy-date`, `oxy-volume`, `oxy-theme`, `oxy-bri`, `oxy-win`, `oxy-wifi`,
`oxy-bt`, `oxy-pass`, `oxy-snip`, `oxy-unit`, `oxy-radio`, `oxy-shortcuts`,
`oxy-omarchy`, `oxy-img` and the git trio keep **no state of their own** —
they read the machine every time (which is also why so many of them are in
batch A: the reading is the whole cost).

### 6.5 The remaining 18 script-backed, measured
| id | keyword | view | script | lines | CLI/tools | refreshMs | cacheMs | cases | size |
|---|---|---|---|---|---|---|---|---|---|
| agent | do | agent | oxy-agent | 2167 | hyprctl, systemctl, python3, git, gh | 600 | – | yes | XL |
| gh | gh | list + ghrepo/ghpr | oxy-gh | 1047 | gh (GraphQL), jq, stat | 900 | – | yes | L |
| tz | tz | hero/zones/timegrid/list | oxy-timezone | 838 | python3, date, timedatectl | – | – | yes | L |
| unit | unit | list | oxy-unit | 838 | qalc, cal | – | 60000 | yes | L |
| repo | repo | repos | oxy-repo | 776 | git, fd, stat | – | – | yes | L |
| alarm | alarm | hero + list | oxy-alarm | 612 | omarchy reminder, date | 15000 | – | yes | M |
| note | note | notes | oxy-note | 599 | uwsm-app, jq, stat | – | – | no | M |
| radio | radio | radioplayer | oxy-search-radio | 446 | curl, mpv, python3 | – | – | no | L |
| spotify-library | sp | cards | oxy-spotify | 384 | curl (Spotify API), jq | – | – | no | M–L |
| def | def | split | oxy-define | 378 | curl (dictionaryapi, Datamuse) | – | 600000 | yes | M |
| herdr | herdr | herdr | oxy-herdr | 368 | herdr, jq | 2000 | – | no | S–M |
| branch | branch | gitbranches | oxy-git-branch | 348 | git, oxy-repo --resolve | – | – | yes | M |
| docker | docker | docker | oxy-docker | 340 | docker (ps + inspect) | 2000 | – | no | L |
| shortcuts | shortcuts | shortcuts | oxy-shortcuts | 302 | omarchy-menu-keybindings, hyprctl | – | – | no | M |
| stash | stash | gitstashes | oxy-git-stash | 296 | git, oxy-repo --resolve | – | – | yes | M |
| git | git | list → gitrepo | oxy-git | 293 | git, oxy-repo --resolve | – | – | yes | M |
| omarchy | omarchy | menutree | oxy-omarchy | 268 | Omarchy's menu file (python3) | – | 600000 | no | M |
| bt | bt | radios | oxy-bluetooth | 259 | busctl (BlueZ), bluetoothctl fallback | – | – | no | M |
| wifi | wifi | radios | oxy-wifi | 236 | nmcli | – | – | no | M–L |
| theme | theme | themes | oxy-theme | 232 | omarchy, omarchy-shell, find | – | – | no | M |
| spotify | spotify | player | oxy-search-music | 189 | busctl (MPRIS), curl (Deezer), mpv | – | – | no | M–L |
| win | win | windows | oxy-search-windows | 156 | hyprctl (clients + monitors), jq | – | – | no | M |
| vol | vol | slider | oxy-volume | 144 | pactl, wpctl, omarchy-audio-* | – | – | no | S–M |
| pass | pass | vault | oxy-pass | 140 | pass / op | – | – | no | S–M |
| img | img | grid | oxy-search-images | 93 | fd, identify | – | – | no | S |
| snip | snip | snippets | oxy-snippet | 68 | jq, wl-copy | – | – | no | S |
| bri | bri | slider | oxy-brightness | 52 | omarchy-brightness-display | – | – | no | S |
| ci | ci | list | oxy-gh-ci | 9 | wrapper → oxy-gh (runs) | – | – | yes | in C |
| issue | issue | list | oxy-gh-issue | 9 | wrapper → oxy-gh (issues) | – | – | yes | in C |
| pr | pr | list | oxy-gh-pr | 9 | wrapper → oxy-gh (prs) | 900 | – | yes | in C |

### 6.6 The traps: rules a port must not "fix"

Every one of these was found by testing, is written down in the script's own
header, and is the kind of thing a clean rewrite quietly loses.

| rule | where it comes from | what breaks if a port ignores it |
|---|---|---|
| A **pin** is clamped to its own tier; **frecency** is added to the finished score and is *not* — a heavily-used row near its tier's ceiling can cross into the next band (the script adds it the same way, so this is parity, not a port bug) | `state/pins.rs`, `state/frecency.rs`, `support/rank.rs` | a pinned substring never outranks a name that starts with what you typed; a frecency-boosted one can, in principle, outrank even a `forced` row |
| The web row is dropped whenever anything real matched; a scoped `?` keeps it | `support/rank.rs` (`merge`) | the fallback row buries the answer, or disappears from `web:` |
| The score tie-break is lowercased byte order, where the script used `localeCompare` — **an accepted deviation**, since matching ICU collation would take a dependency | `support/rank.rs` (`sort_key`) | two rows with *equal* scores and non-ASCII titles can swap places between the two builds |
| An empty answer over rows already on screen keeps those rows | `worker/state.rs` (`keep_stale`) | a timeout blanks the card (looks like "no matches") |
| A nonzero exit that printed rows is not a failure — only a bad exit with *no* rows is | `prov.fail` | a script that warns on stderr stops answering |
| `Close` is emitted *before* the row's `exec` | `engine/activate.rs` | the new window lands behind the overlay, the launch OSD under it |
| Every `git` call carries `--no-optional-locks` | the git family | the launcher makes a commit in another terminal wait |
| `qalc` makes a unit out of any letters — both sides of a conversion must be known units in one family | `oxy-unit` | `5 KM IN MILES` answers `5 K` with exit 0 |
| `nmcli`: one unknown field name makes the whole call print nothing | `oxy-wifi` | the list is empty on a different nmcli version |
| `vol` reads the *chosen* sink (`omarchy-audio-output-sink`), not the default | `oxy-volume` | the DSP sink's volume moves and the speakers do not |
| Hyprland under a Lua config: `hl.dsp.*` only — `dispatch focuswindow` returns `ok` and does nothing | `oxy-search-windows`, `kill_windows.rs` | "focus" silently does nothing |
| herdr reads must not mark a tab seen (focusing does) | `oxy-herdr` | a keystroke erases the `done` state the keyword exists to report |
| `theme:` preview must not call `omarchy theme set` (800ms); use `applyTheme` (44ms) + the foot repaint, and let Enter do the real set | `oxy-theme` | previewing takes a second and leaves the theme changed |
| `alarm` rounds *up* and refuses a time with no message | `oxy-alarm` | an alarm that fires early, or a notification that tells you nothing |
| Spotify: shuffle off first, success is the **track id moving**, and only D-Bus works (`xdg-open` claims the handler and does nothing) | `oxy-search-music` | the wrong track plays, or "success" is reported for a no-op |
| `pass`: no secret in a row, a subtitle, a detail or an argument; the clipboard is cleared only if it still holds the secret | `oxy-pass` | a password in `/proc/*/cmdline` and in the log |
| `wifi`: never rescan per keystroke (`--rescan no`); a new network opens the panel | `oxy-wifi` | every keystroke empties the list for seconds |
| `bt`: connect must survive the rfkill soft block → `omarchy-bluetooth-device` | `oxy-bluetooth` | connect fails outright while the block is set |
| Day arithmetic runs at noon, so a DST change cannot move a count by one | `oxy-date`, `time/date` | "in 90 days" is off by one across a clock change |
| `?` lists keywords whose `when` failed (they answer nothing) | `engine/inline.rs` + README | help hides what the machine actually has |
| `docker`: two calls per answer (`ps -aq` + one `inspect`); `stats` is a 1s reading and is cached | `oxy-docker` | a second per keystroke |
| `shortcuts`: 59 of 235 binds come back from `hyprctl` with an empty key under Lua — use `omarchy-menu-keybindings --print` | `oxy-shortcuts` | most workspace binds are missing |
| The launcher's own keys are in neither source; they are the frontend's | `oxy-shortcuts` | the list claims to be every key and is not |
| `repo`'s resolver makes **no** git calls; per-row state is read only for surviving rows; `branch:` reads `.git/HEAD` | `oxy-repo` | 227ms per keystroke |
| `gh`: text that already says something true draws first; the request is warmed detached; a stale entry is served while fetching | `oxy-gh` | the card stalls on a keystroke |
| A row with an empty title is dropped, and `maxRows` counts it before it goes | `model/row.rs` | a blank row, or one fewer answer than asked for |
| `fill` wins over everything on a row: Enter types, runs nothing | `engine/activate.rs` | picking a keyword from `?` launches something |
| `escExec` belongs to the frontend: Escape runs it before the ladder | `Shell.qml` | a running row is never told to stop |
| The `answer` event is **one line per event** — the card appends a newline between them | `Shell.qml`'s `case "answer"` | a token per event renders one word per line; deltas must be buffered into lines until the framing changes |
| The LLM endpoint is `http://` and nothing else, on purpose | `provider/llm/http.rs` | a launcher quietly sending a question to a remote host in plaintext |
| A configured endpoint replaces the CLI list rather than joining it | `engine/ask.rs`'s `probe_ask` | four probes spawned to ignore their answers |
| A manifest's `native:` name must be an arm in `construct` | `provider/native/mod.rs` | the port never loads and the script answers as before — indistinguishable from success |
| The scorer and the parser must keep matching the script **exactly**: `(?i)` on the filter regex (`FILE:x` is a filter, not text) and UTF-16 indices in the score bands (`Bücher` + `cher` is 7974, not 7964) | `support/score.rs`, `model/query.rs` | an upper-case keyword silently reaches no provider, and an accented name ranks lower here than in the script build |

### 6.7 Verifying a port

```sh
bash tests/run.sh                     # every check CI runs (static, behaviour, cases, cargo, guards)
cd core && cargo test --workspace     # the engine suite (325 tests)
cd core && cargo clippy --workspace --all-targets -- -D warnings

oxy test --only manifest              # every manifest: ids, keywords, views, tiers, `native:` in `construct`
oxy extensions --coverage             # the porting ledger: leg, cases, gate per extension
oxy test --cases <id>                 # that extension's assertions, through the engine (native first)
oxy test --cases <id> --json          # the same, for a CI job
python3 tests/cases.py <id>           # the same assertions against the script alone (the reference)
oxy test <id>                         # its testQuery through the engine
oxy query --local '<query>'           # one question through the in-process engine

printf '{"op":"query","text":"run:","opened":true}\n' | oxy send   # the wire, by hand
```

A case can build what it needs (`setup`) and skip where it cannot run
(`requires`) — see `docs/EXTENSIONS.md` for which is which. The `score` a case
asserts is the script's own 0–99999 number, so the same assertion means the
same thing whichever leg answered.

When a port is of a **JS or QML module**, the strongest check is the original
itself: `plugin/Score.js` and `plugin/Query.js` both run under node (strip the
`.pragma library` line), and the Rust side carries a table of their answers —
156 score cases and 33 parser cases today. Run the script, pin the numbers,
and the port cannot drift without a test failing.

The LLM slice has its own checks — the client and the parser are tested
against a stub server inside `cargo test`, so no network and no model are
needed:

```sh
cd core && cargo test -p oxy-core llm    # url, framing, deltas, retry, request shape
oxy ask doctor --tier offline            # wiring only: no socket, no spend
oxy ask doctor --tier catalog --json     # + connect and read /v1/models, for CI
oxy ask "what is 2+2"                    # the same client without the card
python3 tests/bench_oxy.py               # cold start, per-keystroke p50/p95, RSS
```

To check it end to end by hand, run any streaming OpenAI-compatible server
(or the stub the audits used: a `Transfer-Encoding: chunked` reply of
`data: {"choices":[{"delta":{"content":"…"}}]}` frames), point `ask.endpoint`
at it in a sandbox `oxy.json`, and send an `ask` op:

```sh
printf '{"op":"ask","text":"say hi"}\n' | oxy send
# answerstart provider "Local · <model>" → answer lines → answerdone error ""
```

The live smoke list the audits used, worth re-running after a batch lands:
`?` lists 46+ keywords and the chip reads Keywords; `run:` returns 24 rows;
`apps:` resolves an icon; `later:hi` returns the quicklink; `/clear` →
activate → `/clear-all` shows the prompt and the second Enter clears;
`tick:` refreshes while a client is open and does not after a CLI query;
`preview:` select A → select B → close runs `A-preview`, `B-preview`,
`A-revert`; `savesettings` keeps the file's order and newline.
