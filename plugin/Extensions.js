.pragma library

// An extension is a JSON file in ~/.config/omarchy/oxy/extensions/.
// It declares a keyword and a command that answers for it, so a new source of
// results is a shell script and a JSON file, in any language, with nothing
// compiled and no QML.
//
//   {
//     "id": "spotify",
//     "title": "Spotify",
//     "keyword": "music",
//     "aliases": ["song", "track"],
//     "search": "oxy-spotify search {query}",
//     "minChars": 2,
//     "debounceMs": 250,
//     "when": "playerctl --list-all | grep -q spotify",
//     "glyph": "",
//     "tier": "substring"
//   }
//
// `search` runs with {query} replaced by the shell-quoted search text, and any
// {filter} replaced by another filter's value, so `music:blue year:1959` can
// reach the script as two arguments. It prints JSON: either an array of rows,
// or one row per line.
//
// An action may set `keepOpen: true` when all it does is change something the
// launcher will show next. Without it an action closes the launcher, which is
// right for anything that starts a program and wrong for anything that does not.
//
// A row is { id, title, subtitle, exec }, plus optional icon, glyph, accessory
// and score. Nothing else is read, so a script can carry its own fields through
// for its own use.
//
// Three optional fields change when the command runs rather than what it says:
//
//   "cacheMs":   0    keep this answer for this many milliseconds
//   "refreshMs": 0    re-run while these rows are on screen, this often
//   "socket":    ""   a unix socket to ask instead of running a command
//
// cacheMs defaults to 0, meaning nothing is cached. That default is the
// conservative one on purpose: an answer that reflects live state the user is
// about to change (what is playing, which containers are up, what is on the
// clipboard) is wrong the moment they act on it, and no heuristic here can tell
// those apart from a dictionary lookup. The author says so, or it is not
// cached. The key is the exact command that would have run, so a cached answer
// can never be served to a different question.
//
// refreshMs defaults to 0, meaning off. When set, the command is re-run on
// that interval and its rows are replaced in place: no spinner, no flicker, and
// the selection stays where it was. It only runs while the launcher is open and
// while this extension's rows are actually on screen, and it stops the moment
// the query changes.
//
// See EXTENSIONS.md for the socket protocol.

var TIERS = { calc: 9, forced: 8, prefix: 7, substring: 6, weak: 5, file: 4, web: 1 }

// A hand-edited number that is not a number used to travel as NaN, and NaN
// fails silently: `i < maxRows` is never true so the extension answered
// nothing, and a NaN timer interval is a spinner that only the next keystroke
// cleared. The default is the value the field documented, not zero.
function num(value, fallback) {
  if (value === undefined || value === null || value === "") return fallback
  var n = Number(value)
  return isFinite(n) ? n : fallback
}

function normalize(raw, sourcePath) {
  var ext = raw || {}
  var id = String(ext.id || "").trim()
  if (!id) return null

  var search = String(ext.search || "").trim()
  var socket = String(ext.socket || "").trim()
  // A socket extension needs no command, so one of the two is enough. An
  // extension with neither cannot answer anything and is dropped rather than
  // sitting in the keyword list as a keyword that does nothing.
  if (!search && !socket) return null

  return {
    id: id,
    title: String(ext.title || id),
    // The keyword defaults to the id, so a minimal extension needs neither.
    keyword: String(ext.keyword || id).toLowerCase(),
    aliases: (ext.aliases || []).map(function (a) { return String(a).toLowerCase() }),
    search: search,
    when: String(ext.when || ""),
    glyph: String(ext.glyph || ""),
    subtitle: String(ext.subtitle || ext.title || id),
    minChars: num(ext.minChars, 1),
    debounceMs: num(ext.debounceMs, 200),
    timeoutMs: num(ext.timeoutMs, 4000),
    maxRows: num(ext.maxRows, 8),
    tier: TIERS[String(ext.tier || "substring")] || TIERS.substring,
    // The layout this extension's results want. A row may override it, which
    // is how `music:` shows one hero for what is playing and cards for the
    // rest of the answer.
    view: String(ext.view || "list"),
    // Unscoped, an extension stays quiet unless it says otherwise. A launcher
    // that shells out to six services on every keystroke is a launcher nobody
    // keeps, so answering a bare query is opt in.
    always: ext.always === true,
    // Off by default, both of them. Caching an answer nobody asked to have
    // cached is how a launcher starts lying, and refreshing on a timer nobody
    // asked for is how it starts costing a process a second per extension.
    cacheMs: Math.max(0, num(ext.cacheMs, 0)),
    refreshMs: Math.max(0, num(ext.refreshMs, 0)),
    // A long-running program answers here instead of being started per
    // keystroke. Empty means there is no such program and `search` is the only
    // way in.
    socket: socket,
    // Slash commands the extension itself owns, as opposed to the actions a
    // row carries. `Actions.fromExtensions` reads this and turned up nothing
    // for as long as normalize forgot to copy it, so the one documented
    // example, `/spotify auth`, could never have fired.
    actions: Array.isArray(ext.actions) ? ext.actions : [],
    source: sourcePath || ""
  }
}

