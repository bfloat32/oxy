# Ωᵡ₄Y

> **This is the `experimental/rust-core` branch** — the same launcher with its
> engine rebuilt in Rust behind a daemon, installed beside the script build
> rather than over it. See **[README-RS.md](README-RS.md)** for what changed,
> how to install it, and how to work on it. Everything below still applies:
> the keywords, the views and the file formats are unchanged.

A launcher. One box that answers with apps, arithmetic, files, git, music, your
notes and the web, and draws each of those the way it deserves rather than as
one long list.

## Install

On an Omarchy system, two ways in — the clone works with your usual git
credentials even when the repo is private; the pipe needs a token in scope
(`$GH_TOKEN`) while it is:

```bash
git clone --depth 1 https://github.com/bfloat32/oxy.git ~/.local/share/oxy
~/.local/share/oxy/install.sh
```

```bash
curl -fsSL -H "Authorization: Bearer $GH_TOKEN" \
  https://raw.githubusercontent.com/bfloat32/oxy/main/install.sh | bash
```

Either way it clones to `~/.local/share/oxy` — or installs the checkout it was
run from — then links the plugin, the `oxy-*` commands, the extension
definitions and the keybinding into place, installs whatever `pacman` has that
the keywords need, and enables it. Everything it links is a symlink into the
clone, so `git pull` in that directory updates all of it, and anything of yours
already at a target path is moved aside with a `.before-oxy` suffix rather than
overwritten. Running the same command again updates; running
`install.sh --uninstall` from the clone takes it back out and leaves your
settings, pins and history.

The installer checks for what the core keywords need — `qalc`, `jq`, `curl`,
`fd`, `mpv`, `python3`, `git`, `wl-copy`, `busctl` (systemd) and `cal`
(util-linux) — and offers to `pacman -S` whatever is missing. Optional tools
like `gh`, `docker`, `playerctl` and `socat` are only named: a keyword whose
`when` test fails simply stays hidden, so nothing you lack ever looks broken.

If an install ever misbehaves: a corrupt checkout repairs itself on the next
run (the broken clone is moved aside, never deleted), and any other failure
offers to wipe the checkout and try again clean. `install.sh --fresh` forces
that clean slate without asking, and `install.sh --purge` uninstalls *and*
deletes the clone.

`Super+K` opens it. To change that, edit the one line at the top of
`hypr/keys.lua`:

```lua
local preset = "balanced"
```

- `additive`: `Super+Shift+K`, and every Omarchy default survives.
- `balanced`: `Super+K`, with the keybindings cheatsheet moved to `Super+H`.
- `full`: the same, and nothing else opens a launcher any more.

Nothing reads this from a settings file, because a Hyprland keybinding has to be
Lua and reaching into JSON from there would be worse than a variable. The
Omarchy menu keeps its bar icon and `Super+Shift+F12` in all three, so `full` is
recoverable without editing config.

**Type `?` to see every keyword your machine actually has.** That list is built
from what is loaded, so it is always true, and this file explains what is in it.

---

## Just start typing

With no keyword at all, four things answer:

| Type | Get |
|---|---|
| `firefox`, `thun` | applications, ranked by match and by what you actually launch |
| `theme`, `lock`, `screenshot` | Omarchy's own commands (24 of them, listed under `>` below) |
| `wiki`, `dl` | your own quicklinks, matched by title or tag |
| `2+2*10`, `40 miles in km` | the answer, large |
| `27 november 2027`, `next friday`, `christmas` | the day, and how far away it is |
| anything with no match | search the web |

The calculator answers unscoped only when the text passes a gate: it must
contain a digit **and** either an operator (`+ - * / ^ % ( )`) or one of `to`,
`in`, `as`. "5" alone is not a question and "a + b" is not arithmetic.

`date:` is the only other extension that answers unscoped, and its own matching
is deliberately narrow, because `date -d` reads "may" as a month and "1" as a
day of this month.

With the box **empty** you may also get:

- a URL sitting on your clipboard, offered to open. Only an explicit scheme
  counts; `github.com/x` on its own does not, because this row appears uninvited.
- your last 20 queries, if you turn them on with `"recents": true`. Off by
  default: half of them are partial words from a search you abandoned.

---

## The four shorthands

| Sigil | Same as | For |
|---|---|---|
| `=` | `calc:` | arithmetic and conversion |
| `>` | `run:` | Omarchy commands |
| `?` | `web:` | search the web |
| `/` | `command:` | the launcher's own actions |

Only a leading sigil counts, and everything after it is the whole query, so
`=2+2` works and `total =2+2` is plain text.

`?` on its own is the exception: with nothing after it there is nothing to
search for, so it lists every keyword instead. `:`, `h:` and `help:` do the
same. `Enter` on one of those rows types `keyword:` into the box, ready for the
rest of the line, and runs nothing.

