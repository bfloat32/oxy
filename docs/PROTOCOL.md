# The oxyd wire

`oxyd` is the launcher's engine as a daemon. It listens on one local socket —
`$XDG_RUNTIME_DIR/oxyd.sock` (falling back to `~/.local/state/omarchy/oxyd.sock`)
on Unix, the `oxyd` named pipe on Windows — and speaks JSON lines: one command
in, one event out, one `\n` between them.

The frontend is a teletype. It sends what the box says and draws what it is
sent; every ordering decision — debounce, epochs, staleness, merges, what Enter
does — lives in the engine. `plugin/Shell.qml` is the reference client; for
poking at the wire by hand, `oxy send` pipes stdin commands to the daemon and
prints the events that come back.

## Commands in

```jsonc
{"op": "open",  "text": "fire"}        // a summon: registry + clipboard + query
{"op": "query", "text": "fire", "opened": true}
{"op": "close"}                        // hide: stop refreshes, revert previews
{"op": "activate", "key": "ext:file:x", "action": 1, "shift": false, "ctrl": false}
{"op": "act", "key": "ext:wifi:home", "action": {"exec": "nmcli … {psk}…"}}
{"op": "pin",   "key": "ext:apps:firefox"}
{"op": "select","key": "ext:themes:nord"} // run its previewExec, if any
{"op": "commitpreview"}
{"op": "set",   "key": "ext:vol:main", "value": 63}
{"op": "savesettings", "id": "spotify", "values": {"token": "…"}}
{"op": "ask",   "text": "why is the sky blue"}
{"op": "stopask"}
{"op": "clipboard", "url": "https://…"}   // a URL the frontend already knows
{"op": "reload"}
{"op": "log",   "ev": "open", "fields": {"q": "fire"}}
{"op": "ping"}
```

`key` is the row's stable identity (`ext:<provider>:<id>`). An empty `key` on
`activate` means "whatever leads once the list catches up" — the engine holds
it for three seconds rather than dropping it.

`act` sends a whole action object for things the row never declared — a form's
`exec` with `{field}` tokens substituted. Declared actions go by index on
`activate`; `shift` is shorthand for index 1, `ctrl` is copy-the-row.

## Events out

```jsonc
{"op": "hello", "version": "0.7.0", "keywords": ["apps", "calc", …]}
  — the keyword set as it stands at connect time, rebuilt on every reload,
  so a client that joins mid-session is told the live set, not the boot one

{"op": "registry", "extensions": [{"id","title","keyword","aliases","glyph","accent","view"}],
                   "ask": {"available": true, "model": "Claude"}}

{"op": "results", "epoch": 7, "rows": [ … ], "waiting": ["gh"], "stale": ["file"],
                  "scope": "file", "scopeLabel": "Files",
                  "helpMode": false, "recentMode": false,
                  "view": "list", "confirm": ""}

{"op": "type", "text": "docker:", "flow": true, "poll": true}
{"op": "notice", "text": "Pinned"}
{"op": "close"}

{"op": "answerstart", "question": "…", "provider": "Claude"}
{"op": "answer",      "line": "Because Rayleigh scattering…"}
{"op": "answerdone",  "error": ""}

{"op": "log", "ev": "act", "fields": {"id": "ext:apps:firefox"}}
{"op": "pong"}
```

- `results` is the merged list for the current epoch — `waiting` is provider
  ids still owed an answer, `stale` is providers whose visible rows are older
  than the question, `confirm` is the armed `confirm` prompt while one is armed.
- `type` puts text in the box. `flow` means a step deeper — push the current
  query onto the back-trail first. `poll` means the action's effect lands
  later, so the text is typed and then re-asked a few times over ~4s.
- `answer*` is the `ask` stream: the question, then a line at a time, then the
  end. `error` is only set when the command failed before answering.
- `log` events are also appended to `~/.local/state/omarchy/oxy-log.jsonl` by
  the daemon itself — a client can ignore them.

## Rows

A row is the shape `Extensions.toRow` always produced — `key`, `providerId`,
`title`, `subtitle`, `detail`, `accessory`, `icon`/`glyph`, `view`, `exec`,
`actions`, `fill`, `pending`, `previewExec`/`revertExec`, `setExec`, plus
whatever else the provider sent (`fields`, `submit`, `controls`, `escExec`,
`keepOpen`, `clearTo`, …). Unknown fields pass through untouched.

## Provider protocol

Extensions keep their `*.json` declarations and their stdout contract — a JSON
array, or one JSON object per line. Socket extensions get
`{"epoch", "query", "filters"}` and answer `{"epoch", "rows": […]}`; the epoch
echo is what makes a pushed answer safe. Extensions with `"native": "<name>"`
are answered by a provider compiled into the daemon first and fall back to
`search`/`socket` when it declines — the declaration never stops being true.