// The key an answer is cached and refreshed under. For a command extension it
// is the command itself, which is the whole question: the query, the filters
// and the script are all already baked into that string. A socket extension
// has no command, so the question is spelled out instead.
function cacheKey(ext, command, argText, filters) {
  if (command) return command
  var nul = String.fromCharCode(0)
  return "socket" + nul + ext.socket + nul + argText + nul + JSON.stringify(filters || {})
}

// {query} and {any-filter} are replaced by shell-quoted values. Anything
// unmatched becomes an empty string rather than being left as a literal brace,
// so a script never receives "{year}" and treats it as a search term.
function buildCommand(ext, argText, filters, quote, settings) {
  var command = ext.search.replace(/\{([a-z0-9_-]+)\}/gi, function (whole, key) {
    key = key.toLowerCase()
    if (key === "query") return quote(argText)
    if (filters && filters[key] !== undefined) return quote(filters[key])
    return quote("")
  })

  return settingsPrefix(settings, quote) + command
}

// What `settings:` collected, handed to the script as environment.
//
// This was the missing half of that feature: the launcher wrote a user's
// answers into oxy.json under the extension's id and nothing ever read them
// back, so three keywords offered settings that did nothing at all.
//
// Environment rather than arguments, because an argument is visible in every
// process listing on the machine and one of the first things anyone will put in
// here is an API token. A script reads its own with $OXY_ORG or
// ${OXY_ORG:-default}, which is how a shell script wants to be configured.
function settingsPrefix(settings, quote) {
  if (!settings) return ""

  var out = ""
  for (var key in settings) {
    // Only what could be an environment name, so a hand-edited config cannot
    // inject a second command through a key.
    if (!/^[A-Za-z][A-Za-z0-9_]*$/.test(key)) continue
    var value = settings[key]
    if (value === undefined || value === null) continue
    out += "OXY_" + key.toUpperCase() + "=" + quote(String(value)) + " "
  }
  return out
}

