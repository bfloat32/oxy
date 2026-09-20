# core/ — the Rust core of Oxy

The daemon (`oxyd`), the CLI (`oxy`) and the engine they share (`oxy-core`).
The QML frontend is a thin client over a JSON-lines socket; everything it
asks for is decided here.

## The map

`oxy-core/src/` is organised in layers; dependencies point one way only:

```
model ← registry, settings, state ← provider ← engine ← oxyd, oxy
```

| dir | holds | may not |
|---|---|---|
| `model/` | the wire shapes: `Row`, `Action`, `Query`, `EngineCmd`, `EngineEvent` | open a file, spawn a process |
| `registry/` | extension files on disk: `Extension`, `load_dir`, command building | |
| `settings/` | the user's `oxy.json` and every path we touch (`paths.rs`) | |
| `state/` | what the launcher remembers: frecency, pins, recents, MRU | |
| `provider/` | how a question reaches an extension — `process`, `socket`, `worker`, and `native/` (the compiled-in providers, grouped by subsystem) | |
| `engine/` | the orchestrator — one file per phase of its life | |
| `support/` | small dependency-free helpers: cache, availability, rank, score, quote | |

`oxyd/src/` keeps boot wiring in `main.rs`; the accept loop, wire parsing,
the logfile and the watcher each have their own file. `oxy/src/` keeps
dispatch in `main.rs`; verbs under `cli/`, the case runner under `cases/`.

## The budget

- A file is a noun; if the name needs "and", it is two files.
- **800 lines target, 1200 hard cap.** `tests/run.sh` fails over the cap and
  warns over the target. Data tables are exempt — they are listed, with the
  reason, in `.loc-allow`.
- `mod.rs` is a table of contents: module docs plus `mod`/`pub use` lines.
- Tests live with their subject; `#[cfg(test)] mod tests` in the file tested.
- `pub(crate)` for anything that only crosses module lines; `pub` only for
  the crate API that `lib.rs` re-exports.

## The rules, made mechanical

- `tests/run.sh` checks every `core/**/*.rs` file against the budget
  (`core/.loc-allow` names the exempt data tables).
- A layering check fails if anything under `model/` mentions `std::fs`,
  `std::process` or `tokio::process` — the wire shapes never touch IO.
- `cargo clippy --workspace --all-targets -- -D warnings` is a CI step.

## Run it

```sh
cd core
cargo test --workspace          # the suite
cargo run -p oxyd               # the daemon (foreground)
cargo run -p oxy -- query 'run:'  # a one-shot query through the daemon
bash ../tests/run.sh            # every check CI runs
```

The protocol the socket speaks is `docs/PROTOCOL.md`; the parity story is
`docs/PARITY-RS*.md`.
