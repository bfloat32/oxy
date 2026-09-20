# Parity audit, second pass: the fixes reviewed, and what is still missing

> **Status (post-fix pass):** all findings resolved or deliberately answered.
> P1-15/N7 — the paste row carries `"\u{f0c1}"`, the literal glyph as an
> escape so it stays legible in source. N13 — `run_action` re-asks the
> *confirm* text (`/clear-all `), so the walking-away rule sees the arm's own
> text and keeps it; verified over the wire: arm → `type` → re-query keeps
> `confirm:"Clear recent queries and every pin?"` and the row, second Enter
> runs `clear.all` → "History and pins cleared". N1 — quicklink keywords ride
> the synthetic extension's `aliases`, so `known_keywords`, `claims` and the
> worker gate share one routing table; `extension_claims` skips `quicklinks`
> itself so a link cannot shadow itself; `later:hi` → `ql:later` measured.
> N2 — `spawn_workers` reconciles: workers are fingerprinted by
> `extension::def_stamp` (a `Debug`-text hash of the whole definition, so a
> field added later cannot slip past it) and changed/departed workers get
> `WorkerCmd::Shutdown`; measured: editing `probe.json`'s `search` produced
> the new `cmd` in the next `prov.start`, and `"extensions":{"date":false}`
> stopped `christmas` answering with date rows mid-session. N3 — the icon
> index re-arms only when the `.desktop` set's fingerprint changes, the Rust
> analog of the QML's `DesktopEntries.onValuesChanged`. N4 — the daemon
> creates the socket's parent dir, chmods a state-dir fallback `0700`, and
> chmods the socket `0600`. N5 — `oxy query` sends `opened:false`, and
> `publish` never marks a closed session's rows as showing; a `refreshMs:
> 800` probe did not tick after a CLI query but ticked 5× during a 3.5s open.
> N6 — recents say "Search Again" (help rows keep "Use Keyword", matching
> `Launcher.qml`'s two fill sites). N8 — `staleMap` removed; the wire `stale`
> field stays, it is protocol information the engine already computes. N9 —
> doc bug fixed in `README.md` both places: `?` lists loaded keywords,
> `when`-failing ones answer nothing; probing every `when` per `?` render
> would spawn a process per extension per keystroke, which is why neither
> build does it. N10 — `check.yml` fires on `experimental/rust-core` pushes
> and `tests/run.sh` gained the cargo step. N14 — `serde_json` now uses
> `preserve_order`, so a save keeps the file's key order, and the writer
> preserves the trailing newline. N15 — `hello` is built per connect from a
> shared slot `rebuild_known` refreshes on reload, and its version (and the
> `sess` log's) is `PLUGIN_VERSION` = `0.7.0`, the manifest's. N16 — the
> engine keeps the first previewed row's `revertExec` and never reverts
> between previews, matching `Launcher.qml`'s `previewRevert`; regression
> tests pin both flows. N11 stays open for the other eleven providers — the
> `later:hi` regression is covered by unit tests instead, because the case
> runner's `{keyword}:{query}` shape cannot express a link keyword and the
> link itself lives in settings, not the registry. N12 — `file_url()` is now
> shared: separators flip on Windows and spaces/`#`/`?` percent-encode, in
> `resolve_icon`, the file provider's art, recents and clipboard art.

This follows `docs/PARITY-RS.md`. Part 1 reviews the fix commit
(`d0ba261`, "Close the parity audit's P0 wiring gaps") against the claims in
its message, at the code level and against a live daemon. Part 2 is a deeper
sweep for gaps the first pass did not reach.

Method: the daemon and CLI were driven directly from this checkout over the
real wire (`oxyd` + a pipe client + `oxy send`/`query`), with a scratch
`XDG_CONFIG_HOME` holding the shipped extensions plus purpose-built probes
(`tick` for refreshMs, `slow`/`flaky` for timeout/exit paths, `probe` for the
environment and for edit-staleness), and a synthetic `XDG_DATA_HOME` with one
`.desktop` file and one icon for the icon resolver. Everything marked
*measured* below was observed that way; everything else is code reading.

---

## Part 1 — the fix review

