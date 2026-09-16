# LLM integration — the `ask:` chat surface

Design for a multi-turn conversation with a language model inside the
launcher. The box you already type in becomes the chat input; the card
below it becomes the transcript. One keyword opens it, Enter sends, the
answer streams back word by word.

This document is the full plan: architecture, wire protocol, provider
registry, session model, performance budgets, failure handling, and the
build order. It is written to be implemented from, and to be checked
against later.

---

## 1. What exists today, and what this adds

Two halves of this feature already shipped, on opposite sides of the gap:

**Ctrl+Enter "Ask a model"** — single-shot Q&A. `oxy.json`'s
`askProviders` list names CLI commands (`claude`, `codex`, `gemini`,
`ollama`, `aichat`, `mods`); the first whose `when` probe passes runs the
question and streams stdout line-by-line into `ResultAnswer.qml`, a card
with a blinking caret. `askProvider` pins one. Stateless: every Ctrl+Enter
is a new question with no memory of the last.

**`do:` agent chat** — multi-turn, streaming, Enter-sends-and-clears. A
Python daemon owns the conversation behind a unix socket; the launcher
pushes `{epoch, query}` in and `{epoch, rows}` come back out. Turns
collapse as they finish; refusals render red; the session evaporates after
idle. But it is *agentic* — claude/codex/gemini CLIs with tool
permissions, seconds of startup, built to act on the machine, not to
converse.

**This feature is the middle:** a multi-turn *conversation* — ask, follow
up, ask again — over whatever model the machine can already reach:
subscription CLIs the user has logged into, API-keyed endpoints, or a
local model. No tools, no permissions, no daemon.

| | Ctrl+Enter ask (today) | `do:` agent (today) | `ask:` chat (this) |
|---|---|---|---|
| turns | single | multi | multi |
| model does | answers | acts on the machine | answers |
| transport | one CLI call | daemon + socket | one process per turn |
| startup | fast | seconds | fast |
| backends | CLI list | claude/codex/gemini | registry: subs, keys, local |

Ctrl+Enter and `ask:` are the **same session**. A question fired with
⌃↵ from `git:` is a turn in the same conversation a later `ask:` continues.

---

## 2. Design principles

Every decision below falls out of four constraints:

1. **Zero per-keystroke cost.** Typing is free. The only process that may
   ever run is the one Enter explicitly starts.
2. **Bounded redraws.** Token rate is the model's problem; the UI updates
   at most ~8 times a second no matter how fast the stream arrives.
3. **Nothing runs while idle.** No daemon, no background socket, no
   watcher. State lives in a file; processes live exactly as long as one
   HTTP request takes.
4. **Never silently fail.** Every failure — no provider, dead endpoint,
   expired auth, mid-stream kill — lands as text in the card, naming the
   fix, not as an empty card or a spinner that never stops.

---

## 3. Architecture: the stateless daemon that isn't one

Chat completion APIs are stateless. A "conversation" is a `messages[]`
array resent in full with every request — so the daemon the conversation
seems to need is a file. The entire backend is:

```
bin/oxy-ask   — one Python process, alive only for the duration of a turn
```

```
Enter in ask: scope (or Ctrl+Enter anywhere)
  │
  │   OXY_ASK_PROMPT=<text> oxy-ask ask
  │     │
  │     ├─ read ~/.local/state/omarchy/oxy-ask.json
  │     │    {provider, model, handle, messages[], turns[], touched}
  │     ├─ resolve provider (session's choice, else autodetect)
  │     ├─ transport.send():
  │     │     api kind → POST {endpoint}/chat/completions, SSE
  │     │     cli kind → spawn CLI with its resume flag
  │     ├─ stream tokens → framed stdout ──────────────┐
  │     ├─ append turn to messages[]/turns[], save      │
  │     └─ exit                                        │
  ▼                                                    │
askProcess (QML Process)                               │
  stdout: SplitParser ──► line buffer ──► 120ms flush ─┘
  ──► answerDone + answerTail  (two string properties)
  ▼
ResultAnswer.qml
  collapsed turns (ListModel, append-only)
  live question + streaming answer + caret
  header: provider · model · endpoint host
```