`/` reaches files when what follows looks like a path: a slash, a dot or a
tilde. `/~/Documents`, `//home/ada` and `/report.pdf` all become file searches.
`/etc` does not, because it has none of the three; that one needs `file:etc`.

---

## Keys

| Key | What |
|---|---|
| `Enter` | run the primary action, always named in the footer |
| `Shift+Enter` | run the second action, also named in the footer |
| `Ctrl+Enter` | ask a model, streamed into the card |
| `Ctrl+K` | every other action this result has |
| `Ctrl+1` to `Ctrl+9` | run that row by the number down its left gutter, without moving |
| `Ctrl+P` | pin the selected row, so it leads every query it matches |
| `Down`, `Ctrl+N`, `Tab` | next |
| `Up`, `Ctrl+Shift+P`, `Shift+Tab` | previous |
| `PageDown` / `PageUp` | a screenful at a time |
| `Left` / `Right` | move a cell, in the grid, dashboard, calendar, docker and marketplace views |
| `Left` / `Right` | adjust, in the slider, timegrid, emoji, themes, windows, menutree and radioplayer views |
| `Escape` | four stages, in order |

Escape unwinds one stage per press: leave a streamed answer, then step back one
query in the trail you walked in on, then clear the box, then close. Hold it and
everything comes off in that order. A row that is still working is told to stop
first. With the action panel open, Escape closes only that.

`Left`, `Right`, `Home`, `End`, `Backspace` and the usual editing chords belong
to the text cursor and are not taken, except in the views named above.

A pin lifts a row the way frecency does and by more, and neither ever crosses a
tier: a name that starts with what you typed still beats a pinned substring.
Calculator and web rows cannot be pinned, since their key is the text you typed.
Pins and recent queries live in `~/.local/state/omarchy/oxy-state.json`,
beside the frecency file rather than in your settings, which stay yours to edit.

---

## Every keyword

Aliases are listed with each one; any of them opens the same thing. A keyword
whose requirement is missing is not loaded at all, so it costs nothing and
appears in neither `?` nor your results.

### Finding things on this machine

| Keyword | Aliases | What | Needs |
|---|---|---|---|
| `file:` | `files`, `f` | filenames, narrowed with `format:pdf` and `in:~/Sync` | `fd` |
| `img:` | `image`, `images`, `pic`, `photo` | image thumbnails, newest first, with size and dimensions | `fd`; dimensions need ImageMagick |
| `recent:` | `recents`, `last`, `opened` | files you opened lately in any app, minus what has since been deleted | `~/.local/share/recently-used.xbel` |
| `win:` | `window`, `windows`, `w` | every open window by class or title; focus it or close it | Hyprland |
| `kill:` | `ps`, `proc`, `process`, `top`, `quit` | find a running process and end it | nothing extra |
| `ssh:` | `host`, `hosts` | a host from your ssh config, connected in a terminal | `~/.ssh/config` |
| `repo:` | `repos`, `project`, `projects` | your local git repos, most recently touched first, filterable with `branch:main` | `git`, `fd` |
| `herdr:` | `agents`, `herd` | your coding agents, the ones waiting on you first; Enter focuses one and raises its terminal | `herdr` |

`kill:` answers nothing for a bare `kill:` and wants two characters, so you
cannot fat-finger your way into the process list.

`herdr:` asks every running herdr session, and reading never marks a pane seen,
so a keystroke here cannot erase the state it exists to report.

### Git, on disk

| Keyword | Aliases | What | Needs |
|---|---|---|---|
| `git:` | none | one repo as a panel: branch, what is uncommitted, drift, stash count, recent commits | `git` |
| `branch:` | `branches` | every local branch with ahead/behind against upstream and against trunk; Enter switches | `git` |
| `stash:` | `stashes` | what is actually inside each stash, per file, so you can read one before applying it | `git` |

All three answer about the repo your focused terminal is in, else the one you
touched last; naming one (`git:omarchy`) answers about that repo instead. There
is no `stash drop` anywhere, on purpose. `git:` deliberately does not show pull
requests or issues; that is `gh:`.

### GitHub

All four need `gh` and `jq` installed, `gh auth` signed in, and the network.
Without an account they say so on a titled row rather than looking empty.

| Keyword | Aliases | What |
|---|---|---|
| `gh:` | `github` | your repos, newest-pushed first; `gh:owner/repo`, `gh:owner/repo#123` or a pasted GitHub URL resolves directly |
| `pr:` | `prs`, `pulls` | your pull requests, a repo's open ones, or one specific PR with its checks, reviews and size |
| `issue:` | `issues` | issues assigned to you, a repo's open ones, or one specific issue |
| `ci:` | `workflow`, `workflows`, `actions` | workflow runs for a named repo. A bare `ci:` shows you the shape `ci:owner/repo` rather than guessing |

