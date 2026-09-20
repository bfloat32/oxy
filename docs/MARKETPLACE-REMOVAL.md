# What `bo` and the marketplace are, and what removing them takes

Report before removal, as asked — then executed. **Status: removed**, and
verified: 40 extensions, 16 case files, 387 assertions, `tests/run.sh` green,
`?` lists no `bo`/`market`/`marketplace`, and `bo:` now falls through to the
web row like any other unmatched text. The `bo test` documentation was deleted
with the rest, as chosen.

---

## 1. What `bo` is

**`bo` is `better-omarchy` — an external CLI that is not part of this
repository.** It is a package manager for Omarchy *units*: a "marketplace" is
a collection, and a unit is a plugin, a Hyprland config, or a setting that can
be installed and switched on. In a terminal it is `bo list`, `bo info <unit>`,
`bo add <unit>`; its test runner is `bo test <unit>`.

Nothing here installs it or ships it. The extension is a *client*:

```json
"search": "oxy-bo {query}",
"when": "command -v bo"
```

On a machine without `bo` the keyword is hidden — the gate has never passed on
this one.

## 2. What "the marketplace" is here

The launcher front-end for `bo` — the largest single feature in the repo that
exists to drive someone else's tool:

| what | file | lines |
|---|---|---|
| the script | `bin/oxy-bo` | 891 |
| the manifest | `config/omarchy/oxy/extensions/bo.json` | 21 |
| its cases | `config/omarchy/oxy/extensions/bo.cases.json` | 286 (28 cases) |
| the tile grid of one marketplace | `plugin/ResultMarketplace.qml` | 232 |
| the home screen | `plugin/ResultMarketplaceHome.qml` | 322 |
| one unit's page | `plugin/ResultMarketplaceUnit.qml` | 524 |
| the tile | `plugin/MarketplaceTile.qml` | 144 |
| the back chip | `plugin/MarketplaceBack.qml` | 64 |
| the section label the three views share | `plugin/MarketplaceLabel.qml` | 34 |
| | **total** | **2 518** across 10 files |

What it does, in the script's own terms: three levels you walk with `Enter`
and back out of with `Escape` (home → marketplace → unit); the level written
into the box as an address (`bo:@<marketplace>`, `bo:#<market>/<unit>`)
because typing clears the launcher's step stack; reserved-word narrowing
(`plugin`, `hypr`, `setting`, `on`, `off`, `unavailable`, plus categories),
matched exactly and retried as plain text when nothing matches; and a
close-then-toggle-then-re-summon dance, because switching a plugin unit on
changes `~/.config/omarchy/plugins`, which the shell's inotify watcher reads
as a plugin change, which destroys every panel plugin — including this
launcher.

## 3. Every reference to it

| file | where | what |
|---|---|---|
| `bin/oxy-bo` | all 891 lines | the extension |
| `config/omarchy/oxy/extensions/bo.json` | all | the manifest (`bo`, aliases `market`/`marketplace`, `view: marketplace`) |
| `config/omarchy/oxy/extensions/bo.cases.json` | all 28 cases | the assertions |
| `plugin/Shell.qml` | 444, 462, 647, 1037, 1043, 1169-1171, 1214-1216 | view names in `knownViews`, a deeplink example, three layout conditions, the loader cases, the three Components |
| `plugin/Launcher.qml` | 463, 481, 1789, 2463, 2469, 2619-2621, 2664-2666 | the same, in the script build's frontend |
| `plugin/ResultMarketplace*.qml`, `MarketplaceTile.qml`, `MarketplaceBack.qml` | all | the views (used by nothing else) |
| `README.md` | 139, 313, 350-400, 720 | the `Left`/`Right` row, the keyword row, the whole "The marketplace: `bo:`" section, the view list ("~37 layouts") |
| `docs/EXTENSIONS.md` | 104-106 | the three view names in the view table |
| `docs/EXTENSIONS.md` | 39, 59, 64, 323-388, 434, 553 | **`bo test`** — the *external tool's test runner*, used throughout as the reference for the four check layers |
| `tests/cases.py` | 4 | the same reference in a comment |
| `install.sh` | 536 | a comment: better-omarchy may already have installed the `modules.lua` loader |
| `core/crates/oxy-core/src/provider/process.rs` | 24-26 | two comments explaining the `OXY_PLUGIN_ID` export, using `bo:`'s toggle as the example (the code itself is generic) |
| `docs/PARITY-RS.md`, `PARITY-RS-2.md` | passim | audit history |
| `docs/PORTING-BACKLOG.md` | §1, §1.4, §2.0, §2 batch G, §3.9, §6.1-6.5 | the porting plan still lists `bo` as a target and counts it |

## 4. What removal changes

| | now | after |
|---|---|---|
| extensions | 41 (15 native, 31 script) | 40 (15 native, 30 script) |
| case assertions | 415 in 17 files | 387 in 16 files |
| view names | the README's "~37 layouts" includes three marketplace ones | three fewer — the views stop existing |
| `?` keywords | includes `bo`, `market`, `marketplace` | neither |
| `docs/PORTING-BACKLOG.md` batch G | `agent`, `bo` | `agent` |
| script build (`Launcher.qml`) | has `bo:` | does not — both frontends lose it together, which is the point |

Nothing else depends on any of it: the three views are used only by
themselves, and no extension, test, or engine path references the keyword.

## 5. The three things that need a decision, not just a delete

1. **`bo test` in `docs/EXTENSIONS.md` (six places) and `tests/cases.py`.**
   This is the *external tool's test runner*, not the marketplace — and it is
   the only written specification of the four check layers (`manifest`,
   `answer`, `actions`, `cases`) that `oxy test` is still missing
   (`PORTING-BACKLOG.md` §4.2). Deleting those sections deletes the spec.
   Options: **(a)** rewrite them to describe `oxy test`'s own four layers and
   drop the name — the spec survives, the reference goes; **(b)** delete them
   with the rest; **(c)** leave them (they are about `bo`, but they document
   our backlog).
2. **`install.sh:536`.** The comment explains why `modules.lua` may already
   exist on the machine. The installer's behaviour does not depend on `bo`;
   only the name does. Reword, or leave.
3. **`process.rs:24-26`.** The `OXY_PLUGIN_ID` export exists for *any* script
   that summons the launcher back; `bo:` is only the example. Reword the
   example (e.g. to the agent's re-summon), or leave.

## 6. The removal plan, in order

1. Delete `bin/oxy-bo`, `bo.json`, `bo.cases.json`, and the five QML files.
2. `plugin/Shell.qml` and `plugin/Launcher.qml`: drop the three names from
   the view list, the loader cases, the Components, and the three layout
   conditions each (the `emoji`/`themes` grouping that also names
   `marketplace`).
3. `README.md`: delete the marketplace section and its rows; fix the layout
   count.
4. `docs/EXTENSIONS.md`: drop the three view rows; handle the `bo test`
   sections per §5.1.
5. Comments per §5.2/§5.3.
6. `docs/PORTING-BACKLOG.md`: remove `bo` from batch G, the appendix row, the
   §1/§1.4 counts, the §6.2/§6.3 rows, and the §3.9 `OXY_*` note.
7. Verify: `bash tests/run.sh` (387 cases, no `bo` case file), `cargo test`,
   `?` no longer lists the three keywords, `bo:` returns nothing, and both
   frontends still load (the view list is a static array — a stale name there
   would be a runtime error, which is why it is step 2 and not an
   afterthought).

Recoverable at any point: the removal is a git commit, and nothing outside
the repo depends on the keyword.