| # | Claim | Verdict | Evidence |
|---|---|---|---|
| P0-1 | `Showing` sent, `refreshMs` arms | **fixed** | `tick` (refreshMs 1000) re-ran 33 times over 33s, ~1.03s apart, while its rows were showing; `arm_refresh!` gates on `opened && showing && live.epoch == current_epoch` and the engine now flips the flag on visibility transitions (`engine.rs:997-1014`) |
| P0-2 | `oxy.json` polled beside the extensions dir | **fixed** | editing the file produced a second `ext.load` + a re-`query` ~400ms later; the new quicklink keyword parsed (`scope='later'`) and its label resolved |
| P0-3 | Log vocabulary, `sid`, 1MB rotation | **fixed** | measured lines for `sess`, `env`, `query`, `rebuild`, `prov.start`, `prov.done`, `prov.fail` (code 3 from `flaky`), `prov.timeout` (1200ms from `slow`), `avail`, `ext.load`; every line carries `sid`; a 1.27MB file rotated to `.old` on the next append |
| P0-4 | Login environment captured and replayed | **fixed** | `~/.bash_profile` exporting `LOGIN_PROBE` reached the extension's command (`LOGIN=from-profile`), and `OXY_PLUGIN_ID=oma.oxyrs` rode along |
| P0-5 | `apps:`/`run:` browse; commands/quicklinks uncapped | **fixed** | `run:` returns all 24 rows; `commands.rs` gained the `query.empty` guard so the blank box stays recents-only |
| P0-6 | Icons resolve | **fixed** | a synthetic `.desktop` with `Icon=probe-icon` produced `iconSource: file://…/hicolor/48x48/apps/probe-icon.svg` |
| P0-7 | Provider answers coalesce | **fixed, as designed** | `run:` went from 47 `results` events to 4; an unscoped keystroke measured 14-18, which is one per late answer — the same shape the QML had, where in-pass answers were collected and late ones rebuilt |
| P0-8 | `oxy-bo` summons the right plugin | **fixed** | `${OXY_PLUGIN_ID:-oma.oxy}`; the daemon exports `oma.oxyrs` (measured). `wait_gone`/`oxy_layers` count namespace `oxy`, which both frontends use |
| P0-9 | `frecency_prune` runs | **fixed** | a 900-day-old entry and a 400-day-old one were dropped on the next save; a fresh entry survived |
| P1-10 | `engines` merge by id | **fixed** | code + the new `engines_merge_by_id` test |
| P1-11 | Fuzzy band order matches `Score.js` | **fixed** | name prefix → id prefix → name infix → id infix, UTF-16 lengths (`score.rs:125-160`) |
| P1-12 | Exec after the close event | **fixed** | `engine.rs:1228-1235` emits `Close` first; the action path already did |
| P1-13 | `?` gets the Keywords chip | **fixed** | `scope_label` short-circuits on help mode; measured `scopeLabel: "Keywords"` |
| P1-14 | Notices re-ask on the poll cadence | **fixed** | `Shell.qml`'s `notify()` restarts the poll timer |
| P1-15 | Paste row carries its link glyph | **NOT fixed** | the commit added the comment but left `row.icon_glyph = "".into()` (`engine.rs:680`); the QML sets U+F0C1 (`Launcher.qml:1053`) |

All nine P0s are real and reproducible, and five of the six P1s; the one
exception is a comment-only change. The `P0-7` verdict is "fixed, as
designed" rather than "fixed exactly": the wire still carries one event per
late answer, which is the shape the QML had.

Two smaller things in the same commit worth noting:

- `clip()` is a `pub(crate)` helper added to `lib.rs`; it is used for log
  fields only. Fine, but it duplicates the truncation `Logger.qml` did per
  field — nothing depends on the old shape.
- The `avail` log line reports `ms` from `avail_start`, which is also reset by
  the *re-check* path, so the first line after a typed keyword reports that
  wait rather than the boot probe's. Matches the QML.

---

## Part 2 — new findings

### N1 (P0) — a quicklink addressed by its own keyword answers nothing