A slug, a URL or an `owner/repo#123` draws its card and is pressable **before**
any request goes out. `gh:` and `pr:` refresh themselves every 900ms while their
rows are on screen.

### Answers

| Keyword | Aliases | What | Needs |
|---|---|---|---|
| `calc:` | `math`, `=` | the sums you have already accepted, newest first; typing an expression puts the live answer on top | qalc, and a non-empty history |
| `unit:` | `units`, `convert`, `conv` | unit conversion, with the phrasing fixed up so `40 miles in km` and `180f in c` mean what you said | `qalc` |
| `def:` | `define`, `dict`, `syn`, `word` | every sense of a word by part of speech, with synonyms | `curl` and the network |
| `date:` | `when`, `day`, `days` | a date, a duration, a named day or the gap between two dates | GNU `date` |
| `cal:` | `calendar`, `month` | a month drawn as a month. `cal:december`, `cal:november 2027`, `cal:2027` for all twelve | `cal` |
| `tz:` | `time`, `timezone`, `zone`, `clock` | what time it is elsewhere, and what your time is there | `jq`, `python3` |
| `sys:` | `system`, `status`, `battery`, `info` | battery, uptime, memory, disk, address, temperature | nothing extra |

`unit:` knows its own traps, each found by testing: `in` is a preposition and
not inches, a bare `f` is Fahrenheit and not farads, a bare `k` is kelvin only
next to a temperature, and a word that is not a unit answers nothing rather than
qalc's `0 B`.

`def:` keeps what it fetched under `~/.cache/oxy/define` for 30 days.

`date:` answers five shapes of question:

| Type | Get |
|---|---|
| `27 november 2027`, `25/12/2026` | the weekday, written out |
| `in 90 days`, `3 weeks ago` | the day that lands on |
| `christmas`, `easter 2028` | the day it falls on that year |
| `from 1 jan to today` | the gap, in days and in weekdays |
| `week 34 2027` | the days that week covers |

Easter is computed rather than looked up, so the four days that hang off it stay
right in any year. Every answer is a day and never an instant, and the day
arithmetic runs at noon, so a clock change cannot move a count by one.

`calc:` and `=` decide how a number is written per call rather than reading
`~/.config/qalculate/qalc.cfg`. Exponent notation starts at 10^21 in both
directions, so `2^32` is `4294967296` and not `4.29497E9`. An answer never
carries fewer significant digits than the number you typed into it. Money is
cut to two decimals, and `12% of 250` is read as the multiplication you meant.

`repo:` looks in `$OXY_REPO_ROOTS` (colon separated) when that is set, and
otherwise in whichever of `~/localhost`, `~/Projects`, `~/Work`, `~/src`,
`~/code`, `~/dev`, `~/repos`, `~/git` and `~/Developer` exist.

### Text you keep having to produce

| Keyword | Aliases | What | Needs |
|---|---|---|---|
| `emoji:` | `emojis`, `e`, `smiley`, `symbol` | the emoji by name, copied | Omarchy's emoji data |
| `snip:` | `snippet`, `snippets`, `text`, `expand` | boilerplate you keep retyping, matched by name or body | `~/.config/omarchy/oxy-snippets.json` |
| `ch:` | `clip`, `clipboard`, `paste` | what you copied, newest first, with the full text beside the list | Omarchy's clipboard history |
| `note:` | `notes`, `n`, `notepad` | your notes, and `note:new tuesday standup` to start one | nothing extra |
| `pass:` | `secret`, `password`, `1p` | an entry from `pass` or 1Password, copied, cleared after 45 seconds | `pass`, or `op` signed in |

The secret only ever reaches `wl-copy`; nothing is passed on a command line.

### Music and radio

| Keyword | Aliases | What | Needs |
|---|---|---|---|
| `spotify:` | `music`, `song`, `track`, `play` | bare: the player, with cover, scrubber and transport. With words: search and play, with no API account | the Spotify client; `curl`, `mpv` and the network for search |
| `sp:` | `spotify`, `track`, `album`, `artist` | the real Spotify Web API search: tracks, then albums, then artists, narrowed with `type:album` | a completed `oxy-spotify-auth` |
| `radio:` | `fm`, `station`, `stream` | internet radio from radio-browser.info, played through mpv | `mpv`, `curl`, the network |

Both music keywords claim `spotify` and `track`, so which one answers depends on
which is loaded; `sp:` and `music:` are the two names that are never ambiguous.

### This desktop

