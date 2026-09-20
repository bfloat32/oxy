# Third pass: the fix verification, and the core restructuring plan

Two things in one file, as asked: **Part 1** verifies the fixes in `f5be64b`
("Close the second audit's gaps") against the findings in
`docs/PARITY-RS-2.md`; **Part 2** is a proposal for splitting the oversized
files and reorganising the Rust core — no code was changed for it.

Method for Part 1: `cargo test --workspace`, then the same live harness the
second audit used — a sandboxed daemon (`XDG_CONFIG_HOME` with the shipped
extensions plus probes) driven over the real wire with `oxy send`/`oxy query`
and a purpose-built `.desktop` + icon tree. Everything called *measured* was
observed in that harness.

---

# Part 1 — verification

## 1.1 The findings, one by one

| # | Finding | Verdict | Evidence |
|---|---|---|---|
| N13 | A confirmed action could not be confirmed | **fixed, measured** | `/clear` → `activate action:clear-all` → `type "/clear-all "` → query `/clear-all` returns **1 row** (`action:clear-all`) with `confirm='Clear recent queries and every pin?'`, and the second activate emits `notice "History and pins cleared"`. The arm survives the re-ask, and the prompt reaches the wire. |
| N2 | A reload did not reload the workers | **fixed, measured** | Editing `probe.json`'s `search` to `PROBE v2` and waiting for the watcher: the next query returned `PROBE v2` (was the old command). With `"extensions": {"date": false}` written mid-session, `christmas` returned only the web row — the date worker stopped answering (it used to return two date rows). |
| N1 | A quicklink addressed by its keyword answered nothing | **fixed, measured** | `later:hi` → `ql:later | Added Later | omarchy-launch-browser 'https://x.example/hi'` — routed, parsed, and the argument substituted. |
| N3 | The icon index was rebuilt on every summon | **fixed, measured** | Added `~/.icons/apps/probe-icon.svg` (an earlier dir, same `.desktop` set) → resolution **unchanged**; then added a `.desktop` file → next open rebuilt the index and resolution moved to the new file. That is the QML's `onValuesChanged` edge, not the summon edge. |
| N4 | The daemon socket was not hardened | **fixed, code-verified** | Unix-only, so not runnable here: parent dir created, state-dir fallback `chmod 0700`, socket `chmod 0600` (`oxyd/src/main.rs:231-262`). Windows has no socket file. |
| N5 | `oxy query` left the engine "open" | **fixed, measured** | With the `tick` probe (refreshMs 1000): after `oxy query "tick:"` the probe ran **once** and did not refresh over the next 4s. With a client that sends `opened:true`, the same probe ran 5 times in 4s — refresh still works where it should. |
| N6 | Recents rows said "Use Keyword" | **fixed, measured** | The recents row's action list is `['Search Again']`. |
| N7 | The paste row had no link glyph | **fixed, code-verified** | `row.icon_glyph = "\u{f0c1}"` (`engine.rs`); the clipboard row is Unix-only, so it cannot be seen on this box. |
| N8 | `staleMap` was dead state | **fixed** | Property and its two write sites removed from `Shell.qml`; `waiting` (which is read) stays. |
| N9 | `?` advertised keywords whose `when` failed | **fixed (docs)** | Both README claims now say the keyword stays listed and answers nothing — the behaviour both builds have. |
| N10 | CI and the local suite did not cover this branch | **fixed** | `check.yml` triggers on `experimental/rust-core` too; `tests/run.sh:106-112` runs `cargo check` + `cargo test`, skipped where cargo is absent. |
| N14 | Settings saves reordered keys and dropped the newline | **fixed, measured** | A hand-written file with `zzz_last` first and `aaa_first` fourth came back in the same order with `extensionSettings` appended, and the trailing newline preserved (`preserve_order` + the explicit newline carry). |
| N15 | `hello` was a boot snapshot, version from the crate | **fixed, measured** | Fresh connect: `version 0.7.0`, 180 keywords including `later`. After removing the quicklink and the reload landing: 179 keywords, `later` gone. The log's `sess` line also reports `0.7.0`. |
| N16 | Preview revert was the last row's, not the first's | **fixed, measured** | `select A` → `A-preview`; `select B` → `B-preview` (no revert between); `close` → **`A-revert`**. The QML's `previewRevert` semantics exactly. |
| N12 | Icon URLs carried the platform separator | **fixed, code-verified** | `native::file::file_url` is shared and canonicalises Windows separators; `apps`, `file`, `recent` and `clipboard` all route through it. |
| N11 | 12 of 15 native providers have no case file | **open, as scoped** | Still 17 case files. This was a recommendation, not a bug, and the commit did not claim it. |