Documented in the README ("type its keyword and the rest of the line becomes
the argument in `{}`", and `gh` alone opens the site). Measured: with
`{"title": "Added Later", "keyword": "later", "url": "https://x.example/{}"}`,
`later:hi` returns **zero rows** while the same link found by name (`added`)
returns its row. The scope parses (`scope='later'`) and the label resolves
("Added Later"), so the keyword is known to the parser — but the provider is
never asked.

Cause: the quicklinks built-in is an ordinary extension with keyword
`quicklinks` and no aliases, so both gates decline a query scoped to a link's
keyword:

- `Query::routes_to` (`query.rs:148`) — `scope != "quicklinks"`, aliases empty;
- the engine's `claims` (`engine.rs:538`) and the worker's ask gate
  (`worker.rs:448`) both route on that.

The QML had no such gate: `queryQuicklinks` ran for every query and treated
`query.scope === link.keyword` as "addressed"
(`Launcher.qml:1275-1341`, `Quicklinks.js`).

Fix, either way:

1. On settings load/reload, set the quicklinks built-in's `aliases` to the
   configured link keywords, and make `spawn_workers` respawn workers whose
   definition changed (see N2 — without that, the worker keeps the old list);
2. or give `claims`/`routes_to` a quicklinks special case that consults
   `settings.quicklinks` for the scope, in both the engine and the worker.

Option 2 is smaller today but leaves two routing tables; option 1 is the
honest one once N2 lands.

### N2 (P0) — a reload does not reload the workers

`spawn_workers` (`engine.rs:325`) only *adds* workers: `if
self.workers.contains_key(id) { continue }`. Nothing ever stops a worker, and
a worker owns a clone of the `Extension` it was born with
(`worker.rs:112`). The engine's own copy is replaced on reload
(`on_reload`, `engine.rs:1609`), so after a reload the two halves disagree.

Three measured consequences:

1. **Editing an extension does nothing.** `probe.json`'s `search` was changed
   to print `SECOND EDIT`; after the watcher's reload (confirmed in the log)
   the row still came from the old command, and `prov.start` still logged the
   old `cmd`. Same for `minChars`, `debounceMs`, `timeoutMs`, `view`, `tier`,
   `when`, `always`, `filters`, `cacheMs`, `refreshMs`.
2. **Turning an extension off does not turn it off.** With
   `"extensions": {"date": false}` in `oxy.json` (reload confirmed — the
   keyword left the parser's `known` set), an unscoped query for `christmas`
   still returned two `date` rows: the worker keeps `always: true` from its
   birth and is still asked.
3. **A renamed keyword leaves the two halves split.** The engine's `?` list
   and parser use the new keyword; the worker answers the old one (or not at
   all), so the card is empty while help advertises the keyword.

The QML rebuilt every provider on every registry change (the `Instantiator`
over `root.extensions`), so this is a genuine parity gap, not a design
difference.

Fix: reconcile in `spawn_workers` — keep a fingerprint per worker (id plus
every field a row build or a gate reads; `cache::ext_stamp` already hashes
the `to_row` half) and respawn on change, drop workers whose extension
disappeared, spawn new ones. The existing `ext_stamp` should grow the gate
fields (`search`, `when`, `min_chars`, `debounce_ms`, `timeout_ms`, `always`,
`filters`, `socket`, `cache_ms`, `refresh_ms`) or a second stamp should.

### N13 (P0) — a confirmed action cannot be confirmed

The README documents `/clear-all` as "both. Asks first: the second Enter is
the answer". Measured over the wire, with a Rust client doing exactly what
`Shell.qml` does:

```
query /clear            → 3 rows  [action:clear, action:clear-pins, action:clear-all]
activate action:clear-all → type: "/clear-all "      (armed)
query /clear-all        → 0 rows, confirm: ''        (the arm is gone)
activate action:clear-all → nothing
```

The arm is destroyed by the engine's own re-ask. `run_action`
(`engine.rs:1246-1258`) sets `pending_confirm`, emits `type "/clear-all "`,
and then re-asks **the old text**:

```rust
self.pending_confirm = Some(action.id.clone());
self.emit(Type { text: format!("/{} ", action.id), … }).await;
self.on_query(&self.raw.clone()).await;   // self.raw is still "/clear"
```

and `on_query`'s first rule (`engine.rs:484-488`) drops an arm whose text no
longer matches the typed query — `"/clear" != "/clear-all"` — so the arm is
cleared one line after it was set. Everything downstream then follows: the
`confirm` field on the wire is empty (no prompt is drawn), and the retyped
`/clear-all` no longer matches the action fuzzily, so the row that the second
Enter needs is not in the list either.

The Rust engine already has the two pieces the script build never had — the
`confirming` exception that keeps a re-typed `/id` in the list
(`answer_actions`) and the `confirm` field the empty state draws
(`Shell.qml:1120`) — so it is one line from working:

```rust
let text = format!("/{} ", action.id);
self.emit(Type { text: text.clone(), … }).await;
self.raw = text.clone();          // the arm must survive on_query's rule
self.on_query(&text).await;
```

(Dropping the internal re-ask entirely also works — the frontend sends the
query itself when the text lands — but then a client that ignores `type`
never sees the prompt.)

Worth knowing: the script build is broken here too, in a way the port
inherited — `Score.fuzzy` on the retyped `clear-all` returns −1 (verified by
loading `plugin/Score.js` + `plugin/Actions.js` in node: `arg "clear-all" ->
(no rows)`), so the QML drew the prompt from `pendingAction` but had no row
for the second Enter either. This is a "fix both" item, and the daemon is the
easier place to fix it.

### N14 (P3) — the settings writer reorders keys and drops the newline

`on_save_settings` (`engine.rs:1464`) writes `serde_json::to_string_pretty`
of a `Value` whose object is a `BTreeMap`, so a save reorders the user's keys
alphabetically, and unlike `Settings.withExtensionSettings` (which appends
`"\n"`) the file is left without a trailing newline. Measured: a
`savesettings` round trip turned a hand-written one-line `oxy.json` into a
pretty-printed, alphabetised one with no final newline. The content is
correct and nothing is lost — but the file is documented as one "meant to be
opened and read by hand", and a save that shuffles it is the kind of change
people notice.

### N15 (P3) — `hello` is a boot-time snapshot, and its version is the crate's

`oxyd/src/main.rs:276` builds the `hello` line once, before the accept loop,
from `engine.keywords()` as they were at boot. `docs/PROTOCOL.md` calls it
"the keywords the parser validates against", but after a reload (a new
extension, a renamed keyword, a new quicklink) a freshly connected client is
told the old set. `Shell.qml` ignores `hello` and waits for `registry`, so
nothing breaks today; a third-party client following the document would be
misled. Its `version` is `CARGO_PKG_VERSION` (0.1.0) where the plugin
manifest and the log's `sess` line say 0.7.0 — worth aligning, since the log
is what a bug report is read from.

### N3 (P1) — the icon index is rebuilt on every summon

`native/apps.rs:87` runs `scan_icons()` inside the same `if !scan.scanned ||
fresh` branch as the application scan, and `fresh` is the worker's
`fresh_open` flag — set on **every** `Opened(true)`, i.e. every launcher
open. `scan_icons` walks `~/.icons`, `$XDG_DATA_HOME/icons`, every
`$XDG_DATA_DIRS/icons` and `/usr/share/pixmaps`, twice (svg then png).

The QML deliberately did the opposite (`AppLibraryFallback.qml:107`): "The
index builds once per session. A summon used to re-walk every icon dir on
disk". On a box with Papirus installed that is tens of thousands of
`read_dir`/`stat` calls on the first app query after every Super+R.

Two fixes, both small:

- build the icon index once per daemon (lazily, on first use) and re-arm it
  when the applications dirs change — the daemon already polls a directory
  signature for the extensions dir and could take the applications dirs too;
- `Shell.qml:500` still calls `appLibrary.refreshIcons()` on every open. The
  fallback no-ops after its first build, but with the daemon resolving icons
  now, the only remaining consumer is `ResultWindows`'s window-class lookup
  (`ResultWindows.qml:131`), which does not need a per-open re-arm.

### N4 (P1) — the daemon socket is not hardened

`oxyd/src/main.rs:235` binds with `ListenerOptions::new().name(name)`, and
`interprocess` only chmods the socket when a mode is given (it is
`pub(crate)`-set in 2.4.4, and the daemon passes none), so the socket file
gets `0777 & ~umask` — typically `0755`. On Unix a socket is connectable by
anyone with write permission on the file, so on the state-dir fallback path
(`~/.local/state/omarchy/oxyd.sock`, which is not 0700 on a plain install)
**any local user can drive the launcher**: issue `activate` with an arbitrary
`exec`, or read what the user types. The script build had no IPC surface at
all, so this is new exposure introduced by the daemon.

Also: nothing creates the parent directory before binding, so a machine with
no `$XDG_RUNTIME_DIR` and no `~/.local/state/omarchy/` fails to start the
daemon (`bind` returns ENOENT), and the frontend then retries every 12s
forever.

Fix: after binding, `std::fs::set_permissions(socket, 0o600)` on Unix (and
`create_dir_all` + `0700` on the parent when falling back to the state dir).
The `$XDG_RUNTIME_DIR` case is already protected by logind's 0700.

### N5 (P1) — a CLI query leaves the engine "open"

`oxy query` sends `{"op":"query","text":…}` with no `opened` field, and
`parse_cmd` defaults it to `true` (`oxyd/src/main.rs:122`). So after
`oxy query "fire"` the engine believes a launcher is up: `refreshMs` timers
arm for visible rows and re-ask forever, because no `close` ever comes.
Measured: the `tick` probe (refreshMs 1000) kept re-running every ~1.03s for
as long as the client stayed connected, after a bare `query` op.

Fix: have `oxy query`/`oxy send` send `"opened": false` (or add a `--open`
flag), or treat a `query` that is not preceded by an `open` from the same
client as closed. The wire already has the field; only the CLI is not using
it.

### N6 (P2) — recents rows say "Use Keyword" where the QML said "Search Again"

`fill_row` hardcodes the action title (`engine.rs:1758`), and
`answer_recents` reuses it. The QML's `queryRecent` passed "Search Again"
(`Launcher.qml:848`) while `queryHelp` passed "Use Keyword" (`:827`). The
footer and Ctrl+K show the wrong verb on a past query.

### N7 (P2) — the paste row's glyph (the unfixed P1-15)

`engine.rs:680` sets `icon_glyph` to the empty string under a comment that
says it is a link glyph. The QML's row carries U+F0C1. One-line fix.

### N8 (P2) — `staleMap` is dead state in `Shell.qml`

`Shell.qml:41` holds it, `applyResults` fills it (`:366`), nothing reads it —
the engine owns staleness now (it holds Enter on a stale row and marks the
provider in `stale`). Either use it (e.g. a quiet hint) or drop the property
and the map-building loop.

### N9 (P2) — the `?` list advertises keywords whose `when` failed

Measured on this box: `?` lists Marketplace, Wi-Fi, Volume, Bluetooth and
Brightness, none of which can answer here (`bo`, `nmcli`, `pactl`,
`bluetoothctl`, `brightnessctl` are absent). The README claims twice that a
keyword whose requirement is missing "is not loaded at all" and "appears in
neither `?` nor your results" (`README.md:67`, `:162`).

This is not a port regression — the QML's `Help.entries` was handed every
loaded file and filtered on `extensionEnabled` only, exactly as the engine
does now — but the README is wrong for both builds, and the honest version of
the claim ("it answers nothing") is already true. Either filter `?` on
availability or fix the two sentences.

### N10 (P2) — CI and the local suite still do not cover this branch

`.github/workflows/check.yml` triggers on pushes to `main` and on pull
requests, so a push to `experimental/rust-core` runs nothing; the `rust` job
exists but only fires for a PR. `tests/run.sh` (the "everything CI runs,
locally" script the README points at) has no `cargo` step, so a local run
before pushing does not build or test the Rust half.

### N11 (P2) — 12 of the 15 native providers still have no case file

Unchanged from the first audit: `apps`, `calc`, `calchist`, `ch`, `commands`,
`file`, `kill`, `quicklinks`, `recent`, `ssh`, `sys`, `web`. `quicklinks`
would have caught N1 in a test: one case for `later:hi` is exactly the
assertion the port needed.

### N12 (P3) — icon URLs carry the platform separator on Windows

`resolve_icon` builds `format!("file://{path}")` from a `Path`, so on Windows
the URL is `file://C:/…\icons\hicolor\48x48\apps\probe-icon.svg`. Qt resolves
it, but a consumer that parses the URL (or a future `--local` CLI user) sees a
mixed separator. Percent-encoding is also skipped, so a path with a space or
`#` produces a URL that is not one. The `file:` provider already has
`file_url()` for exactly this; the icon resolver should reuse it.

### N16 (P3) — preview revert is the last row's, not the first's

Measured with a probe extension whose two rows each carry their own
`previewExec`/`revertExec`:

```
select row A   → A-preview
select row B   → A-revert, B-preview
close          → B-revert
```

The QML did something different (`Launcher.qml:565-588`): it remembered the
**first** `revertExec` it saw and ran that one on the way out, without
reverting between rows. For the only shipped previewing extension, `theme:`,
the two are equivalent — `bin/oxy-theme` computes one `$back` command for
every row (`bin/oxy-theme:212`) — but a provider whose revert is per-row
would restore the wrong state under the daemon. Worth aligning when the
porting wave reaches the next previewing extension, or documenting which one
is the contract.

---

## Part 3 — what was checked and found clean

So the second pass is not only a list of complaints, here is what was read or
measured and matched:

- **Row contracts, view by view.** Every field each `Result*.qml` reads was
  compared with what the provider emits: `files` (`art dir ext kind size age
  title`), `processes` (`pid user own cpu cpuLive mem memShare age count
  stopped windowed cmd win* machine{}` — `kill.rs:433` does carry the
  `machine` object), `hosts` (`alias hostName user port sourceFile identity
  proxyJump known`), `emoji` (`glyph` → `iconGlyph`), `dashboard` (`progress
  accessory detail`), `calendar` (`year month today weekStart marks`), `hero`,
  `split` (`preview art mono`), `grid`, `cards`. All present.
- **The engine's inline answerers** — help ordering, dedup and sigils; the
  settings list and form field shape; `/` action namespacing and `keepOpen`;
  recents' `worth_keeping`; paste's URL gate and actions. All match the QML,
  except the two cosmetic items above.
- **The worker state machine** — debounce, epoch drop, timeout, stale-keeps,
  cache/stale-while-revalidate, socket push epochs, native→fallback, and the
  new `Finished { code, timed_out }` plumbing (verified with `flaky` and
  `slow`).
- **State and settings** — frecency maths and prune, pins, recents, the
  settings write path (through the file's own text), the `engines` merge.
- **The installer** — option surface, link/unlink/hand-back, the shared-file
  ownership rules, the keybinding.
- **`bo:` end to end** — the toggle's return summon now uses the right id,
  `wait_gone` counts namespace `oxy` (both frontends), and the SEEN-file
  readiness check is unchanged.
- **Windows** — `cargo check`/`test` and the daemon over a named pipe still
  work; extension sockets are Unix-only by design.

---

## Part 4 — suggested order

1. **N13** (a confirmed action cannot be confirmed) — one line in
   `run_action`, and it is a documented key sequence that silently does
   nothing today. Do it first because it is the cheapest.
2. **N2** (reload must reconcile workers) — it is the root of N1's fix, of the
   `extensions: false` hole, and of every "I edited it and nothing happened"
   report. One function, one fingerprint, one test per symptom.
3. **N1** (quicklinks addressed by keyword) — the documented way to use a
   quicklink; lands naturally on top of N2 (aliases refreshed with the
   definition) or as a two-line gate special case before it.
4. **N5** and **N4** — the wire/CLI correctness and the socket hardening;
   both small and both about not being surprised by your own daemon.
5. **N3** — the per-summon icon walk; it is the one item here that costs
   something on every launch.
6. **N6-N9, N14, N15** — the cosmetic, documentation and file-format items,
   in one pass.
7. **N10-N11** — the CI trigger, the `cargo` step in `tests/run.sh`, and the
   case files for the native ports (start with `quicklinks` and `apps`, which
   between them cover N1 and the icon path; a `quicklinks` case would have
   caught N1, and a `clear-all` case N13).