| Keyword | Aliases | What | Needs |
|---|---|---|---|
| `theme:` | `themes`, `colours`, `colors`, `appearance` | every Omarchy theme. Moving the selection applies it live; leaving without choosing puts yours back | Omarchy |
| `omarchy:` | `oma`, `menu`, `settings-menu`, `o` | every route in Omarchy's own menu, flattened into one searchable line each ("Theme · Style") | Omarchy |
| `vol:` | `volume`, `audio`, `sound`, `mute` | output and input volume as sliders, and mute. `vol:mic` for the input alone | `pactl` (PipeWire or PulseAudio) |
| `bri:` | `brightness`, `screen` | the focused display's brightness, as a slider | a controllable backlight under Hyprland |
| `wifi:` | `wlan`, `network` | networks in range and saved ones, active first; connect, enter a password, speedtest | `nmcli` (NetworkManager) |
| `bt:` | `bluetooth` | paired bluetooth devices, connected first; connect, disconnect, toggle the radio | `bluetoothctl`, and something paired |
| `docker:` | `container`, `containers` | your containers, running first, with logs, a shell, and start/stop | a docker daemon actually answering |
| `alarm:` | `timer`, `remind`, `reminder` | a reminder said the way you would say it: `alarm:25m tea is ready` | Omarchy |
| `shortcuts:` | `keys`, `keybindings`, `binds`, `hotkeys`, `keymap`, `kb` | every key bound on this machine, as a keymap. Search the action or the combination | `hyprctl` |

`docker:` is gated on the daemon answering rather than on the binary existing,
so a stopped daemon hides the keyword instead of giving you an empty list.

`shortcuts:` reads `omarchy-menu-keybindings --print` when it is there, because
Hyprland reports an Omarchy Lua bind as dispatcher `__lua` with a number and
reports a `code:` bind with no key at all. `hyprctl binds -j` is the fallback,
and it can only report the binds that still carry a key. The launcher's own keys
are in neither source, so they are listed too and marked Oxy. Enter copies
the combination and never fires it.

### The launcher itself

| Keyword | Aliases | What |
|---|---|---|
| `?` | `:`, `h:`, `help:` | every keyword loaded right now, in three groups: built in, extensions, quicklinks |
| `settings:` | none | the extensions that declare settings; picking one opens its form |
| `apps:` | `app`, `launch` | applications only |
| `run:` | `commands`, `>` | Omarchy commands only |
| `web:` | `search`, `google`, `ddg`, `?` | search the web with the default engine |
| `do:` | `agent`, `ai` | hand a local coding agent an instruction, and watch what it does |
| `bo:` | `market`, `marketplace` | browse your marketplaces and switch a unit on or off. Needs `bo` |

Three extensions declare settings today: `def:` (how long to keep definitions),
`repo:` (where your repos live) and `tz:` (which zones to show). The form saves
them to `extensionSettings` in your config, under each extension's id. The
launcher hands them to the script as environment: a key `cacheDays` arrives as
`$OXY_CACHEDAYS`. Environment rather than an argument, because an argument
is visible in every process listing on the machine.

All three read theirs: `def:` honours `$OXY_CACHEDAYS`, `repo:` honours
`$OXY_ROOTS`, and `tz:` honours `$OXY_ZONES` — a comma-separated line of zone
names resolved the same loose way a `tz:` query is. The env value wins over
`timezones` in `oxy.json`, because the thing just typed means more than the
file edited last month.

### `/` actions

Only reachable with `/`, never unscoped, because an instruction that turns up
uninvited beside your search results is a way to clear your history by accident.

| Type | What |
|---|---|
| `/clear` | forget the recent queries |
| `/clear-pins` | forget the pins |
| `/clear-all` | both. Asks first: the second Enter is the answer |
| `/reload` | rescan `~/.config/omarchy/oxy/extensions` |
| `/settings` | types `settings:` for you |
| `/config` | open `~/.config/omarchy/oxy.json` in your editor |
| `/stats` | extensions loaded, answers cached, `when` checks and how many pass |
| `/logs` | open the event log in your editor |

An extension file may also carry an `actions` block, and those join this list:
each is namespaced by the extension's id, so two extensions can both offer
`auth` without one shadowing the other. See docs/EXTENSIONS.md for the shape.

---

## The marketplace: `bo:`

`bo:` is better-omarchy inside the launcher. It has three levels. You walk them
with `Enter` and leave them with `Escape`.

| Level | What is on it |
|---|---|
| home | your marketplaces, and the units you have on |
| a marketplace | its units, as tiles |
| a unit | its own page: what it does, what it needs, and the switch |

Typing narrows the level you are on and nothing else. On home a search reaches
every unit and not only the ones you have, and the section takes the search as
its label to say so. The reserved words are the kinds (`plugin`, `hypr`,
`setting`), the states (`on`, `off`, `unavailable`) and the categories units
declare. They are matched exactly, and a query that matches none of them is
retried as plain text.

The level is an address written into the box: `bo:@<marketplace>` is one
marketplace and `bo:#<market>/<unit>` is one unit. Nobody types those. The
launcher writes them when you press `Enter`, which is what keeps the level alive
while you type.