`cargo test --workspace`: **46 passed**, including the five new regression
tests (`quicklink_keyword_routes_to_provider`,
`quicklink_alias_survives_reload_and_disable`, `def_stamp_covers_gate_fields`,
`confirmed_action_arms_and_confirms`, `preview_revert_is_the_first_rows`).

## 1.2 One new nit the fixes introduced

**The scope chip for a link keyword says "Quicklinks" instead of the link's
name.** Measured: `later:hi` reports `scopeLabel: "Quicklinks"`; the QML
reported `"Added Later"`.

Cause: quicklink keywords now ride the synthesized quicklinks extension as
aliases (which is what makes routing work), and `scope_label` checks the
extension list *before* the user's quicklinks
(`engine.rs`, `scope_label`: the `for ext in self.extensions` loop runs
first). The QML's extension list never contained quicklinks, so its lookup
fell through to the quicklinks loop.

Fix (one of): check `settings.quicklinks` before the extensions loop, or skip
`ext.id == "quicklinks"` in the extension loop. Cosmetic, but it is the one
thing the previous behaviour got right and this one does not.

## 1.3 Still open from the two audits

- **Case files for the native ports** (N11): `apps`, `calc`, `calchist`,
  `ch`, `commands`, `file`, `kill`, `quicklinks`, `recent`, `ssh`, `sys`,
  `web`. A `quicklinks` case would have caught N1; a `/clear-all` case N13.
- The porting backlog from `docs/PARITY-RS.md` is untouched (31 script
  extensions), by design — those were never bugs.

---

# Part 2 — the restructuring plan

## 2.1 The problem, measured

`cargo`'s share of the tree is 14,817 lines across 37 files. Two files carry
the bulk of the debt, one more is a single function, and a handful are in the
400–600 band:

| file | lines | shape |
|---|---|---|
| `oxy-core/src/engine.rs` | **2305** | one `impl Engine` of 1580 lines + 137 lines of protocol enums + 460 of built-ins + 189 of tests |
| `oxy-core/src/native/date.rs` | **1718** | 5 unrelated concerns: holiday table, word normalisation, date parsing, rendering, grammar rules |
| `oxy-core/src/native/calc.rs` | 995 | gate, units, money, numbers, answer shaping, provider — all in one |
| `oxy-core/src/provider/worker.rs` | 817 | types (117 lines) + **one `fn run` of 684 lines**, held together by 8 macros |
| `oxy/src/main.rs` | 639 | 4 CLI verbs + a case runner + its assertion engine |
| `oxy-core/src/native/apps.rs` | 534 | scan, desktop parsing, icon index, icon resolution |
| `oxy-core/src/native/ssh.rs` | 513 | config parsing, glob matching, known_hosts, provider |
| `oxyd/src/main.rs` | 491 | boot, accept loop, wire parsing, log file, watcher, clipboard |
| `row.rs` / `emoji.rs` / `extension.rs` / `kill.rs` / `settings.rs` | 434–468 | one type each plus helpers |
| `calendar.rs` / `recent.rs` / `file.rs` / `state.rs` / `sys.rs` | 306–394 | fine, but `state.rs` holds four unrelated memories |

Everything else is under 300 and healthy. The flat `src/` (20 files) also
mixes layers: `engine.rs` next to `dirs.rs` next to `rank.rs`, with no signal
about which is model, which is IO, and which is orchestration.

## 2.2 The rules this plan follows

1. **A file is a noun.** Its name says what is inside; if the name needs
   "and", it is two files.
2. **Budget: 800 lines target, 1200 hard cap.** Nothing in the current tree
   needs to be near the cap once split; the cap exists so the next feature
   has room. Data tables are exempt (see 2.5).
3. **One concept per file, one layer per folder.** Dependencies point one
   way: `model` ← `registry`/`settings`/`state` ← `provider` ← `engine` ←
   `oxyd`/`oxy`. Nothing in `model` may open a file or spawn a process.
4. **`mod.rs` is a table of contents**: module docs and `mod`/`pub use`
   lines, no logic.
5. **Tests live with their subject** (`#[cfg(test)] mod tests` in the file
   they test); a suite that needs the whole engine goes to
   `engine/tests.rs`.
6. **Visibility discipline**: `pub(crate)` for anything that only crosses
   module lines; `pub` only for the crate's API (`lib.rs` re-exports).
