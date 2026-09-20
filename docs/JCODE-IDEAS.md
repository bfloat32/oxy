# What jcode has that we could port — analysis and proposals

Read-through of `C:\Users\Administrator\Downloads\jcode-master` (a Rust coding
agent harness: **1 237 files, 713 k lines, 83 workspace crates**) for ideas
that fit **our** core: a launcher daemon with a native LLM track and an agent
port still ahead of it.

**No implementation in this document** — it is the analysis. The proposals
below are being worked through in the order §6 gives:

| proposal | status |
|---|---|
| P1.3 memoize `search_text`/`acronym` | **landed** (`4b9c36f`) — 417–444 µs → 72 µs on the new `score400` bench, 156-case parity table unchanged |
| P7 corrupt-file recovery + save-path fixes | **landed** (`d6f4ce9`) — `support/store.rs`, `state.recovered`/`settings.recovered` log lines, no more silent reset, no more panic on a non-object `oxy.json`; secret hardening still waits for the key store |
| P4 retry / `Retry-After` | **landed** (`840b1f3`) — `provider/llm/retry.rs`, capped at 60 s, verified against a stub that 429s once; the fallback chain landed with the probe below |
| P2 `ask doctor` | **landed** (`db84e8b`) — `oxy ask doctor --tier offline\|catalog [--json]`, checkpoints PASS/FAIL/skip, verdict + next step, non-zero exit |
| P3 provider metadata | **partly** — `ask.key` (`env:NAME` or a literal), the credential checkpoint, and the chip's "what to do" hint are in; the tables-as-code half is not |
| §3's small ideas | **landed**: setup hints in `?` (live facts, so no dismissal file), keybinding conflicts in `shortcuts:`, soft interrupt, the model usage ledger, `/stats` slowest-three, the Windows paste row |
| §4's process ideas | **landed**: `oxy extensions --coverage` and `tests/bench_oxy.py` (90 ms cold start, 0.3 ms p50, 10.2 MB RSS on this box) |
| P5 budget · P1 positions/typo band · P6 command risk · herdr state · install channels · hooks | next |

Method: crate map and sizes first, then the crates and docs that overlap what
we are actually building — the `ask:` track (`docs/LLM-INTEGRATION.md`), the
`do:` port (`docs/PORTING-BACKLOG.md` §batch G), and the daemon.

## 1. What jcode is, in numbers

| fact | value |
|---|---|
| workspace | 83 members; 1 237 `.rs` files, 713 k lines — of which `jcode-tui` (213 k), `jcode-app-core` (150 k) and `jcode-base` (119 k) are the app itself, and the crates below are the interesting minority |
| shape | single **server** + multi-client TUI over a Unix socket; providers behind a registry; sessions named and resumable |
| providers | 20 `jcode-provider-*` crates — one per vendor plus `-runtime` halves, `-core` (the trait and shared concerns), `-metadata` (tables), `-doctor` (diagnostics) |
| the crates this document leans on | `jcode-fuzzy` 774 lines, `jcode-compaction-core` 1 042, `jcode-storage` 1 194, `jcode-provider-metadata` 2 077, `jcode-command-risk` 2 542, `jcode-provider-doctor` 6 759 |
| docs | 72 markdown files in `docs/` — several are the *reasoning* behind a subsystem, not a reference |
| own claim | "most RAM efficient harness" — 27.8 MB PSS with local embedding off, vs 140 MB for Codex CLI (their measurement, `README.md`) |

What matters for us is that jcode solved, in Rust, three problems we have
open: **provider plumbing** (auth, retry, failover, catalog, doctor), **context
management** (budget, compaction, usage stats), and **safety** (a deterministic
command-risk classifier for an agent that runs shell commands). It also solved
two problems we have *partly* solved and can compare notes on: fuzzy matching
and the daemon protocol.

## 2. The seven proposals

### P1 — Typo tolerance, match positions, and the cache our port dropped

**Source:** `crates/jcode-fuzzy` (774 lines, 1 file, **zero dependencies**).