A unit page carries two rows: the way out, and the switch. Arriving selects the
way out, so the first key you can press undoes the navigation rather than
changing a system component. One `Down` lands on the switch and `Enter` commits.

Turning a plugin unit on or off closes the launcher, runs `bo add` or
`bo remove`, and summons the launcher back on the page you were on. It has to.
The shell watches `~/.config/omarchy/plugins`, and a change in that directory
unloads every panel, overlay and menu plugin. This launcher is an overlay.
Nothing in the sequence is a timed sleep: the script waits for the surface to
go, then for its own next invocation to come back carrying the address it asked
for. A `hypr` or a `setting` unit changes nothing the shell watches, so its
switch moves with the window still up.

The unit that ships this launcher is the one that does not come back. Its page
says so.

A unit whose `needs` are not all on this machine cannot be turned on. `bo:`
draws it faintest of the three states, with the reason where its summary would
be, so you learn it before you press anything.

### The deeplink

That summon is a public entry point. Any payload with a `query` key opens the
launcher on that query:

```bash
omarchy-shell shell summon oma.oxy '{"query":"bo:"}'
```

A payload that will not parse opens the launcher empty rather than not at all.
With no payload the launcher opens empty, or on your last query when
`resetOnOpen` is `false`.

---

## Doing something: `do:`

`Ctrl+Enter` asks a model a question. `do:` hands a local agent an instruction
and lets it act.

    do: open a new workspace and split it into four terminals
    do: now put my editor in the top left one
    do: find every TODO in this repo and list them

It is a chat. `Enter` sends and empties the box, what you sent stays on the card
above the answer, and the next sentence continues the same session, so "that
file" and "now make it four" mean something. `do: /new` starts over, and
`do: @codex ...` picks which agent takes it.

While it works you get three lines: what it is doing now, the last few things it
did, and the answer growing under them. Each line names the thing rather than
the tool -- `Go to an empty workspace`, `Open 4 x terminal`, `Read Launcher.qml`
-- because "Bash" thirty times is a progress bar with extra steps. When it
finishes those collapse to a count and a clock, and the answer is what is left.

Where the line is:

- **It is allowed to act.** You typed the sentence into your own launcher, on
  your own machine, and pressed Enter. That is the consent, and asking again for
  every window is how a launcher becomes something you stop using. The shell,
  writing files and editing them are all pre-approved.
- **Confirmation is kept for what cannot be undone.** Deleting, `sudo`, package
  management, credentials, and anything that leaves this machine (`git push`,
  `gh`, `ssh`, `curl`) come back refused, named, with the reason. Type
  `do: /policy` to read the whole list.
- **`Ctrl+K` is the way through one.** It hands the same instruction to an
  interactive agent in a terminal, where the permission prompts exist and you
  answer them.
- **Typing runs nothing.** An instruction comes back as something to look at:
  which agent, which directory. `Enter` starts it.
- **`Enter` carries a token, not your sentence.** The row's command names a hash
  of what was previewed, so nothing you typed reaches a shell, and nothing can
  run that was not on screen first.
- **`Escape` stops it**, and stopping kills the process group rather than the
  one pid the agent told us about. Closing the launcher stops a run too, a
  couple of seconds later, which is why long unattended work belongs in a
  terminal.

The deny list is a speed bump on a model that means well, not a sandbox: `bash
-c` is still a shell. What actually keeps this safe is that a person typed the
sentence and pressed a key.

It knows this desktop. Hyprland here is configured in Lua, where `hyprctl
dispatch workspace 9` is not merely wrong but can return `ok` and do nothing;
the agent is told the real form, and `oxy-agent desk` gives it the
dispatchers already spelled right, waiting for windows to actually appear:

```
oxy-agent desk empty              # the lowest workspace with nothing on it
oxy-agent desk tile 4 terminal    # four of them, splitting the largest each time
oxy-agent desk help               # the rest
```

It needs `claude`, `codex` or `gemini` on `PATH`. With none of them there the
keyword still answers, saying so, rather than going quiet and looking broken.

---

## Asking a model: `Ctrl+Enter`

Streams an answer into the card without leaving the launcher.

Providers are tried in order and the first one installed wins, so if you already
have the Claude, Codex or Gemini CLI signed in this works with no setup. Ollama,
aichat and mods are in the list too. Availability is probed once at startup,
never per keystroke.

Pin one with `"askProvider": "gemini"`, or add your own. A provider's command
gets `{query}` shell-quoted and `{model}` unquoted, has to write to stdout as it
goes, and has to exit when it is done. It runs with stdin closed and stderr
folded in, because a CLI that reads stdin otherwise waits, and some print a
warning into the middle of the answer.

Escape leaves the answer. So does typing.

