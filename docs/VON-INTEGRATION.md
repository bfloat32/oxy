# Von in Oxy — a System One classifier, analysed and designed

Everything in §0–§1 was verified against the model's own repository, HuggingFace
files and model card on 2026-09-24. Everything in §2–§8 is grounded in this
tree: every integration point named below exists, and the numbers come from the
benchmark and the suite.

**No code changes here.** This is the analysis the decision needs.

---

## 0. What Von actually is

| fact | value | source |
|---|---|---|
| backbone | **ModernBERT-large** (395 M params) + an **Option-Marker scoring head** | model card |
| context | 8 192 tokens; the card says premise reading holds "out to ~2048 tokens" | model card |
| licence | **Apache-2.0** (academic, personal, commercial) | repo, HF |
| language | **English only** | limitations |
| weights | `model.safetensors` **1.58 GB** + `option_marker.pt` **1.58 GB** (fp32) | HF file list |
| tokenizer | `tokenizer.json` **3.58 MB** — standard HF format | HF file list |
| calibration | `calibration.json` + `marker_calibration.json` (input-conditioned temperature map, zero-shot `noul` prior, `independent_options: true`) | model card |
| **ONNX / WASM** | **none shipped.** The TypeScript SDK is an *HTTP client* for `von serve` (`baseURL: http://localhost:8000`) | `js/README.md` |
| server | `von serve` → `POST /v1/systemone` (the TypeSafe Jev wire shape) | repo README |

**The API is three primitives plus a fan-out** — and they are the reason this
model fits a launcher rather than an LLM:

| primitive | question it answers | output |
|---|---|---|
| `choice` / `decide` | pick one of N described options | option + calibrated probability per option |
| `noul` / `judge` | is this true of the state? | P(y=1) ∈ [0,1] |
| `score` / `rate` | how bad / good, on an ordered scale | expected value on [0, K−1] |
| `system_one` | all of the above at once | one forward pass, every answer |

Patterns built on top: `confidence_gate` (automate above a threshold, escalate
the tail), `route` (dispatch by choice), `composite_score` (weighted risk),
`two_stage_choice` (>25 options in two passes).

**1.2's headline is the one property that matters most to us.** In 1.1 the
answer depended on the order the options were listed: **49.5 %** of hard-tier
answers changed when the same options were shuffled. 1.2 makes each option's
logit a function of *(premise, that option)* alone — per-option attention, and
position ids that restart after the premise — so permuting options permutes the
logits and nothing else. Measured: **0 flips in 111 hard items × 4 orderings.**
That is a guarantee, not a tendency, and it is exactly what a launcher needs
when it feeds a candidate list of keywords to a classifier.