What it is: subsequence matching with a bounded error budget — substitutions,
adjacent transpositions, extra typed characters — scored with bonuses for
exact, consecutive, boundary and prefix matches, and a **score floor** that
rejects weak scattered matches (`score_floor`, scaled by pattern length).
Three API shapes we do not have:

- `fuzzy_match` / `fuzzy_match_positions` → **the matched character indices**,
  for highlighting;
- `fuzzy_score_tokens` — a multi-word query where **each word must match within
  one field**, so characters are never stitched across unrelated columns;
- `PreparedTokenQuery` — the query lowered once and the DP scratch rows reused
  across every entry, "so the matcher is not reallocated for each token of each
  entry" (their comment).

**Why it fits us.** Our `support/score.rs` is a faithful port of Omarchy's
scoring: prefix/infix/haystack/acronym bands, **no typo tolerance, no
positions**. Three separable ideas:

1. **Positions** — our rows carry no match offsets, so no view can bold the
   matched substring. Every modern launcher does. This is a *new wire field*
   (`match: [3,4,5]`) plus frontend work in `ResultList`/`ResultApps`.
2. **A typo-tolerant band**, additive only: entries that score `-1` today
   (`fzf` for `firefox`, `chrme` for `chrome`) could match in a new band
   *below* `weak`, so nothing that matches today moves. Parity-safe by
   construction — the script build would show fewer rows, so this is a
   deliberate, documented improvement, not a port.
3. **Memoize `search_text`/`acronym` per `Entry`.** This one is a **bug in our
   port**: `plugin/Score.js` caches both on the entry (`entry._searchText`,
   `entry._acronym` — see its comments about building the same string three
   times per keystroke), and our `Entry::search_text()`/`acronym()` rebuild
   them on **every call**, per entry, per keystroke. The app list is built once
   (`apps.rs` line 43: "same entries keeps the index it already paid for"), so
   a `OnceLock` on the two fields removes two `format!`+`to_lowercase` and two
   regex passes per app per keystroke. `benches/hot.rs` can measure it.

**Size:** positions M (scorer + wire + two views); typo band S–M; memoization
**S**. **Risk:** the band and positions change what the UI shows — both need
the parity table kept green plus new cases; memoization must not change a
single score (the 156-case table covers it).

### P2 — `oxy ask doctor`: tiers, checkpoints, spend, and a coverage ledger

**Source:** `crates/jcode-provider-doctor` (6 759 lines) + `docs/PROVIDER_DOCTOR.md` (166 lines).

What it is: one command that answers *"why isn't my provider working?"* with
three tiers — `offline` (wiring only, no key, no spend), `catalog` (live
`/models` fetch: bad key, dead endpoint, model missing), `full` (a real chat,
a real stream, a real tool call) — and **twelve named checkpoints** reported in
order:

```
[ PASS] Credential loaded          Loaded credential from CEREBRAS_API_KEY
[ PASS] Live model catalog endpoint 2 live model(s) returned
[ skip] Non-streaming chat         catalog tier: requires --tier full (spends balance)
Verdict: tier `catalog` passed. Run `--tier full` to confirm full readiness.
```

The details worth stealing, all of them deliberate:

- **`skip` is a first-class state** — a lighter tier records API-dependent
  checkpoints as *skipped*, "so nothing is over-credited in the coverage
  ledger".
- **The verdict names the first failure and the next step**, and the command
  **exits non-zero** when the tier did not fully pass → usable as a CI gate.
- **Spend accounting**: `Spend this run: 3 billable API calls, 554 tokens
  (289 in + 265 out), cost not reported by provider` — and it says "cost not
  reported" rather than inventing one. Streaming probes ask for usage
  explicitly (`stream_options.include_usage`) so streamed calls count too.
- **A coverage ledger** behind it (`jcode provider-test-coverage`) and
  **coverage as data**: `jcode-provider-metadata` gives every login provider a
  per-surface order (`cli_login`, `tui_login`, `server_bootstrap`,
  `auto_init`, `auth_status` — each `Option<u8>`), so "does this provider
  support this surface" is a table lookup, not prose.

**Why it fits us.** Our LLM track has `Local::probe` (250 ms TCP connect) that
**nothing calls**, and a card error that says "Is the model server running?".
Our surfaces are fewer (Ctrl+Enter, and whatever `ask:` grows into), so the
same shape is cheaper:

- `oxy ask doctor [--tier offline|catalog|full] [--json]` where offline =
  settings parse + URL strictness + request shape; catalog = reachable +
  `GET /v1/models` lists the configured model; full = one streamed turn, with
  token accounting from the usage block.
- Checkpoints named after ours: `settings_parsed`, `endpoint_loopback_ok`,
  `server_reachable`, `model_listed`, `stream_completed`, `stream_usage_reported`.
- A **coverage ledger** is data we already have and do not show: which
  extensions have case files, which natives have none
  (`PORTING-BACKLOG.md` §1.4/§4.7). `oxy test --coverage` would print it as a
  matrix instead of a paragraph in a doc.

**Size:** M (a `cli/doctor.rs` + checkpoints in `provider/llm/`). **Risk:**
none to parity — it is a new command; the only cost is the API surface.

### P3 — Provider metadata and credential sources as code

**Source:** `crates/jcode-provider-metadata` (2 077 lines, deps: `url` only) + `docs/AUTH_CREDENTIAL_SOURCES.md`.

Two tables and one honesty rule:

- A **login-provider table**: `id`, `display_name`, `auth_kind`,
  `auth_state_key`, `auth_status_method`, `aliases`, `menu_detail`,
  `recommended`, and the surface order above.
- An **API-provider table**: `id`, `display_name`, `api_base`, `api_key_env`,
  **`env_file`**, `setup_url`, `default_model`, `requires_api_key`.
- The rule, from a doc written because "the same confusion keeps recurring":
  a provider can have **two independent credential paths** (OAuth token vs API
  key), the key usually lives in an app config dir rather than an env var, and
  **an inconclusive check must report "unknown", never "signed out"** — a
  naive grep for `sk-ant-api` misses an OAuth-only setup entirely.

**Why it fits us.** Our design doc §5.1 is exactly such a table, in markdown,
hand-written, and it will drift. Making it code gives us: the ask chip's model
list, `setup_url` for a "how do I set this up" row, `api_key_env`/`env_file`
for key resolution (`env:NAME` in the design), and the surface order for the
doctor's checkpoints. The honesty rule is a one-line policy for our registry
chip: no endpoint configured is *unknown*, not *unavailable*.

**Size:** S–M (one module, tables, a resolver). **Risk:** none; it replaces
prose with data.

### P4 — Retry-After, backoff, and picking a fallback

**Source:** `crates/jcode-provider-core/src/retry_after.rs` (~100 lines),
`failover.rs` (`ProviderFailoverPrompt`, `FailoverDecision`),
`fallback_pick.rs` (`FallbackPickOptions`).

What it does: parses `Retry-After` as delta-seconds **or** an HTTP date,
**caps the wait at 60 s** "so a malformed or hostile upstream cannot stall a
turn indefinitely", uses saturating arithmetic so an arbitrarily long digit
string is safe, and falls back to normal exponential backoff when the header is
missing or invalid. Around it: a failover *decision* type (retry? switch model?
ask the user?) and a fallback picker.

**Why it fits us.** Our LLM client has **no retry, no backoff, and no
Retry-After handling at all** — a 429 or a 503 lands in the card as an error
and the user retries by hand. The port is small and self-contained: a
`retry_after` helper, a bounded backoff around `post_json`, and — because our
ask path already has a CLI fallback tier — a **fallback chain** that is
actually specified (local endpoint → first live CLI provider → error card),
rather than the current "endpoint set means the CLI list is not probed".