A multi-turn `ask:` chat surface — subscription CLIs, API-keyed endpoints and
local models behind one provider registry — is designed in
[docs/LLM-INTEGRATION.md](docs/LLM-INTEGRATION.md).

---

## Settings

`~/.config/omarchy/oxy.json`, watched, so an edit takes effect on the next
keystroke. `/config` opens it.

```json
{
  "recents": false,
  "defaultEngine": "google",
  "engineActions": ["google", "chatgpt", "ddg", "youtube", "github"],
  "quicklinks": [
    { "title": "GitHub", "keyword": "gh", "tags": ["dev"],
      "url": "https://github.com/search?q={}" },
    { "title": "Downloads", "keyword": "dl",
      "open": "nautilus --new-window ~/Downloads" }
  ],
  "extensions": { "radio": false },
  "notesDir": "~/Documents/Notes",
  "askProvider": "claude",
  "frecency": true,
  "maxRows": 9,
  "cardWidth": 620,
  "resetOnOpen": true
}
```

| Key | Default | What |
|---|---|---|
| `recents` | `false` | offer your last 20 queries when the box is empty |
| `defaultEngine` | `"google"` | which engine `Enter` uses on a web row |
| `engines` | six built in | merged over the built-ins **by id**, so adding one does not mean restating Google, DuckDuckGo, ChatGPT, Perplexity, YouTube and GitHub |
| `engineActions` | five of them | which engines appear under `Ctrl+K`, in that order. Perplexity ships configured but unlisted |
| `quicklinks` | `[]` | your own links, see below |
| `extensions` | `{}` | name one `false` to silence it. Absent means on, so an extension that arrives in an update starts working |
| `extensionSettings` | `{}` | what `settings:` writes, under each extension's id so two extensions wanting a `token` cannot read each other's |
| `notesDir` | `~/Documents/Notes` | where `note:` keeps its markdown |
| `askProvider` | `""` | force one `Ctrl+Enter` provider by id; a typo falls back to the list rather than going silent |
| `frecency` | `true` | rank by what you actually use. `false` ranks purely on match quality; pins still apply |
| `maxRows` | `9` | rows shown, and what PageUp/PageDown jump by |
| `cardWidth` | `620` | the card, in unscaled pixels |
| `resetOnOpen` | `true` | `false` reopens on your last query |
| `log` | `true` | one JSON event per line in `~/.local/state/omarchy/oxy-log.jsonl`, rotated at 1MB. `false` stops writing |

## The event log

Every keystroke, provider answer, timeout, stale drop and launch lands as one
JSON line in `~/.local/state/omarchy/oxy-log.jsonl`. `/logs` opens it.

The format is built to be analyzed, not just read: every event carries `ts`
(epoch ms), `sid` (one shell session), `ev` (what happened) and, where a query
is involved, `ep` — the query epoch that ties a `prov.start` to its
`prov.done`, `prov.timeout`, or `drop`. Hand the file to an agent and ask what
misbehaved; it can tell a slow extension (`prov.done` with a big `ms`) from a
dead one (`prov.timeout`, `prov.fail`), a daemon writing garbage (`sock.bad`)
from a daemon that is not there (`prov.start` `via:sock` followed by a
fallback), and an empty answer that was real from one that was a timeout hidden
behind stale rows (`prov.stale`).

```json
{"ts":1758631234551,"sid":"mf2xk9","ev":"query","ep":41,"q":"file:report","s":"file"}
{"ts":1758631234589,"sid":"mf2xk9","ev":"prov.start","ep":41,"via":"proc","cmd":"oxy-search-files 'report'","id":"files"}
{"ts":1758631234702,"sid":"mf2xk9","ev":"prov.done","ep":41,"via":"proc","rows":8,"ms":113,"id":"files"}
```

It costs almost nothing: lines are buffered and appended once per 400ms tick,
and the file can never grow past 2MB including its `.old` rotation. It records
what you typed — that is the point of it — and it never leaves the machine.
Set `"log": false` in `oxy.json` to turn it off.

A quicklink is found two ways, because people reach for both: type part of its
title or tag and it appears among everything else, or type its keyword and the
rest of the line becomes the argument in `{}`. Finding it by name cannot also
supply an argument, since the words that found it are not what goes in the
placeholder. With no argument, the placeholder and everything after it is
trimmed, so `gh` alone opens the site rather than an empty search. `open`
replaces `url` when the link should run something instead. An extension that
owns the same keyword wins.

---

## Timezones

`tz:` reads its zones from `timezones` in `~/.config/omarchy/oxy.json`:

```json
{
  "timezones": [
    { "label": "Ana", "zone": "Europe/Lisbon" },
    { "label": "Kenji", "zone": "Asia/Tokyo" },
    { "label": "UTC", "zone": "UTC" }
  ]
}
```