7. **No behaviour change.** Every step is a move or a mechanical
   extraction; `cargo test --workspace` and a live smoke test gate each one.

## 2.3 Target tree

```
core/
├── README.md                     new: the crate map, the layering rule, how to run/test
└── crates/
    ├── oxy-core/src/
    │   ├── lib.rs                crate docs, re-exports, PLUGIN_VERSION, clip()
    │   ├── model/                the shapes on the wire — no IO, no logic beyond parsing
    │   │   ├── mod.rs            docs: what a row, an action, a query, an event is
    │   │   ├── event.rs          EngineCmd + EngineEvent (from engine.rs)
    │   │   ├── row.rs            Row + RESERVED + to_row* + parse_rows
    │   │   ├── action.rs         Action
    │   │   └── query.rs          Query (parse, routes_to, arg_for, extras)
    │   ├── engine/               the orchestrator — one file per phase of its life
    │   │   ├── mod.rs            Engine, start, run, handle_cmd, shared/keywords
    │   │   ├── pipeline.rs       on_query, claims, put_inline, handle_worker, publish, scope_label
    │   │   ├── inline.rs         the launcher's own answers: help, recents, paste, settings, actions
    │   │   ├── activate.rs       on_activate, run_action, pin, select/preview, set
    │   │   ├── ask.rs            on_ask, stop_ask, probe_ask, emit_registry
    │   │   ├── persist.rs        on_reload, on_save_settings, remember*, save_state, emit*
    │   │   ├── workers.rs        spawn_workers + the reload reconciliation
    │   │   ├── builtins.rs       builtin_help/keywords/extensions/actions
    │   │   └── tests.rs          the engine suite
    │   ├── provider/             how a question reaches an extension
    │   │   ├── mod.rs            NativeExt, Ctx, NativeOutcome
    │   │   ├── process.rs        bash -c, the login env, timeouts
    │   │   ├── socket.rs         the socket actor
    │   │   ├── worker/
    │   │   │   ├── mod.rs        WorkerCmd/Msg, Shared, Pending, Live, the select loop
    │   │   │   ├── state.rs      the per-extension state machine, as a struct
    │   │   │   └── route.rs      ask → native/socket/proc fallback, cache + stale rules
    │   │   └── native/           the compiled-in providers, by the subsystem they read
    │   │       ├── mod.rs        construct()
    │   │       ├── desktop/      apps (+icons), commands (+data), quicklinks, web, emoji (+data)
    │   │       ├── calc/         mod (provider+gate), money, units, numbers, answer
    │   │       ├── time/         date/ (mod, holidays, words, parse, render, grammar), calendar, days
    │   │       ├── system/       sys, kill (+windows), file, recent, clipboard, ssh (+config)
    │   │       └── text/         calchist
    │   ├── registry/             extension files on disk
    │   │   ├── mod.rs            load_dir + LoadReport
    │   │   ├── def.rs            Extension, Setting, Raw, def_stamp
    │   │   └── command.rs        build_command, settings_prefix, cache_key, known_keywords
    │   ├── settings/             the user's configuration
    │   │   ├── mod.rs            Settings + merge + accessors
    │   │   ├── defaults.rs       default engines/askProviders, url_encode
    │   │   └── paths.rs          where every file lives (today's dirs.rs)
    │   ├── state/                what the launcher remembers
    │   │   ├── mod.rs            State, load/save, atomic write
    │   │   ├── frecency.rs       decay, boost, record, prune, apply
    │   │   ├── pins.rs           has/toggle/apply
    │   │   ├── recents.rs        worth_keeping, record
    │   │   └── mru.rs            the provider-declared recency files
    │   └── support/              small, dependency-free helpers
    │       ├── cache.rs          TTL + LRU + stale-while-revalidate
    │       ├── availability.rs   the `when` store
    │       ├── rank.rs           tiers + merge
    │       ├── score.rs          the fuzzy scorer
    │       ├── quote.rs          shell quoting (today's shellquote.rs)
    │       └── text.rs           clip()
    ├── oxyd/src/
    │   ├── main.rs               boot + wiring
    │   ├── server.rs             accept loop + serve_with
    │   ├── wire.rs               parse_cmd + hello
    │   ├── logfile.rs            sid, log_line, append_log, rotation
    │   ├── watch.rs              signatures + the reload poll
    │   └── clipboard.rs          url_in_clipboard + read_clipboard
    └── oxy/src/
        ├── main.rs               dispatch + USAGE
        ├── engine_local.rs       the in-process engine
        ├── cli/                  query.rs, send.rs, test.rs, extensions.rs
        └── cases/                mod.rs (runner), check.rs (assertions), view.rs (row mapping + preflight)
```