**And it is honest about where it fails.** Their own numbers, JevBench public
splits: easy **100 %** (n=48), standard **63.9 %** (n=72), hard **36.9 %**
(n=111) — with the card saying it is "near-chance on long multi-hop legal/policy
reasoning; do not use it unsupervised". Calibration ECE 0.045–0.109, but
**fitted on the same 231 public items** (in-sample; split-half 67.6). Zero-shot
`noul` with no criteria: **85.1 %** on a held-out 175-item dev set. English
only. And the guidance that matters most for prompt design: *give explicit,
descriptive criteria rather than bare labels* — "instead of `["beginner",
"advanced"]`, provide concrete operational definitions".

Their latency claims — "sub-15ms", "sub-25ms" — are **GPU** numbers (the Doom
demo says "on a GPU" in its own caption). A 395 M encoder on CPU for a
launcher-sized premise is tens to low hundreds of milliseconds. That single
fact shapes everything below.

---

## 1. What that means for *this* launcher

Measured on this machine, release build, `tests/bench_oxy.py --queries 20`:
**cold start 92 ms, keystroke p50 0.1 ms (p95 0.2 ms), daemon RSS 11.1 MB.**

| constraint | the number | consequence |
|---|---|---|
| the keystroke path | 0.1 ms p50 | a 395 M model is **~300–1000× too slow** for it. The classifier is never in that path. It runs at **open**, on **idle**, and in the **background**, and its answers are cached like any other provider answer |
| memory | 11.1 MB daemon | 1.6 GB fp32 is **~160×** the whole daemon. Either it lives in its own process (sidecar) or it is **distilled smaller** — a 395 M fp32 model in-process would end the resource story this project is proud of |
| parity | the script build is the reference | every use must be **additive**: a suggestion row, a `fill`, a log line, a setting. Never a change to what a keyword means, never an auto-execution, never a ranking change |
| determinism | the order-invariance guarantee | covers option *shuffling*, not version drift — pin the model revision, tokenizer and calibration and hash them, the way we pin the scripts' contracts |
| English only | card limitation | fine for us now; note it, and never let a classifier be the only path to a function |

---

## 2. Three ways to run it, with real costs

**A. Sidecar — `von serve`, HTTP on loopback.**
The launcher talks to `127.0.0.1:8000/v1/systemone` exactly the way it already
talks to `ask.endpoint`: our HTTP client, our retry policy, our doctor. Zero ML
engineering, and the officially supported path (the TS SDK is this, and only
this). Cost: Python + torch + the 1.6 GB weights on disk and ~2–4 GB RSS when
warm — a second service to install and supervise. Latency: the model's own
pass plus HTTP, realistically 20–80 ms.
*This is phase 1.*

**B. ONNX + Rust — export it ourselves.**
`torch.onnx.export` on their weights, the `tokenizers` crate for
`tokenizer.json`, and a Rust runtime (`ort`). The hard part is not the export:
it is reproducing **the Option-Marker packing exactly** — one sequence, a
`[MASK]` marker per option, per-option attention isolation, position ids that
restart after the premise, and the sliding-window mask computed from those
position ids. Get that subtly wrong and the model still runs, still answers,
and has quietly lost the order-invariance guarantee that is the reason to
choose 1.2. Add int8 quantization to get ~400 MB and re-measure accuracy.
Expect 80–250 ms per pass on CPU, 15–30 ms on a GPU. *A real project, worth it
only if the sidecar proves the value.*

**C. Distil into a launcher-sized model — the end state.**
Von is a **teacher**: generate labels with it over *our* queries (the event log
plus synthetic ones), train a small encoder (6–22 M: ModernBERT-small,
DeBERTa-v3-xsmall) with the same option-marker head, ship *that*. CPU
**3–15 ms**, ~50–150 MB, in-process if we want it, no Python, no service. Their
repo ships `training/` and `benchmarks/fit_calibration.py`, and the licence
permits it. This is the only path that gives a **sub-15 ms classifier that is
actually sub-15 ms** — and it is ours, on our domain.

**Recommendation: A → shadow mode + data → C.** B only if something in A proves
insufficient.

---

## 3. The use-case catalogue

Twenty-four uses, grouped by *where they run*. Each names the primitive, the
state it reads, the integration point that exists today, its budget, and what
it must never do.

### Tier 1 — at open, cached, never per keystroke

| # | use | primitive | state → question | integration point |
|---|---|---|---|---|
| 1 | **Unscoped intent routing**: "my bluetooth mouse isn't tracking" → `bt:` | `choice` over the 45 keywords, each described by its title + aliases | the box's text → "which keyword's tool would fix this?" | a **suggestion row** in `engine/inline.rs`'s empty-box rows, `fill: "bt:"`; the box is pre-filled, Enter still decides |
| 2 | **Sigil disambiguation**: `/etc` is a path, not a command | `noul` — "does this look like a filesystem path?" | the text after `/` | `model/query.rs::looks_like_path` — the classifier becomes an *additional* vote beside the existing `contains('/'|'.'|'~')` heuristic, suggestion-only |
| 3 | **The calc gate**: is this arithmetic at all? | `noul` — "is this a mathematical expression, ignoring alphabetic units?" | the text | `native/calc/mod.rs`'s regex gate (a documented trap: qalc accepts invalid units unless gated). Suggestion: "did you mean a calculation?" — never a route change |
| 4 | **Semantic app search**: "spreadsheet" → LibreOffice Calc | `choice` over installed app entries | the text → the app list | `native/desktop/apps.rs` — a **second band** below `weak` in the suggestion row, only when the fuzzy scorer returns nothing |
| 5 | **Quicklink routing**: "the thing I use for tickets" | `choice` over the user's quicklinks | the text | `native/desktop/quicklinks.rs` — same shape as #1 |
| 6 | **Agent vs launcher**: "why is my wifi slow" is a *question*, "wifi" is a *query* | `noul` — "is this a request for an explanation rather than a search?" | the text | `engine/ask.rs` — a hint beside the `⌃↵ Ask` chip ("this looks like a question"), never an automatic ask |
| 7 | **The `?` router**: "how do I connect to wifi" → `wifi:` | `choice` over keywords | the text | the `?` help rows — a "Start here" row above the keyword list |
| 8 | **Settings discovery**: "my keyboard layout is wrong" → `omarchy:` | `choice` over extensions with settings + the omarchy tree | the text | same as #7 |

**Why open-time:** one forward pass per summon, cached by query text, and the
result is stale the moment the box changes — which is fine, because it is a
*suggestion* and the user's typing still decides.

### Tier 2 — per query, on idle, needs the small model (C)

| # | use | primitive | state → question | integration point |
|---|---|---|---|---|
| 9 | **"Did you mean `git:`?"** for a typo'd keyword (`gti:`, `rep:`) | `choice` over the ~45 keywords | the typed word → the closest keyword | `engine/inline.rs` (the rows `answer_recents`/`answer_paste` already add) — one row, `fill: "git:"` |
| 10 | **Mixed-query scope**: `report format:pdf` typed with no scope | `choice` over candidate providers | the text + the filters present | `model/query.rs`'s scope resolution — suggestion only, because a scope change is a route change |
| 11 | **Unknown keyword**: `k8s:` typed when no extension owns it | `noul` — "does this look like a provider someone meant?" | the keyword + the registry | the same row as #9, with "no such keyword" as the fallback text |
| 12 | **Selection hint**: three rows, the user's intent between them | `choice` over the top 3 row titles | the text + the rows | the *hint* line, never the selection (`rank.rs` stays the only thing that orders rows) |

**Why this tier needs C:** at 0.1 ms p50 per keystroke there is no room for
anything, but at idle — 150 ms after the last keystroke, when the engine is
already publishing — a 3–15 ms model fits without being felt.

### Tier 3 — background and ambient (the best fit of all)

| # | use | primitive | state → question | integration point |
|---|---|---|---|---|
| 13 | **Clipboard triage**: today the paste row offers *URLs only* | `choice` over {url, path, code, prose, secret} | the clipboard text | `oxyd/src/clipboard.rs` + the paste row — offer the right *kind* of row instead of one narrow rule |
| 14 | **Alert scoring** for the bar/notifications: "memory at 98 % with OOM kills" | `score` on [ignore, toast, modal] | the log line | the event log + a `notice` — the launcher's existing vocabulary |
| 15 | **System anomaly triage**: docker/system rows worth surfacing | `score` | `docker:`/`sys:` row text | a "worth a look" row, only above a threshold |
| 16 | **Error triage for the agent**: is this trace blocking? | `noul` + `score` | the `do:` agent's stderr tail | the agent track (batch G) — decide whether to raise a card |
| 17 | **Multi-turn topic shift** for `ask:` | `noul` — "is this a follow-up to the previous turn?" | the new question + the last turn | `provider/llm/`'s session design (§6 of the LLM doc) — decides whether history is replayed |
| 18 | **Provider routing for `ask:`**: local model, CLI, or keyed endpoint | `choice` | the question + the registry's cost/latency facts | `engine/ask.rs::probe_ask` — a *suggestion* in the chip, never a silent switch |
| 19 | **A second opinion on command risk**: the deterministic classifier says `Confirm` | `noul` — "would this destroy something outside the working directory?" | the command line | `do:`'s policy engine and the confirm flow — the two-stage cascade jcode's crate already models |
| 20 | **Ambient theme/mode suggestion** | `choice` | time, running apps, the theme list | low value; listed for completeness |

### Tier 4 — agent supervision (batch G, where it pays most)

| # | use | primitive |
|---|---|---|
| 21 | **Permission triage**: the safety queue's auto-allow vs ask | `noul` with the action described |
| 22 | **Plan-step verification**: did this step do what it said? | `noul` |
| 23 | **"Is this done?"** before the agent stops | `noul` + `score` |
| 24 | **herdr state reporting**: working / blocked / idle, with the honesty rule that claiming a state you cannot back up is worse than silence | `choice` |

---

## 4. The feedback loop — what makes this *ours* rather than a demo

The launcher already writes the labels. `oxy-log.jsonl` records `open` (the
query), `prov.done` (which provider answered, how fast), `act` (which row was
run) and the close. That is, per summon: **what the user typed, what they were
shown, and what they chose.**

- **Shadow mode first.** Run the classifier on every open, log
  `von.pred` beside the eventual `act`, change nothing. This is a one-line
  setting and it produces the accuracy number for *our* domain, which is the
  only number that matters — their hard-tier 36.9 % is a warning, and our
  domain (short, well-posed, single-step) is precisely where the card says the
  model is strongest.
- **Then enable per use case**, each behind its own threshold, each measured
  against the shadow log.
- **Then fine-tune**, using their `training/` recipe on our labels — and
  **refit calibration** with `benchmarks/fit_calibration.py` on our data,
  because their map was fitted on 231 benchmark items and the card says so.
- **Then distil** (option C) with Von as the teacher.

---

## 5. Risks, and what each one costs

| risk | evidence | mitigation |
|---|---|---|
| accuracy on launcher intents is unknown | their hard tier is 36.9 %; no launcher corpus in training | shadow mode before any visible behaviour; suggestions only; `noul` (85 %) for binary gates, `choice` only above a measured threshold |
| latency | 395 M params; their ms numbers are GPU | never in the keystroke path; one pass per open, cached by query; idle-time tier; distilled model for tier 2 |
| memory | 1.6 GB fp32 vs our 11.1 MB daemon | out-of-process sidecar, or distilled; never a 1.6 GB daemon |
| parity | the script build is the reference | additive only: a row, a `fill`, a log line, a setting. Never an auto-execute, never a reorder, never a keyword's meaning |
| determinism | order-invariance is a guarantee; version drift is not | pin model revision + tokenizer + calibration, hash them, record the hash in every shadow line |
| English only | card | fine now; never the only path |
| calibration drift | in-sample ECE; card says refit | refit on our data before trusting a threshold |
| a sidecar is a service | Python + torch + 1.6 GB | the same lifecycle story as ollama/herdr: install script + a doctor tier + supervision; loopback-only, and the doctor already knows how to say "not running" |
| the "smart" illusion | a classifier right 85 % of the time is a delight as a *suggestion* and a betrayal as an *action* | every tier-1/2 use pre-fills or hints; nothing runs itself |

---

## 6. First slice, concretely

Small enough to land in one pass and honest enough to measure:

1. **`von.endpoint` in `oxy.json`** — the same shape as `ask.endpoint`, the
   same loopback-`http://` rule, the same client, retry and doctor. Nothing new
   to trust.