A label is a name, not a caption. `tz:ana` finds Ana, and the answer says Ana
rather than Lisbon, because the question was never really about Lisbon. Zones
are the IANA names `timedatectl list-timezones` prints, and a zone that is not
in that list is dropped rather than silently read as UTC.

Without the key you still get a working `tz:`: San Francisco, New York, London,
Berlin and Tokyo, which is most of a working day. UTC is not among them, because
it is a reference for machines and not a place anybody is. `tz:utc` still
answers.

Zone names match loosely, so `tokyo` finds `Asia/Tokyo`, `sp` finds
`America/Sao_Paulo` and `ny` finds `America/New_York`. Initials beat substrings,
which is what keeps `la` on Los Angeles rather than on Blantyre. A country
resolves to its main city, so `spain` is Madrid.

`tz:3pm tokyo` reads the time as yours and answers in theirs. `tz:9am tokyo in
london` reads it as Tokyo's. That is the difference an explicit `in` makes, and
it is the way both sentences are meant out loud.

`tz:john:tokyo maria:spain` draws a day grid instead: one row per person, one
shaded cell per hour, so a meeting slot is a band of colour rather than a sum.
`me:` puts you in it.

Paste a Discord timestamp in and every zone reads it. `Ctrl+K` on any answer
copies it back out as one, in all nine of Discord's styles, since working out
that 3pm is 9pm for somebody else is usually the step before telling them.

---

## Snippets

`snip:` reads `~/.config/omarchy/oxy-snippets.json`, a flat object of name
to text:

```json
{
  "sig": "Ada\nhttps://example.com",
  "addr": "1 Example Street\nLisbon"
}
```

`Enter` copies one. `Ctrl+K` offers typing it into whatever window had focus,
through `wtype`. Until that file exists the keyword is not loaded at all, so an
empty snippet list costs nothing and says nothing.

---

## Playing music without an account

`spotify:daft punk` searches and plays with no key, no account and no developer
app, through a chain worth knowing about because each link is doing work.

Deezer's search endpoint is keyless and returns an ISRC on every row.
MusicBrainz maps an ISRC to a Spotify track URL. Spotify's Linux client
implements MPRIS `OpenUri`. That resolves the exact track about six times in
ten; the rest open that search inside Spotify, which is one keypress from right
rather than zero.

Three things that had to be got right, each found by testing:

- With shuffle on, `OpenUri` plays a random track from the same context instead
  of the one asked for. Shuffle is turned off first.
- A bad track id is accepted silently and changes nothing, so success is the
  track id moving rather than the call returning.
- `xdg-open` does nothing for `spotify:` URIs even though it claims the handler.
  Only D-Bus works.

Searching Spotify's own catalogue properly needs the Web API, which needs a
Spotify developer app. That ships here too, as `sp:`, and stays silent until you
run `oxy-spotify-auth`. See [docs/SPOTIFY-LIBRARY.md](docs/SPOTIFY-LIBRARY.md).

---

## The built-in extensions

Each keyword above is one `oxy-*` script in `bin/` plus a JSON file in
`config/omarchy/oxy/extensions/` — nothing compiled, nothing hidden. They
are the working examples to copy when writing your own; the full field and
row reference is [docs/EXTENSIONS.md](docs/EXTENSIONS.md).

A few design decisions worth knowing because they are easy to break:

- **`repo:` discovery is cached, status is not.** The list of repositories
  under your roots is scanned once and reused; each row's branch and dirty
  state is computed live and re-validated after the answer shows, so a
  commit lands on screen one query later, not never. Untracked files count
  toward dirty, and git worktrees — a `.git` *file*, not a directory — are
  discovered too.
- **Structured data never travels through shell quoting.** Scripts emit
  fields separated by a unit separator, so a tab or newline inside a commit
  subject, stash message or filename cannot corrupt the row carrying it.
  And anything untrusted — a Wi-Fi SSID, a branch name — is quoted before
  it touches a shell.
- **`file:` matches literally.** `a.b` finds `a.b`, not `axb` — `fd` runs
  fixed-string, and `file://` artwork URLs are percent-encoded so `#` and
  spaces in a name still draw a thumbnail.

---

## Writing your own extension

`bo new extension weather` writes a unit that already answers. Add it, type
`weather:hello`, and the row comes back with hello in it. Then replace the body
of the script and keep the shape.

An extension is a JSON file in `~/.config/omarchy/oxy/extensions/` naming a
keyword and a command:

```json
{
  "id": "weather",
  "keyword": "wx",
  "aliases": ["forecast"],
  "title": "Weather",
  "search": "my-weather-lookup {query}",
  "when": "command -v my-weather-lookup",
  "view": "hero"
}
```

The command prints JSON, either an array or one row per line, so it can be a
shell script, a Python file, or anything else that writes to stdout. Anything
printed before the JSON starts is skipped, because tools like mise announce
themselves the first time a shimmed binary runs.