Why not a daemon (the `do:` pattern): a daemon earns its keep when the
server holds state a client can't — the agent's tool session. Here the
"state" is a JSON array we already own, and the connection is per-request
anyway. A file plus a per-turn process gives the same continuity with zero
idle memory, zero socket lifecycle, and a failure surface of exactly one
process at a time.

Why `oxy-ask` is Python and not curl+jq: SSE parsing, two wire protocols
(OpenAI + Anthropic shapes), four CLI continuation dialects, session
management, and graceful SIGTERM handling are all ~real logic. One
`python3` stdlib script keeps it dependency-free on a stock Omarchy
install.

---

## 4. The streaming wire protocol

The constraint that shapes this section: `SplitParser` (the only
incremental stdout reader Quickshell gives us) emits **on newline only**.
A token stream like `"Hel"`, `"lo"`, `" wor"`, `"ld"` produces nothing
until the first paragraph break — visible as the card sitting frozen for a
second or two, which reads exactly like a stalled request.

So `oxy-ask` frames its output:

| Output | Frame | Launcher behavior |
|---|---|---|
| complete line | `text\n` | append to `answerDone` |
| in-progress tail | `text\x00\n` | **replace** `answerTail` |

The NUL byte is the "this is a rewrite, not a new line" marker. The
displayed text is `answerDone + answerTail`; on each NUL-framed flush the
tail is swapped, giving a word-level typing effect. `oxy-ask` emits tail
frames at most every ~120ms and always on a real newline, so the final
flush is a complete-line frame and `answerTail` drains to empty.

```
stream:  "The " → "The answer" → "The answer is\n" → "The answer is\n42"
wire:    The \x00\n   The answer\x00\n   The answer is\n   42\x00\n
card:    "The "       "The answer"       "The answer is" + "42"
```

**Coalescing on the QML side too.** Even at 8 tail-frames/sec, appending
to a bound string property costs a text re-layout per flush. Incoming
lines accumulate in a plain JS array; a 120ms `Timer` joins and appends.
Two string properties change per flush cycle, one `Text` re-lays out —
that is the entire per-frame cost, decoupled from token rate:

```
model at 10 tok/s  → ~8 UI updates/s
model at 200 tok/s → ~8 UI updates/s   (same)
```

**Markdown:** while streaming, the body is `Text.PlainText` — raw `**`
visible mid-type, which is honest "it's typing" and matches how agentic
harnesses render their streams. ~400ms after the stream ends, the
component swaps `textFormat` to `Text.MarkdownText` — one parse, one
layout, formatted result. `"markdown": false` in settings keeps it plain
forever. Live re-parse-per-flush was rejected: Qt's markdown parser runs
over the *whole* document each time, making the cost O(n²) in answer
length for flicker nobody asked for.

**Why a sentinel and not a second channel:** the alternatives are a
socket (a daemon, rejected) or emitting each token as a complete line
(breaks words, breaks markdown). The sentinel keeps one stream, one
parser, one code path — and a provider that emits no newlines still
streams visibly.

---

## 5. The provider registry

`oxy-ask` owns every provider's quirks behind one interface. The launcher
asks for "a turn" and "a probe"; it never knows which backend answered.

### 5.1 The matrix