2. **`oxy von doctor`** — a third tier for the existing doctor: reachable,
   `/v1/systemone` answers, one known-answer probe ("is this a greeting?" with
   a fixture) so a wrong model or a broken calibration is caught at setup.
3. **Shadow mode** (`von.shadow = true`, the default *off*): one forward pass
   per open, in a task, `von.pred` written to the event log beside the `act`
   that follows. Zero user-visible change.
4. **One suggestion row** (`von.suggest = true`, default off): use case #1,
   the unscoped intent router, rendered as a normal row with `fill: "bt:"`.
   It disappears the moment the box is scoped or the confidence is under the
   threshold.
5. **`tests/bench_von.py`** — the sidecar's own numbers (first token, warm
   pass, RSS) beside `bench_oxy.py`'s, so "it feels instant" is a measurement.

Acceptance: with both settings off, `tests/run.sh` is byte-identical in
behaviour (the parity rule); with shadow on, the log carries predictions for
real usage; with suggest on, the row appears only where the confidence clears
the threshold, and the *script* build is untouched.

---

## 7. Option C in detail — how the distillation would actually work

The point of C is not "a smaller Von". It is: **keep Von as the label
machine, and build a student whose shape is chosen for our runtime** — because
the student's architecture is free, only its *answers* have to match.