## 2.4 The splits, file by file

Estimates are from the current line ranges, so the result is checkable.

**`engine.rs` (2305) → `engine/` (9 files, 90–370 each)**

| target | contents | ~lines |
|---|---|---|
| `model/event.rs` | `EngineCmd`, `EngineEvent`, `NO_FRECENCY` | 145 |
| `engine/mod.rs` | `Engine` struct, `start`, `run`, `handle_cmd`, `shared`, `keywords`, `rebuild_known` | 330 |
| `engine/pipeline.rs` | `on_query`, `claims`, `put_inline`, `handle_worker`, `publish`, `scope_label` | 270 |
| `engine/inline.rs` | `answer_help/recents/paste/settings/actions`, `settings_form`, `all_actions`, `fill_row`, `FillSpec` | 370 |
| `engine/activate.rs` | `on_activate`, `run_action`, `on_pin`, `on_select`, `unpreview`, `on_set`, `find_row` | 360 |
| `engine/ask.rs` | `on_ask`, `stop_ask`, `probe_ask`, `emit_registry` | 120 |
| `engine/persist.rs` | `on_save_settings`, `on_reload`, `remember*`, `save_state`, `emit`, `emit_log` | 160 |
| `engine/workers.rs` | `spawn_workers` (the def-stamp reconciliation) | 90 |
| `engine/builtins.rs` | `builtin_help/keywords/extensions/actions`, `now_ms` | 260 |
| `engine/tests.rs` | the current `mod tests` | 190 |

Note the `impl Engine` blocks spread across `engine/*`: Rust allows several
`impl` blocks for one type, so each file keeps `impl Engine { … }` for its own
methods, with `use super::Engine;` at the top. No trait or wrapper needed.

**`native/date.rs` (1718) → `native/time/date/` (6 files)**

| target | contents | ~lines |
|---|---|---|
| `date/mod.rs` | `Date`, `resolve`, `resolve_span`, `NativeExt` | 130 |
| `date/holidays.rs` | `easter_of`, `nth_weekday`, `quarter_start`, `monday_of`, `Rule`, `Holiday`, `HOLIDAYS`, `holiday_date`, `Prefer`, `match_holidays` | 300 |
| `date/words.rs` | `normalize_relative`, `normalize_words` | 90 |
| `date/parse.rs` | `day_first*`, `pivot_year`, `slash_*`, `split_three`, `MONTH_ABBRS`, `DAY_NAMES`, `looks_like_date`, `iso_parts`, `rel_parts`, `month_of_word`, `gnu_day`, `resolve_one`, `plural` | 380 |
| `date/render.rs` | `span_words`, `distance_words`, `weekdays_between`, `span_text`, `period_phrase`, `Spec`, `date_row`, `period_row`, `render` | 250 |
| `date/grammar.rs` | `try_range`, `try_week`, `try_bounds`, `try_quarter`, `try_weekend`, `try_month`, `try_next_weekday` | 500 |

**`native/calc.rs` (995) → `native/calc/` (5 files)**

| target | contents | ~lines |
|---|---|---|
| `calc/mod.rs` | `Calc`, the gate (`looks_like_math`, the operator/conversion regexes), `command`, `NativeExt` | 250 |
| `calc/money.rs` | `symbol_code`, `currency_name`, `currency_in_context`, `is_fiat`, `names_currency`, `with_currencies`, `symbol_re`, `name_re`, `context_name_re`, `priced_in`, `is_money` | 330 |
| `calc/units.rs` | `ambiguous_unit`, `magnitude_word`, `MAGNITUDE_SUFFIX`, the unit regexes, `magnitude_after`, `shift_decimal` | 120 |
| `calc/numbers.rs` | `NumberToken`, `number_tokens`, `groups_ok`, `Read`, `read_number`, `with_numbers`, `significant_digits`, `precision_for`, `PRECISION_*`, `EXPONENT_FROM` | 200 |
| `calc/answer.rs` | `undecidable`, `reading`, `for_qalc`, `normalize`, `is_echo`, `parse`, `load_history`, `history` | 150 |

**`provider/worker.rs` (817) → `provider/worker/` (3 files)** — the one
structural refactor in the plan:

| target | contents | ~lines |
|---|---|---|
| `worker/mod.rs` | `WorkerCmd`, `WorkerMsg`, `Shared`, `RunOut`, `Pending`, `Live`, `share_rows`, `build_rows_owned`, and the `run` select loop | 260 |
| `worker/state.rs` | a `WorkerState` struct owning what are now ~20 locals (`pending`, `live`, `debounce`, `refresh_at`, `showing`, `available`, `stale_shown_key`, …) with methods: `handle_ask`, `deliver`, `arm_refresh`, `cancel`, `on_opened`, `on_showing`, `on_available` | 420 |
| `worker/route.rs` | the ask path and its fallbacks (`native` → socket → command), the cache/stale reads, the socket push handling | 200 |

Why: `fn run` is 684 lines and the eight macros (`emit!`, `plog!`,
`deliver!`, `arm_refresh!`, `ask!`, `handle_ask!`, `abort_run!`,
`cancel_run!`) exist only because the state is a pile of locals inside it.
Turning the state into a struct deletes the macros, makes each path testable
in isolation, and is the difference between "a state machine" and "a function
that is 684 lines long". This is behaviour-preserving if done as a pure
mechanical extraction (locals → fields, macro bodies → methods), with the
existing live probes (`tick`, `slow`, `flaky`, `probe`, the socket actor's
test) as the gate.

**`oxy/src/main.rs` (639) → 9 files**: `main.rs` (50), `cli/query.rs` (80),
`cli/send.rs` (45), `cli/test.rs` (75), `cli/extensions.rs` (30),
`engine_local.rs` (25), `cases/mod.rs` (135), `cases/check.rs` (120),
`cases/view.rs` (45).

**`oxyd/src/main.rs` (491) → 6 files**: `server.rs` (90), `wire.rs` (90),
`logfile.rs` (60), `watch.rs` (70), `clipboard.rs` (40), `main.rs` (120).

**The 400–600 band** (do these when convenient, they are not urgent):

| source | split |
|---|---|
| `native/apps.rs` 534 | `desktop/apps.rs` (provider + scan + desktop parsing, 300) + `desktop/apps_icons.rs` (index, walk, resolve, 240) |
| `native/ssh.rs` 513 | `system/ssh.rs` (provider + row build, 330) + `system/ssh_config.rs` (expand, glob, parse, tilde, known_hosts, 200) |
| `native/kill.rs` 450 | `system/kill.rs` (provider + folding, 380) + `system/kill_windows.rs` (hyprctl lookup, 80) |
| `native/emoji.rs` 464 | `desktop/emoji.rs` (provider + ranking, 350) + `desktop/emoji_data.rs` (DEFAULTS + intents, 130) |
| `row.rs` 468 | `model/row.rs` (340) + `model/action.rs` (60) |
| `extension.rs` 452 | `registry/def.rs` (180) + `registry/command.rs` (110) + `registry/mod.rs` (90) |
| `settings.rs` 434 | `settings/mod.rs` (structs + `merge` + accessors + tests, 245) + `settings/defaults.rs` (default engines/askProviders, `str_array`, `url_encode`, 120) |
| `state.rs` 330 | `state/mod.rs` (90) + `frecency.rs` (120) + `pins.rs` (55) + `recents.rs` (60) + `mru.rs` (40) |
| `native/calendar.rs` 394, `recent.rs` 364, `file.rs` 350, `sys.rs` 306, `commands.rs` 293 | leave as-is; they are one concept each |

## 2.5 Special cases

- **Data tables stay whole.** `HOLIDAYS` (180 lines), `DEFAULTS` + `intents`
  (126), the 24 `COMMANDS` entries, `MAGNITUDE_SUFFIX`, the month/weekday
  tables: these are data, and splitting them makes the file harder to check
  against its source. They get their own `*_data.rs` (or stay at the bottom of
  their module) and are **exempt from the cap** — the guard in 2.7 carries an
  explicit allowlist with the reason.
- **Tests count toward the file.** A 1200-line file with 400 lines of tests is
  still 1200 lines to read. The engine suite (189) and the socket actor's test
  move to `tests.rs`/`mod tests` as listed; everything else stays inline.
- **`#[cfg(test)]` helpers** used by several modules go in a
  `#[cfg(test)] pub(crate) mod test_support` in `lib.rs`, not duplicated.
- **Public API stability**: `lib.rs` keeps re-exporting the same names
  (`Row`, `Action`, `Query`, `Engine`, `EngineCmd`, `EngineEvent`,
  `Extension`), so the move is invisible to `oxyd`/`oxy`.