| id | transport | multi-turn mechanism | auth source | stream shape |
|---|---|---|---|---|
| `claude` | `claude -p --output-format stream-json --verbose` | `--resume <session_id>`; id read from the result event | claude CLI login (subscription) or `ANTHROPIC_API_KEY` | NDJSON deltas |
| `codex` | `codex exec --json` | `codex exec resume --last` or `<id>`; `--all` not used (cwd-scoped is correct for a launcher) | codex CLI auth (ChatGPT sub) or `OPENAI_API_KEY` | JSONL events → `agent_message` text |
| `gemini` | `gemini -p` (prompt via stdin) | `gemini --resume` (latest) or `--resume <index\|uuid>` + stdin prompt | gemini CLI OAuth or `GEMINI_API_KEY` | plain stdout stream |
| `opencode` | `opencode run --format json` | `--continue` / `--session <id>` | `opencode auth` — its own provider registry | JSON events |
| `zai` | `POST api.z.ai/api/coding/paas/v4/chat/completions` | `messages[]` replay | `ZAI_API_KEY` or `ask.providers.zai.key` | SSE |
| `openai` | `POST api.openai.com/v1/chat/completions` | `messages[]` replay | `OPENAI_API_KEY` | SSE |
| `anthropic-api` | `POST api.anthropic.com/v1/messages` | `messages[]` replay | `ANTHROPIC_API_KEY` | SSE (`content_block_delta`) |
| `ollama` | `POST localhost:11434/v1/chat/completions` | `messages[]` replay | none — local | SSE |
| `lmstudio` | `POST localhost:1234/v1/chat/completions` | `messages[]` replay | none — local | SSE |
| `custom` | `POST ask.endpoint` | `messages[]` replay | `ask.key` (`env:NAME` or literal) | SSE |
| `agy` | probe-only, experimental | TBD — no documented non-interactive prompt interface | antigravity CLI auth | TBD |

### 5.2 Two transport kinds

**`api` kind — messages[] replay.** The session file's `messages[]` is
the context: `[system, user, assistant, user, ...]`, POSTed in full each
turn. Works identically for z.ai, OpenAI, OpenRouter, Groq, ollama, LM
Studio, llama.cpp, and any custom endpoint. Provider differences are
three fields: URL, auth header, and which JSON path holds the streamed
delta (`choices[0].delta.content` vs Anthropic's
`content_block_delta.delta.text`).