// Accepts a JSON array, or one JSON object per line. Line mode matters for a
// script that streams, and costs nothing for one that does not.
function parseRows(text) {
  var trimmed = String(text || "").trim()
  if (trimmed === "") return []

  // Anything before the JSON starts is not ours. mise prints a line naming its
  // tools the first time a shimmed binary runs under `bash -lc`, which is
  // exactly how every extension runs, so a perfect answer arrived with one
  // stray line in front of it and parsed as nothing at all. A pretty-printed
  // array cannot fall back to line mode either, so this was silent: the
  // launcher said nothing matched while the same command answered in a shell.
  var start = trimmed.search(/[\[{]/)
  if (start > 0) trimmed = trimmed.slice(start)

  try {
    var whole = JSON.parse(trimmed)
    if (Array.isArray(whole)) return whole
    if (whole && typeof whole === "object") return [whole]
  } catch (e) {
    // Not one document. Fall through to line mode.
  }

  var rows = []
  var lines = trimmed.split("\n")
  for (var i = 0; i < lines.length; i++) {
    var line = lines[i].trim()
    if (line === "") continue
    try {
      rows.push(JSON.parse(line))
    } catch (e2) {
      // One malformed line should not lose the rest of a long answer.
    }
  }
  return rows
}

// Names the launcher owns. A script that sets one of these is naming something
// it does not get to decide, so they are dropped rather than copied through by
// the loop at the end of toRow. `score` is read, but as `local`, on its own
// terms.
var RESERVED = {
  key: true, providerId: true, tier: true, local: true,
  score: true, run: true, pending: true, icon: true, glyph: true
}

// A script's own `score` orders its rows against each other. It never crosses
// tiers, so an extension cannot outrank a calculator answer by returning a big
// number.
function toRow(ext, raw, index) {
  var id = String(raw.id !== undefined ? raw.id : (raw.title || index))
  var local = raw.score !== undefined
    ? Math.max(0, Math.min(99999, Number(raw.score)))
    : Math.max(0, 90000 - index * 1000)

  var row = {
    key: "ext:" + ext.id + ":" + id,
    providerId: ext.id,
    group: String(raw.group || ext.title),
    title: String(raw.title || ""),
    subtitle: String(raw.subtitle !== undefined ? raw.subtitle : ext.subtitle),
    detail: String(raw.detail || ""),
    accessory: String(raw.accessory || ""),
    iconSource: String(raw.icon || ""),
    iconGlyph: String(raw.glyph || ext.glyph),
    art: String(raw.art || ""),
    // Only the first row's view is read, so a script puts the row it wants to
    // set the layout first and the rest follow it.
    view: String(raw.view || ext.view),
    preview: String(raw.preview || ""),
    // Preformatted text: a calendar's columns only line up in a fixed pitch.
    mono: raw.mono === true,
    progress: raw.progress,
    // A month, drawn as a grid rather than as `cal` output.
    year: raw.year,
    month: raw.month,
    today: raw.today,
    weekStart: raw.weekStart,
    marks: Array.isArray(raw.marks) ? raw.marks : null,
    // A player: what is playing, how far in, and the calls to change it.
    status: String(raw.status || ""),
    player: String(raw.player || ""),
    lengthSeconds: raw.lengthSeconds,
    seek: String(raw.seek || ""),
    controls: (raw.controls && typeof raw.controls === "object") ? raw.controls : null,
    shuffle: raw.shuffle === true,
    loop: String(raw.loop || "None"),
    canNext: raw.canNext !== false,
    canPrev: raw.canPrev !== false,
    actions: Array.isArray(raw.actions) ? raw.actions : null,
    // A slider: a number the user drags, and the command that writes it back.
    // min and max default to a percentage because nearly every one of these is
    // one, and a script that means something else says so. `setExec` keeps its
    // literal {value} until the drag ends: only the view knows what the number
    // turned out to be.
    value: raw.value === undefined ? 0 : Number(raw.value),
    min: raw.min === undefined ? 0 : Number(raw.min),
    max: raw.max === undefined ? 100 : Number(raw.max),
    step: raw.step === undefined ? 1 : Number(raw.step),
    setExec: String(raw.setExec || ""),
    tier: ext.tier,
    local: local,
    exec: String(raw.exec || ""),
    pending: false
  }

  // The header above has always told extension authors that a script can carry
  // its own fields through for its own use. It was not true: this function
  // built a row from a fixed list and dropped everything else, so every new
  // view meant another line here and an extension could not experiment at all.
  // Anything the launcher has not already named is copied across untouched,
  // which is also what keeps this list from growing with each view.
  for (var field in raw) {
    if (RESERVED[field]) continue
    if (Object.prototype.hasOwnProperty.call(row, field)) continue
    row[field] = raw[field]
  }

  return row
}