**Size:** S–M. **Risk:** a retry must respect `stop_ask` (the abort), and the
cap must be visible in the card when it applies ("rate limited; retrying in
12 s") — silence would look like a hang.

### P5 — Context budget and compaction for the multi-turn `ask:`

**Source:** `crates/jcode-compaction-core` (1 042 lines, deps: a message type +
`serde_json`) — the constants alone are worth reading:

```
DEFAULT_TOKEN_BUDGET        200_000   COMPACTION_THRESHOLD  0.80
CRITICAL_THRESHOLD            0.95    RECENT_TURNS_TO_KEEP    10
MIN_TURNS_TO_KEEP              2     EMERGENCY_TOOL_RESULT_MAX_CHARS 4 000
CHARS_PER_TOKEN                4     IMAGE_TOKEN_COST      1 600
SYSTEM_OVERHEAD_TOKENS    18 000     PAYLOAD_IMAGE_CHAR_BUDGET 12 MB
```

and the API around them: `estimate_compaction_tokens`,
**`effective_context_tokens_from_usage`** (prefer the provider's own usage
numbers over the estimate), `safe_compaction_cutoff` (never split a
tool-call/tool-result pair), `build_compaction_prompt`, and the emergency
paths — truncate tool results, strip large images, and
`is_request_payload_too_large_error` to recognise a 413 and recover.

The insight in the comments is the valuable part: counting an inline image's
**base64 length** as `len / 4` tokens "massively overestimates the real context
cost … spuriously tripping the compaction threshold and driving repeated
back-to-back ('triple') compactions". They charge a flat 1 600 tokens per
image instead.

**Why it fits us.** `docs/LLM-INTEGRATION.md` §6 specifies a session file and
budgets but no numbers and no policy. This gives us both, plus the two rules
that are easy to get wrong: **cut on a safe boundary**, and **trust reported
usage over your own estimate**. We do not need the embedding-based semantic
mode (`semantic_*`, `jcode-embedding` with ONNX) — plain threshold compaction
with a summary turn is enough for a launcher, and the design already says
"one process per turn".

**Size:** M (estimator + policy + cut + emergency truncation). **Risk:** the
summary turn needs the model itself, so the first version should *drop* old
turns with a visible marker ("12 earlier turns elided") rather than silently
summarising — honesty over cleverness, and it needs no second call.

### P6 — A deterministic command-risk classifier for `do:` and destructive actions

**Source:** `crates/jcode-command-risk` (2 542 lines with tests, **zero
dependencies**), motivated by a real incident: "a model that decides to run
`rm -rf ~` is obeyed immediately … issue #604, where a user lost their home
directory."

The design, in their words: **classify by blast radius, not by command name** —
a denylist of `rm -rf` misses `find -delete`, `shred`, `truncate`, `dd` and
`>file` — and **bias hard toward recall**, because "a false positive costs one
reflection turn, a false negative costs a home directory". Four levels:

| level | meaning | action |
|---|---|---|
| `Safe` | no destructive potential detected | run immediately |
| `Low` | destructive but bounded (inside cwd, recoverable via git, temp dir) | run, record it |
| `Confirm` | destructive target cannot be determined statically | make the model re-justify |
| `Catastrophic` | home, root, or credentials | **never runs**, no justification unlocks it |

plus `ProtectedPaths` / `is_catastrophic_target` (an absolute, path-based deny
that "does not depend on parsing the command correctly") and a shell
tokenizer.

**Why it fits us.** Two places, one small and one large:

1. **The launcher already has the confirm machinery** — `pending_confirm`, the
   armed prompt, the two-Enter flow the audits tested. Today only actions that
   *declare* `confirm` arm it. A risk classifier would let any row whose `exec`
   is destructive arm it automatically: `/clear-all` is the proof it works;
   the classifier decides *which* rows deserve it. This is a launcher-native
   feature nobody else has: "Enter on a destructive action asks first".
2. **The `do:` agent port** needs exactly this as its policy engine's first
   stage (their two-stage cascade: cheap deterministic filter → reflection).

**Size:** M to port the crate (it is dependency-free and self-contained; the
tokenizer and path rules are the work), S to wire the confirm flow, S to keep
it out of the way of the agent (a `pre_tool` hook, see below). **Risk:**
false positives are user-visible; their own bias is "escalate when ambiguous",
which is right for an agent and should be softened for a launcher (a `Confirm`
on `file:` rows would be noise — gate it to rows whose exec is a *destructive*
verb, and let the setting turn it off).

### P7 — Storage hardening and corrupt-file recovery

**Source:** `crates/jcode-storage` (1 194 lines) — a small API with three ideas
we lack:

- **`read_json_with_recovery_handler`** — a corrupt JSON file is recovered by a
  caller-supplied handler rather than silently becoming defaults. Ours:
  `Settings::load`, `State::load`, frecency, cache — every one does
  `.ok()`/`unwrap_or_default()`, so a truncated `oxy-state.json` (a daemon
  killed mid-write) **silently resets pins and recents** with no trace. A
  recovery path that moves the bad file aside and logs one line is a real
  robustness fix, and it is small.
- **Secret hardening**: `harden_secret_file_permissions(path)`,
  `write_text_secret`, `write_json_secret`, plus an async Windows ACL hardening
  pass with backoff (`SECRET_HARDEN_*`) because Windows has no `chmod`. Our
  `oxy-spotify.json` is a live credential and an `ask` key may be too; we
  chmod the socket but not those.
- **`upsert_env_file_value`** — edit a key in a `.env`-style file without
  rewriting the rest, which is how their login flow stores API keys
  (`~/.config/jcode/anthropic.env`). Ours would be the `ask` key store.

**Size:** S each. **Risk:** a recovery handler must not lose a file a user
cared about — move, never delete, and say so in the log.

## 3. Smaller ideas, one line each

| idea | source | our hook point |
|---|---|---|
| **Setup hints with permanent dismissal** — platform-aware nudges ("Terminal.app renders poorly; try Ghostty", "create a .desktop launcher"), each dismissible forever in `setup_hints.json` | `crates/jcode-setup-hints` (11 k) | our `notice` + a `setup-hints.json` in state: "`gh` is not installed — `gh:` is hidden", "ollama detected on :11434 — set `ask.endpoint`", "`fd` missing: `repo:` will be slower" |
| **Keybinding conflict detection** — reads macOS `symbolichotkeys` and `ghostty +list-keybinds` to warn when a chord never reaches the app | `docs/KEYMAP_CONFLICTS.md` | our `shortcuts:` extension already lists Hyprland binds and marks our own keys "Oxy"; the missing piece is a **conflict row**: "Ctrl+K is also bound to X in Hyprland" |
| **Per-turn response stats, with the honesty rule** — duration, input/output/cache tokens, "timestamps are not used to invent elapsed time", absent metrics omitted | `docs/HISTORY_RESPONSE_STATS.md` | our event log already has `prov.done`/`ms`; a `/stats`-style summary per provider and per ask turn, with absent ≠ zero |
| **Soft interrupt** — queue a typed message and inject it at the next idle point instead of cancelling the generation | `docs/SOFT_INTERRUPT.md` | `on_ask` currently calls `stop_ask` and restarts; queueing the new question until the current stream ends is a small, visible improvement |
| **A hook contract** — `pre_tool` is a *gate* with a timeout, observers are fire-and-forget, a JSON payload capped at 16 KB, a recursion guard (`JCODE_HOOKS_DISABLED=1`), and commands executed directly rather than through a shell | `docs/HOOKS.md` | the `do:` port's policy engine, plus `OXY_*`-style env conventions we already ship |
| **A server registry file** — `~/.jcode/servers.json` so a client finds its daemon, plus a **debug socket** beside the main one | `docs/SERVER_ARCHITECTURE.md` | `oxyd` already has socket fallback + `oxy send`; a registry file would fix "two daemons, which one am I talking to", and a debug socket is a cleaner surface than the event log for tracing |
| **State reporting to herdr** — `pane.report_agent_session` over `HERDR_SOCKET_PATH`, with the warning that claiming a lifecycle you cannot back up *makes the fallback worse* | `docs/HERDR.md` | we already have a `herdr:` extension; reporting the `do:` agent's real state (working/blocked/idle) to herdr's socket is a small integration with a big UX payoff |
| **Install channels** — a launcher symlink, immutable `versions/<v>`, `stable` vs `self-dev`, and an update policy that **never downgrades** (compares git ancestry and stops when it cannot verify) | `README.md`, `AGENTS.md` | our `install.sh` links into the clone (so a pull can break a running daemon); the channel model plus "verify at runtime, `cargo build` proves nothing" are both worth adopting |
| **`ModelCapabilities.context_window`** + `DEFAULT_CONTEXT_LIMIT = 200_000` | `crates/jcode-provider-core/src/models.rs` | feeds P5's budget and the ask card's "long conversation" warning |
| **Model usage ledger** — `{count, last_used, tracking_started, selection_count, last_selected}`, explicitly "does not measure task success" | `docs/MODEL_USAGE.md` | a `model-usage.json` in state: which backend answered, how often, when last — drives the registry chip and a stats row |
| **Two-tier safety queue for unsupervised runs** — auto-allowed vs requires-permission, a persistent review queue, notification channels, per-session reports | `docs/SAFETY_SYSTEM.md` | the `do:` port's approvals; our `pending_confirm` is the in-launcher half of it |

## 4. Process ideas worth as much as the code

- **Benchmarks as scripts, isolated by construction.** `scripts/bench_startup.py`
  runs under a temporary `JCODE_HOME`/`JCODE_RUNTIME_DIR` "so it does not
  interfere with the user's real server, logs, or credentials", parses a
  built-in startup profile, and can regression-check. `bench_memory_cli.py`
  samples RSS per mode. We care about both numbers (faster responses, less
  memory) and have no such script — one `bench_oxy.py` that measures daemon
  RSS, cold start, and per-keystroke latency in a sandbox would make our own
  claims checkable.
- **A coverage ledger as a command** (`jcode provider-test-coverage`): which
  provider × surface is verified, with `skip` distinct from `pass`. Our
  equivalent is a matrix of extensions × (cases / behaviour suite / native),
  printable from `oxy test --coverage`.
- **`*-types` crates own stable data, and success is measured in compile
  time** (`docs/CRATE_OWNERSHIP_BOUNDARIES.md`): types crates hold plain data
  with no IO; behaviour stays in the domain module; the *goal* is shrinking the
  recompilation surface, and they explicitly warn against "hidden coupling
  through broad re-exports". Our layering rule (`core/README.md`) is about
  correctness; adding the compile-time rationale would explain *why* the next
  module should be a types module rather than another file in `provider/`.
- **The incident write-ups.** `command-risk`'s module doc names issue #604 and
  what it cost. That habit — the *why* in the header, with the failure that
  motivated it — is the same one our scripts already follow, and it is the
  reason this analysis could be done at all.

## 5. What I would not port

| not ported | why |
|---|---|
| `jcode-embedding` (ONNX via `tract-onnx` + `tokenizers`) and the semantic compaction mode | a large dependency tree for a launcher; the deterministic path (P5) gets most of the value |
| swarm / overnight / productivity / plan subsystems, `jcode-harness-api` + SDK | a different product: multi-agent orchestration and a public API |
| MCP pool, browser providers, PDF, voice, email notifications | nothing in our core consumes them; each is a subsystem, not an idea |
| the TUI (`jcode-app-core`, 150 k lines) | we are QML; the interesting parts (panels, keymaps) are already solved on our side |
| multi-session client architecture | a launcher is one session by definition |
| the Jev memory service | external, and they chose it *after* rejecting embeddings, BM25 and generative rerankers — worth reading (`MEMORY_ARCHITECTURE.md`) if the `do:` agent ever grows memory |

## 6. Suggested order

Cheapest-first, and each one is independently verifiable:

1. **P1.3 memoization** (S) — a port bug, measurable on `benches/hot.rs`.
2. **P7 recovery + secret hardening** (S) — robustness we can test by
   truncating a state file in a sandbox.
3. **P3 metadata tables** (S–M) — replaces prose; feeds P2.
4. **P4 retry/backoff** (S–M) — testable against a stub that returns 429.
5. **P2 `ask doctor`** (M) — the stub-server harness we already built for the
   LLM tests is most of the offline/catalog tiers.
6. **P5 budget + compaction** (M) — needed before `ask:` grows turns.
7. **P1 positions + typo band** (M) — the only proposal that touches the wire
   and the frontend, so it lands after the batch A wave.
8. **P6 command risk** (M) — pairs with the `do:` port, which is the last
   batch anyway; the launcher-side confirm wiring can land earlier and alone.

Nothing here conflicts with the batch A ports in flight: every proposal lands
in `provider/llm/`, `support/`, `state/`, `oxyd/`, or the CLI — none in
`provider/native/`.