## 2.6 Order of work, with a gate per step

The point of the order is that each step is reviewable on its own and green
before the next. Suggested commits:

1. **The folder layout, moves only** — create `model/`, `registry/`,
   `settings/`, `state/`, `support/`, move the small files with `git mv`,
   fix `mod`/`use` paths, add the `mod.rs` docs. No logic touched.
   *Gate:* `cargo test --workspace`; `git diff -M` shows renames.
2. **`engine.rs` → `engine/`** (2.4) — the single biggest win. Do it as one
   commit: move the file to `engine/mod.rs`, then cut the methods out one
   file at a time, compiling between cuts.
   *Gate:* `cargo test` + the live smoke list in 2.6.1.
3. **`native/date.rs` and `native/calc.rs`** — independent of each other, two
   commits, cases still green (`oxy test --cases date`, `calendar`).
4. **`provider/worker.rs` → `worker/`** — the macro-to-struct refactor. Keep
   the existing probes running while you go; the socket actor's test covers
   the push path.
5. **`oxyd` and `oxy` mains** — mechanical, one commit each.
6. **The 400–600 band** — one commit per file, in any order.
7. **Guards and docs** (2.7 + `core/README.md`).

### 2.6.1 The smoke list (run after steps 2–4)

The harness from the audits, as a checklist: `?` lists 46+ keywords and the
scope label reads `Keywords`; `run:` returns 24 rows; `later:hi` returns the
quicklink row; `/clear` → activate → `/clear-all` shows the prompt and the
second Enter clears; a `probe.json` edit takes effect after the watcher; a
disabled extension stops answering; `apps:` resolves an icon; `tick:`
refreshes while a client is open and does not after a CLI query; `preview:`
select A → select B → close runs `A-preview`, `B-preview`, `A-revert`;
`savesettings` keeps the file's order and newline.

## 2.7 Guards, so it stays this way

- **A file-length check** in `tests/run.sh`'s static layer (and CI): walk
  `core/crates/**/*.rs`, fail over 1200 lines, warn over 800, with an
  allowlist file (`core/.loc-allow`) naming the data tables and why. Ten
  lines of Python, in the spirit of the existing extension-JSON check.
- **A layering check**: a grep that fails if `model/` mentions
  `std::process`, `std::fs` or `tokio::process` — the rule in 2.2.3, made
  mechanical. Cheap now, valuable the first time someone adds IO to a row.
- **`cargo clippy --workspace -- -D warnings`** as a CI job. The tree is
  small enough that the first run will be mostly mechanical fixes, and the
  macros in `worker.rs` are exactly what clippy nags about — the refactor in
  step 4 removes most of its ammunition.
- **`core/README.md`**: the crate map, the layering rule, the file budget,
  and the two commands (`cargo test`, `tests/run.sh`). The repo's other
  READMEs explain the product; nothing explains the core.

## 2.8 Risks, and what not to do

- **Do not split by line count alone.** `calendar.rs` (394) and `file.rs`
  (350) are one idea each; splitting them would produce two files that only
  make sense together. The rule is "one noun", not "under 800".
- **Do not do the moves and the splits in one commit.** A rename plus an
  extraction is unreviewable; the order in 2.6 keeps each diff to one kind of
  change.
- **Do not let the split change the wire.** Row/event field names, the JSON
  the daemon writes and the log's shape are the contract with `Shell.qml`,
  the scripts and `PROTOCOL.md`; a rename there is a breaking change, not a
  refactor. `serde` renames and the existing tests pin this.
- **Do not touch `plugin/` in this pass.** `Shell.qml` (1301) and
  `Launcher.qml` (2757, the reference build) have the same problem, but the
  frontend is where the audit's live-verification is thinnest; split it only
  with a running shell to check against.
- **Expect conflicts.** Another stream is committing to this branch; land
  the steps small and often, and rebase rather than merge.

## 2.9 Out of scope, noted for later

- `plugin/`: `Launcher.qml` 2757, `Shell.qml` 1301, `ResultRadioPlayer.qml`
  795, `ResultDocker.qml` 677 — the same treatment (a `views/` folder, one
  view per file, `Shell.qml` cut into lifecycle/results/keys sections) once
  there is a shell to smoke-test on.
- `bin/`: `oxy-agent` 2167, `oxy-gh` 1047 — the Python daemon and the GitHub
  client are the last two monoliths; they become relevant when the porting
  backlog reaches them.