### 7.1 The shape: split the options from the premise

Von packs the premise and every option into one sequence and scores each
option's `[MASK]` marker in a single pass. That is what makes it fast *for its
size*, but it also means the whole sequence is re-encoded for every query.

Our options are **static**: the 45 keywords' titles and aliases (tier 1), or a
handful of intent labels (tier 2). So the student is a two-tower scorer:

```
   option table (static)                 premise (per query)
   +-------------------+                 +----------------+
   | "bt: Bluetooth..."|                 | "my bluetooth  |
   | "wifi: Networks..."|  encode ONCE   |  mouse isn't   |  encode per query
   | ...45 entries     | --------------->|  tracking"     | --------------->
   +-------------------+   at startup    +----------------+   ~10-30 tokens
            | cached states (45 x d)              | state (d)
            +--------------+----------------------+
                           v
                 cross-attention scorer (2-4 heads, one layer)
                           v
              softmax over options -> calibrated probabilities
```

Three consequences, all good:

- **Warm latency is a premise encode, not a table encode.** ~10-30 tokens
  through a 20-40 M encoder is **~1-3 ms** on CPU, and the option side is
  already in memory. That is the "sub-15 ms" claim made true — in-process, no
  Python, no HTTP.
- **Order-invariance becomes structural.** Each option is scored against the
  premise *alone*; nothing an option does can affect another's score. Von 1.2
  had to engineer that guarantee (per-option attention, position restarts);
  the two-tower student has it for free, and the shuffle diagnostic becomes a
  regression test rather than a hope.