The fields that matter most:

| Field | What |
|---|---|
| `when` | a shell test run once at load, with an eight-second bound — an extension for software you do not have costs nothing |
| `filters` | extra `name:value` filters to parse out of the query; undeclared, `year:1959` stays literal text |
| `cacheMs` / `refreshMs` | both off by default, on purpose: live state is wrong the moment you act on it, and a timer nobody asked for is a process a second |
| `view` | one of ~37 layouts — `list`, `hero`, `calendar`, `timegrid`, `gitrepo`, `slider`, `agent`, `marketplace`… the full table is in [docs/EXTENSIONS.md](docs/EXTENSIONS.md) |
| `socket` | a unix socket to ask instead of a process per keystroke |

Rows are `{ id, title, subtitle, exec }` plus whatever your own view needs:
fields the launcher has not already named pass through untouched, so a new
view needs no change to the row builder. `fill`, `keepOpen`, `clearTo` and
`escExec` decide what `Enter` and `Escape` do to the row, and an action's
`query` navigates instead of closing.

The complete schema — every extension field, every row field, the socket
protocol, `settings:`, and how `.cases.json` assertions work — is
[docs/EXTENSIONS.md](docs/EXTENSIONS.md).

---

## Files

```
plugin/          the QML: Launcher.qml, one Result*.qml per view, the .js logic
bin/             one script per built-in extension
config/          the extension JSON, mirrored into ~/.config on add
tests/           the check suite: run.sh, logic, behavior and case tests
docs/            EXTENSIONS.md, SPOTIFY-LIBRARY.md, LLM-INTEGRATION.md
```

A `<name>.cases.json` sits beside `<name>.json` in the same directory and holds
the assertions `bo test` — and this repo's own `tests/cases.py` — run against
that keyword. Seventeen keywords ship one. The launcher globs the whole
directory and a cases file survives only because it is an array rather than an
object, so keep the two names in step.

---

## Testing

```bash
tests/run.sh                  # everything CI runs, locally
tests/behavior.test.sh        # end-to-end script checks, in sandboxes
python3 tests/cases.py        # the shipped .cases.json fixtures
python3 tests/cases.py repo   # one extension's cases
node --test tests/logic.test.mjs
```

The suite is four layers deep. Static checks cover bash and python syntax,
shellcheck, executable bits, extension JSON and case-file shape, duplicate
ids and keywords. The logic tests exercise the launcher's JavaScript modules
— query parsing, filter handling, command building, caching, frecency —
without a shell runtime. The behavior suite runs the real `bin/` scripts
against temporary fixtures: hostile Wi-Fi SSIDs, git worktrees, dirty-tree
detection, stale-while-revalidate caching, delimiter-safe parsing, IPv6
docker URLs, multi-alias ssh hosts, half-hour timezones, and the installer's
backup/restore cycle. The case runner answers the shipped `.cases.json`
files in a throwaway `HOME` with a fixture git playground, and skips —
rather than fails — an extension whose tools or data service are not on the
machine.

Nothing in the suite touches your real state: every test gets its own
`HOME`, `XDG_*` and repository roots. What cannot run somewhere is skipped
and says why — `setsid`, `qalc`, `cal`, an authenticated `gh`, tzdata — so a
green run means what ran, held, and the output lists what did not run.

GitHub Actions runs the same layers in two jobs — `static` and `behavior` —
on every push and pull request, with `gh` authenticated so the live GitHub
cases run for real.

### What is not covered

The QML half is verified statically, not on a running shell — there is no
Quickshell in CI. The provider race handling, theme preview and navigation
clamps are logic-reviewed but worth a smoke test on a real install. Anything
hardware-bound — wifi, bluetooth, brightness, volume — is stub-tested or
skipped rather than faked.

---

## Troubleshooting

- **A keyword is missing.** Its `when` test failed — the tool it needs is
  not installed. `?` lists only what loaded. `/stats` shows every loaded
  extension, cached answers, and how many `when` checks pass.
- **A row does nothing.** Check `journalctl --user -n 50 | grep -iE
  "TypeError|error"` after `omarchy restart shell`: a TypeError inside a
  delegate binding renders a blank row rather than failing loudly.
- **Something misbehaved earlier.** `/logs` opens the event log — one JSON
  line per keystroke, provider start/finish, timeout and stale drop, tied
  together by query epoch. Hand it to an agent and ask.
- **Install went sideways.** `install.sh` moves a broken clone aside rather
  than deleting it; `--fresh` wipes and reinstalls, `--purge` uninstalls and
  deletes the clone, `--uninstall` restores your `.before-oxy` files.
- **Before reporting**, run `tests/run.sh` in the clone — if it is green
  and your machine is not, the difference is the bug report.