**`cli` kind — native session handle.** The CLI's own continuation
mechanism carries context; our session file stores a `handle`
(claude's `session_id`, `"latest"` for gemini/codex where no id is
exposed, opencode's session id). `turns[]` is still kept — it is what
the card renders — but it is a display copy, not the context.

### 5.3 Per-provider notes

**claude.** `claude -p --output-format stream-json --verbose` emits
newline-delimited JSON events; text deltas arrive as
`stream_event`/`assistant` payloads and the final `result` event carries
`session_id` — captured into `handle` for `--resume` on the next turn.
`--model <id>` accepts aliases (`sonnet`, `opus`, `haiku`) and full IDs.
Models list: hardcoded defaults (`sonnet`, `opus`, `haiku`) plus free-form
— the CLI validates.

**codex.** `codex exec --json "<prompt>"` emits JSONL state-change events;
the assistant text lives in `agent_message` items. Continuation is
`codex exec resume --last "<prompt>"` (cwd-scoped — correct here, and the
documented behavior). `resume <id>` is used when an id is captured.
`--model` takes the model name directly.

**gemini.** `gemini -p` with the prompt piped on stdin (never argv —
same discipline as `oxy-agent`'s stdin feed). Continuation is
`gemini --resume` (latest) or `--resume <index|uuid>`, with the prompt
piped in — supported in non-interactive mode. Output is plain streamed
stdout, the simplest case in the registry.

**opencode.** `opencode run --format json --model provider/model`.
`--continue` resumes the last session; `--session <id>` a specific one.
It is also the universal adapter: an authed opencode reaches anthropic,
openai, google, z.ai and more through its own registry, so a machine with
only opencode still gets every cloud provider. `opencode models` lists
what its registry offers — the live model list for `/models`.

**z.ai GLM.** OpenAI-compatible at
`https://api.z.ai/api/coding/paas/v4/chat/completions` — the *coding*
endpoint specifically (the general `/paas/v4` endpoint does not draw from
Coding Plan quota). `Authorization: Bearer <key>`; `glm-5.x` models;
`thinking`/`reasoning_effort` fields are passed through when configured.

**Local (ollama / lmstudio / llama.cpp).** `POST /v1/chat/completions`
on the probed port. Probe = `GET /api/tags` (ollama) or `/v1/models` with
a 200ms connect timeout; the model list is live — `/models` shows what is
actually pulled, and a missing model row tells you `ollama pull <name>`.

**agy (Antigravity).** Listed so `probe` can report it; marked
experimental. No documented non-interactive prompt interface exists yet —
when one lands it joins the `cli` transport with a two-line spec change.

**deliberately absent: `devin`, fusion, agent harnesses.** Those create
*work sessions*, not conversations — they belong to `do:`'s world. The
registry's `cli` spec could carry one tomorrow without a Launcher change,
but shipping it as a chat provider would set the wrong expectation about
what Enter does.

### 5.4 Model lists

`/models` resolves per provider:

| provider | source |
|---|---|
| `opencode` | `opencode models` (live) |
| `ollama`, `lmstudio`, `custom`, `openai` | `GET {endpoint}/models` or `/api/tags` (live) |
| `claude`, `codex`, `gemini`, `zai`, `anthropic-api` | shipped defaults + free-form `/model <anything>` |
| any | `ask.models.<provider>` in `oxy.json` overrides the shipped list |

Free-form is always allowed — a model the CLI accepts is valid even if
the shipped list predates it.

---

## 6. Session file

`~/.local/state/omarchy/oxy-ask.json` — one JSON document, owned and
rewritten by `oxy-ask` alone:

```json
{
  "v": 1,
  "provider": "claude",
  "model": "sonnet",
  "endpoint": null,
  "handle": "a1b2c3d4-…",          // cli-kind session id; null for api kind
  "messages": [                   // api-kind context; cli kind keeps [] — its
    {"role": "system", "content": "…"},   // handle carries the context
    {"role": "user", "content": "…"},
    {"role": "assistant", "content": "…"}
  ],
  "turns": [                      // display copy — what the card renders
    {"q": "…", "a": "…", "ms": 4200, "provider": "claude"}
  ],
  "created": 1758…,
  "touched": 1758…
}
```

Rules `oxy-ask` enforces:

- **TTL:** `touched` older than 6h → the session is gone. Next ask starts
  fresh. Matches the `do:` philosophy — nothing yesterday.
- **Caps:** `messages[]` ≤ 24 entries and ≤ 32KB total — oldest
  user/assistant *pairs* drop first, `system` survives. Bounds tokens,
  memory, and cost with one rule.
- **`turns[]`** caps at 20 — display history, not context.
- **Corrupt file** → treated as absent, renamed to `.broken` for
  forensics, never crashes a send.
- **Provider switch** mid-session → `turns[]` is kept (the card's
  history is still honest) but `handle`/`messages[]` reset — the new
  provider never inherits context it cannot read.
- **SIGTERM mid-stream** → the partial answer is committed as the turn's
  answer with a `stopped` flag — an interrupted answer is still
  something said, and the card shows it.

---

## 7. Configuration

`~/.config/omarchy/oxy.json`, merged over defaults by `Settings.merge`:

```json
{
  "ask": {
    "provider": "",              // "" = autodetect; else a registry id
    "endpoint": null,            // custom OpenAI-compat URL — implies provider "custom"
    "model": null,               // override per-provider default
    "key": "env:OPENAI_API_KEY", // "env:NAME" (recommended) or a literal
    "system": "Answer briefly; this renders in a launcher card, not a terminal.",
    "maxTokens": 800,
    "temperature": 0.4,
    "markdown": true,            // post-stream markdown render
    "ttlHours": 6,
    "priority": [],              // autodetect order override, e.g. ["ollama","claude"]
    "providers": {               // per-provider overrides
      "zai":   { "key": "env:ZAI_API_KEY", "endpoint": null },
      "claude":{ "model": "opus" }
    },
    "models": {                  // model-list overrides
      "claude": ["sonnet", "opus", "haiku"],
      "zai":    ["glm-5.2", "glm-4.6"]
    }
  }
}
```

The existing `askProviders` CLI list stays untouched — it becomes the
**fallback tier** when no registry provider is live (single-shot, as
today). `askProviders` entries are *commands*; the registry is
*transports* — they coexist because they answer different needs.

### 7.1 Autodetect order

`oxy-ask` resolves a provider when the session has none, in this order
(`ask.priority` reorders; first `live` wins):

1. `ask.provider` / `ask.endpoint` explicitly configured
2. authed subscription CLIs: `claude` → `codex` → `gemini` → `opencode`
3. key'd APIs: `zai` → `openai` → `anthropic-api`
4. local: `ollama` → `lmstudio`
5. none → error card (see §11)

Subscription CLIs before key'd APIs before local: the user asked for
"activated subscriptions first, local fallback" — and a logged-in CLI is
the strongest signal of intent there is.

---

## 8. The `ask:` surface

### 8.1 Scope registration

`"ask"` and `"chat"` join the hardcoded `knownKeywords` list in
`Launcher.qml` — no extension JSON, no provider machinery, no `search`
command. `ask:` parses to `query.scope === "ask"`; `chat:` aliases to the
same path. `Query.parse` already treats `word:` as a scope only when the
word is registered, so this is the entire cost of the feature at
keystroke time: two strings in a list.

### 8.2 Enter semantics

The Enter handler (the `Qt.Key_Return` branch of `Keys.onPressed`)
gains one check before `activateSelected()`:

```
if (query.scope is ask|chat && query.text !== ""):
    if streaming → stop (partial kept)     — Enter means "that's enough"
    else         → ask(query.text); setInput(scope + ":")  — box resets, stays in scope
```

Ctrl+Enter stays exactly as today — `root.ask(input.text.trim())` — but
now writes into the same session rather than a stateless call. Enter on
an empty `ask:` does nothing. Enter while streaming is *stop*, never
queue — a queued send you can't see is a surprise; stop is obvious.

### 8.3 Slash commands

Input beginning `/` inside the ask scope is a **command, not a turn** —
`oxy-ask` interprets it and prints card text; the LLM never sees it:

| input | effect |
|---|---|
| `/providers` | list: `● claude — sonnet (live)` `○ codex — run: codex auth` `· gemini — not installed` |
| `/provider <id>` | switch provider; resets `handle`/`messages[]`, keeps `turns[]` |
| `/models` | live list for the active provider (or shipped defaults) |
| `/model <name>` | switch model — free-form allowed |
| `/new` | wipe the session; card shows "new conversation" |
| `/system <text>` | replace the system prompt for this session |
| `/export` | write the transcript to `~/notes/` or stdout — TBD at build |
| `/help` | the table above, as card text |

`/…` typed when no provider is live still works — commands need no
backend.

### 8.4 The card — `ResultAnswer.qml` extension

```
┌──────────────────────────────────────────────────────┐
│ claude · sonnet · subscription           2 turns  ⌄ │
│ ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ │
│ you: why is my battery draining                       │
│ ◆ check powerprofiles… · 6 more · 4.2s        (collapsed)
│ ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄ │
│ you: and which process is eating cpu                  │
│ ▌Run `top -o %CPU` — the first column sorts by…▌caret │
│                                                       │
│ Enter send · Esc stop · /provider /model /new        │
└──────────────────────────────────────────────────────┘
```

- **Turns** render from `answerTurns`, a `ListModel` — append-only, so a
  finished delegate is never re-instantiated or re-laid-out.
- **Collapsed by default**: your line + the answer's first line + a
  `· N more · 4.2s` summary. Expand-on-click is a v2 nicety, not v1.
- **Live turn** = `answerQuestion` + `answerDone + answerTail` + caret —
  the existing streamed-answer rendering, unchanged.
- **Header**: `provider · model · source` (`subscription` / `api key` /
  `local`) + turn count — answers "which brain is this" at a glance.
- **Footer**: the key hints, already the launcher's pattern.
- **Auto-scroll**: the `Flickable` follows `contentHeight` while
  streaming unless the user has scrolled up (stick-to-bottom rule —
  scroll up = reading, don't steal the view).

### 8.5 Keys

| key | in ask: / answerMode |
|---|---|
| `Enter` | send · while streaming: stop |
| `Ctrl+Enter` | send (same session) from anywhere |
| `Esc` | stop stream → leave answerMode → clear box → close (existing 4-stage unwind, unchanged) |
| `Ctrl+C` | copy `answerDone+answerTail` (or selected text — editor's own copy wins, as everywhere) |
| `Ctrl+K` | action panel (unchanged) |
| typing `/` | commands, not turns |

---

## 9. `bin/oxy-ask` — the verb surface

```
oxy-ask ask            # send $OXY_ASK_PROMPT as a turn; stream framed stdout
oxy-ask cmd "<line>"   # slash commands (/provider, /models, /new, …)
oxy-ask probe          # JSON: per-provider {id,name,kind,status,models,active}
oxy-ask history        # JSONL of turns[] — card replay on reopen
oxy-ask new            # delete session
oxy-ask status         # one line: "claude · sonnet · 2 turns · 14m old"
```

Env (set by the launcher's `settingsPrefix` from `oxy.json` — never argv):

| var | content |
|---|---|
| `OXY_ASK_PROMPT` | the turn's text — argv stays clean of user text |
| `OXY_ASK_PROVIDER` / `OXY_ASK_MODEL` / `OXY_ASK_ENDPOINT` | config passthrough |
| `OXY_ASK_KEY` | resolved key (or `env:NAME` reference resolved by oxy-ask) |
| `OXY_ASK_SYSTEM` / `OXY_ASK_MAXTOK` / `OXY_ASK_TEMP` | generation params |

`OXY_ASK_PROMPT` via env rather than argv: `/proc/<pid>/environ` is
owner-readable; argv is world-readable. The prompt never appears in
`ps`. (Same reason `oxy-agent` feeds prompts on stdin — stdin is
unavailable to a fire-and-forget `Process`, so env is the next-best
channel.)

### 9.1 Internal structure

```
PROVIDERS = {
  "claude":   CliSpec(bin="claude",   args=..., resume=..., stream="ndjson",
                      auth_probe=..., models=[...]),
  "codex":    CliSpec(...),
  "gemini":   CliSpec(...),
  "opencode": CliSpec(...),
  "zai":      ApiSpec(endpoint=..., key_env="ZAI_API_KEY", shape="openai"),
  "openai":   ApiSpec(shape="openai", key_env="OPENAI_API_KEY"),
  "anthropic-api": ApiSpec(endpoint=..., key_env="ANTHROPIC_API_KEY",
                           shape="anthropic"),
  "ollama":   ApiSpec(endpoint="http://localhost:11434/v1/chat/completions",
                      probe="/api/tags", shape="openai"),
  ...
}
```

- `ApiSpec.send(messages)` — urllib POST, SSE line loop, yields deltas.
- `CliSpec.send(prompt, handle)` — builds argv with the provider's resume
  flag, spawns, parses its event format, yields deltas, returns the new
  handle.
- One `stream(deltas)` wrapper writes the framing (complete lines +
  120ms NUL-tail) — *all* providers get identical wire behavior for free.
- A provider entry is ~30 lines. Adding one never touches QML.

### 9.2 Probe semantics

`oxy-ask probe` runs once per `ask:` open (never per keystroke — the
launcher caches it for the session):

- `cli` providers: `command -v` (free) + a cheap auth probe where one
  exists (`claude auth status`-style; otherwise presence-only, and a
  failed send marks it unauthed at use-time).
- `api` providers: key present? → `live`; else `needs-key`.
- `local`: HTTP probe, 200ms connect timeout.
- Probes run in parallel on threads (all I/O-bound, ~200ms total).
- Output feeds `/providers` and the card header; the `active` provider is
  the autodetect winner.

---

## 10. Performance budget — the contract

| resource | budget | mechanism |
|---|---|---|
| keystroke cost | 0 processes | scope in `knownKeywords`; no `search` command |
| per turn | 1 process, 1 conn | `oxy-ask` (~40MB transient RSS, exits at stream end) |
| stream redraws | ≤ 8/sec | 120ms coalesce timer, tail-replace framing |
| row model | 0 rebuilds | answer is a string property, never a row |
| markdown parses | 1 per turn | PlainText while streaming → MarkdownText swap after |
| turn delegates | +1 per turn | append-only `ListModel`; collapsed, fixed height |
| session memory | ≤ 32KB messages, 20 turns | pair-drop cap in `oxy-ask` |
| answer buffer | ≤ 64KB | `answerDone` truncates oldest lines past cap |
| idle cost | 0 | no daemon, no socket, no timers armed |
| probe | ~200ms, once per ask: open | parallel threads, cached |

The one number worth defending: **the row model is never involved**.
`putRaw`/`rebuild`/`measureChips` — the machinery every other answer
pays — is bypassed entirely. The answer view binds to two string
properties and one ListModel; a 60-second stream costs ~480 text layouts
and nothing else.

---

## 11. Failure modes — every one lands in the card

| failure | card shows |
|---|---|
| no provider live | "No model configured. Live options: `ollama` (not running), `claude` (run `claude auth login`), or set `ask.endpoint`/`OPENAI_API_KEY` in oxy.json." |
| provider present, unauthed | "claude is installed but not signed in — run `claude auth login`, then `/provider claude`." |
| endpoint unreachable | "could not reach localhost:11434 — start ollama or `/provider <other>`" |
| API error (429/500/auth) | status + first 200 chars of body — the API's own words |
| mid-stream disconnect | partial answer kept + `⚠ stopped mid-answer` |
| SIGTERM (Esc/Enter) | partial committed as the turn, flagged `stopped` |
| CLI flag drift (old version, unknown resume flag) | degrade to single-turn + warn once in card |
| corrupt session file | fresh session + file moved to `.broken` |
| `oxy-ask` missing | "oxy-ask not found — reinstall oxy" |
| session provider gone (uninstalled since) | re-autodetect, note the switch |

Rule: **an error is a row of text, never a silence and never a spinner.**

---

## 12. Security & privacy

- **Prompt transport:** `OXY_ASK_PROMPT` env — owner-only in `/proc`,
  never argv. CLI providers get the prompt on stdin where they accept it
  (gemini; claude `-p` positional is the documented form — noted if
  argv is unavoidable).
- **Keys:** `env:NAME` indirection is the documented form; literals work
  but the docs say why not. Keys never appear in logs (`log()` clips,
  and `oxy-ask` never prints headers).
- **Local-first privacy note:** `ollama`/`lmstudio` means the transcript
  never leaves the machine — stated plainly in README so the choice is
  informed, not assumed.
- **Session file:** `0600`, lives under `$XDG_STATE_HOME` — it's a
  transcript; treat it like shell history.
- **No new network surface:** `oxy-ask` makes outbound requests only; it
  binds nothing.

---

## 13. Testing plan

The harnesses below already exist: `tests/behavior.test.sh` runs real
scripts against sandboxed fixtures (fake `nmcli`, temp git repos, fakebin
stubs) and `tests/cases.py` runs the shipped `.cases.json` files under a
throwaway `HOME` — `ask.cases.json` slots into both without new machinery.

**`oxy-ask` (the logic core — where the bugs will live):**

- **Stub SSE server** (python `http.server`, emits canned
  `data:` frames incl. multi-line deltas and `[DONE]`) → assert framed
  output shape, tail-replace cadence, full text, messages[] append.
- **Anthropic shape** stub → `content_block_delta` extraction.
- **Fake CLI bins** — a `claude` stub emitting canned `stream-json`
  NDJSON (deltas + result with `session_id`) → assert `--resume` used on
  turn 2 with the captured id. Same pattern for codex JSONL and a
  plain-stream gemini stub.
- **Session:** TTL expiry, 24-msg cap drops oldest pair + keeps system,
  corrupt file recovery, provider-switch reset semantics.
- **Slash commands:** `/providers`/`/model`/`/new` output and state
  effects without a backend.
- **SIGTERM:** partial committed + `stopped` flag.
- **Framing:** NUL-tail lines decode correctly; no stray `\x00` in
  `answerDone`.

**Launcher side:**

- `tests/logic.test.mjs`: `Query.parse("ask: hello")` → scope `"ask"`;
  `chat:` aliases; `ask:` unregistered when keyword absent (guards the
  registration).
- Manual matrix: real ollama, a key'd endpoint, one CLI — send, follow-up,
  `/provider`, `/model`, `/new`, Esc-mid-stream, close+reopen (history
  replay).

---

## 14. Implementation phases

Each phase is independently useful and independently verified.

**Phase 1 — `bin/oxy-ask` core.** ApiSpec transport (openai + anthropic
shapes), session file, framing writer, autodetect, `probe`, slash
commands. Verified against the stub SSE server + live ollama — no QML
touched yet.

**Phase 2 — CLI transports.** `CliSpec` + claude/codex/gemini/opencode
entries with real flags verified against installed binaries (stubs where
not installed). Graceful degrade on flag drift.

**Phase 3 — Launcher wiring.** `ask`/`chat` in `knownKeywords`; Enter
intercept; `OXY_ASK_PROMPT` env in `shellArgv`; `answerTurns` ListModel;
120ms flush + NUL-tail; Ctrl+C → answer; `/`-input routing to `oxy-ask
cmd`.

**Phase 4 — Card + polish.** `ResultAnswer` turns view + header/footer +
stick-to-bottom; `probe` on `ask:` open; markdown post-swap; `unit.toml`
touches; README section; tests.

**Phase 5 — stretch (not v1):** `/export` transcript, expandable turns,
`Ctrl+M` model cycle, `agy` once a prompt interface exists, agent-harness
providers as `cli` specs if ever wanted.

---

## 15. File inventory

| file | Δ | what |
|---|---|---|
| `bin/oxy-ask` | +~450 | registry, transports, session, probe, slash, framing |
| `plugin/Launcher.qml` | +~130 | scope intercept, `answerTurns`, flush/NUL-tail, env prompt, `/` routing, Ctrl+C→answer |
| `plugin/ResultAnswer.qml` | +~110 | turns ListView, header, footer hints, markdown swap, stick-to-bottom |
| `plugin/Settings.js` | +~25 | `ask` defaults, `ask.priority`, `models`/`providers` maps |
| `tests/logic.test.mjs` | +~30 | scope parse, alias, framing decode |
| `tests/fixtures/` | +~120 | stub SSE server + fake CLI bins |
| `unit.toml` | +1 | `~/.local/state/omarchy/oxy-ask.json` in `touches` |
| `README.md` | +~50 | `ask:` section, provider table, config keys |
| `docs/LLM-INTEGRATION.md` | this file | the plan |

---

## 16. Explicit non-goals

- **No tools.** The model never gets function-calling, file access, or a
  shell — that is `do:`'s job, and mixing them is how a chat becomes a
  footgun.
- **No daemon.** Revisited and rejected in §3; if a future need
  (background streaming while closed) appears, the socket pattern in
  `oxy-agent` is the template, not a redesign.
- **No per-keystroke work, ever.** Not a hint row, not a `when` probe.
  `ask:` costs exactly two strings in `knownKeywords` until Enter.
- **No auth implementation.** Each provider's own login is the
  activation; `probe` reports status, never credentials.
- **No images/tools/multimodal in v1** — the wire supports it later;
  the card doesn't need it now.

---

## 17. Known risks & mitigations

| risk | mitigation |
|---|---|
| CLI flag drift (resume flags differ across versions) | isolate per-provider in `CliSpec`; on send failure naming a flag → degrade to single-turn + warn; never crash |
| `agy` interface unknown | probe-only entry; documented as experimental; two-line spec when it lands |
| markdown flicker mid-stream | PlainText-during-stream chosen; `markdown: false` escape hatch |
| very long answers | 64KB answer cap; `messages[]` 32KB cap; turn collapse keeps the card short |
| user expects chat to act ("delete X") | footer + docs point at `do:` — honest boundary |
| session file as plaintext transcript | `0600`, state dir, TTL 6h, `/new` wipes — same posture as shell history |

---

*Status: planned, not yet implemented. Phase order and the per-provider
spec details in §5.3 are the contract the implementation is checked
against.*