- **The option table can be baked into the artifact.** Encode the 45 keyword
  descriptions once, at training time, and ship the resulting matrix inside
  the model file — the daemon then loads a premise encoder plus a table, and
  startup costs nothing.

### 7.2 Where the labels come from

Two sources, deliberately mixed:

| source | what it is | weight |
|---|---|---|
| **Von, as teacher** | `choice` over the option table, `system_one` for a batch of use cases. The card says its probabilities are *calibrated*, which is exactly what makes them good **soft targets** — a distribution transfers more than an argmax | the bulk of the data |
| **the event log** | `oxy-log.jsonl`: what was typed (`open`), what answered (`prov.done`), what was chosen (`act`). Real queries, real choices — the only ground truth that matters | weighted up; it is the tie-breaker where teacher and user disagree |

Plus **synthetic queries** for coverage, generated with our own `ask:` endpoint
("write 200 ways someone asks to connect to wifi, and 200 that are *not*
that"), seeded from the manifests' own titles and aliases, with the other
keywords' descriptions as hard negatives. A few hundred to a few thousand
examples per use case is enough for a small encoder with soft targets —
**10-30 k total**, which is a laptop-sized fine-tuning job, not a cluster one.

### 7.3 The training recipe

- **Base**: a pretrained small encoder, so the student starts with language and
  needs only the decision head — `bert-small`/`distilbert` (~14-22 M),
  `deberta-v2-xsmall` (~22 M). All three have a pure-Rust path (below).
- **Loss**: `KL(student || teacher)` on the option distribution (the soft
  signal) + `lambda * CE(real label)` from the log (the true signal) + a
  **Brier term**, which is Von's own RLCD idea — cross-entropy alone produces
  the overconfident, badly calibrated probabilities we would then have to
  threshold against.
- **Shuffle augmentation**: permute the options every step. With the two-tower
  shape this is belt-and-braces, but it costs nothing and it is how a
  *future* architecture change would keep the property.
- **Calibration last**: fit a temperature map on a held-out split of *our* data
  with their `benchmarks/fit_calibration.py`, and record the ECE next to the
  accuracy. A threshold is only meaningful on a calibrated score.
- **Their `training/` directory is the reference implementation** for the
  pipeline shape; our script is a fork of it with our data and our head.

### 7.4 The Rust side

- **Runtime: `candle`** — HuggingFace's pure-Rust engine. It already has
  `modernbert`, `bert`, `distilbert` and `debertav2` implementations, and
  loads `safetensors` directly, so the student needs **no ONNX export and no
  C++ runtime**: a new `oxy-core/src/provider/system_one/` module with
  `runtime.rs` (load + forward), `pack.rs` (the option table and the premise
  template), `calib.rs` (the temperature map), and `mod.rs` exposing the same
  three primitives the teacher has (`choice`/`noul`/`score`) so call sites do
  not care which is answering.
- **Tokenizer: the `tokenizers` crate**, reading the same HF `tokenizer.json`
  format — and we can reuse **Von's own tokenizer** for the student, so one
  tokenizer covers both and there is no vocabulary drift to reason about.
- **If candle's CPU speed disappoints**, `ort` (ONNX Runtime) is the fallback:
  faster on CPU, at the cost of a C++ runtime dependency. That is the *only*
  reason to export ONNX at all in this design.
- **Artifact**: `model.safetensors` (f16 ~15-45 MB for 14-22 M), the shared
  `tokenizer.json` (3.58 MB), our `calibration.json`, and a `manifest.json`
  with hashes — one pinned bundle, downloaded by the installer or shipped in
  the release, hash-checked at load, and a mismatch **disables the feature
  rather than crashing the daemon**.
- **Doctor**: `oxy von doctor` gains a tier that verifies the bundle's hash
  and runs one known-answer probe, so a wrong or half-downloaded model is
  caught at setup instead of as a strange suggestion.

### 7.5 What it costs, and what could go wrong

| | |
|---|---|
| compute | label generation: thousands of teacher calls x 20-80 ms = minutes. Training: one GPU-hour-class run per revision, re-runnable from a pinned script and dataset |
| memory | ~30-60 MB in-process (f16 weights + the option table) on top of the daemon's 11.1 MB — the whole point of C over A/B |
| latency | ~1-3 ms warm per query; the option table is encoded once (or shipped precomputed) |
| the honest ceiling | **a student cannot beat its teacher on the teacher's labels.** Von's hard tier is 36.9 %, so the student inherits that weakness — which is why the log's real labels are weighted up, and why the domain choice matters: our use cases are the "short, well-posed" kind the card says the model is strongest at |
| distribution shift | synthetic queries are not real ones. Mitigation: shadow mode on real traffic before anything is enabled, and the real-log share grows as it accumulates |
| overfitting the golden set | keep a held-out split the training script never sees, and report accuracy *and* ECE on it every run |
| licence | teacher Apache-2.0, student ours; cite Von per its card |
| language | English only, inherited — never the only path to a function |

### 7.6 The order it lands in

- **C0 — data.** Sidecar + shadow mode (phase 1). Every open logs
  `von.pred` beside the eventual `act`. This is the label set, and it needs no
  ML work at all.
- **C1 — offline.** Train the student on the log plus synthetic data; measure
  accuracy, ECE and the shuffle diagnostic on a held-out split. Nothing ships;
  if the numbers are bad, C stops here and we have lost a weekend, not a
  quarter.
- **C2 — ship one use case.** The `system_one` module, the doctor, the pinned
  bundle, and *one* suggestion row (tier 1, use case #1) behind its setting.
- **C3 — grow.** Each further use case is enabled only once its own accuracy
  clears its own threshold on the held-out set, and the model is retrained when
  a new use case changes the option table.

---

## 8. What I would decide, and what I would not

**Would:** start with the sidecar and shadow mode, because it costs no ML
engineering, reuses machinery we already trust (the ask client, the doctor, the
retry policy), and produces the one number nobody has: accuracy on *launcher*
intents. Then let the data pick the use cases, then distil.

**Would not:** put a 395 M fp32 model in the daemon; put any classifier in the
keystroke path; let it reorder rows, change a keyword's meaning, or execute
anything on its own; or ship a suggestion whose confidence has never been
measured against this launcher's own log.
