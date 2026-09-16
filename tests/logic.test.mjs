// Logic tests for the .pragma library modules — the parts of the launcher that
// can break without ever drawing a pixel: query parsing, command building,
// caching, ranking. Run with: node tests/logic.test.mjs
//
// The modules are QML library files, not ES modules, so each is loaded by
// stripping the .pragma line and evaluating it inside a Function scope whose
// return value names the functions to export.
import { test } from "node:test"
import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { fileURLToPath } from "node:url"
import { dirname, join } from "node:path"

const pluginDir = join(dirname(fileURLToPath(import.meta.url)), "..", "plugin")

function loadModule(file, names) {
  const src = readFileSync(join(pluginDir, file), "utf8")
    .replace(/^\.pragma library\s*$/m, "")
  return new Function(`${src}\nreturn { ${names.join(", ")} }`)()
}

const Query = loadModule("Query.js",
  ["parse", "routesTo", "argFor", "extras", "looksLikePath"])
const Extensions = loadModule("Extensions.js",
  ["normalize", "buildCommand", "settingsPrefix", "parseRows", "toRow", "cacheKey"])
const Cache = loadModule("Cache.js",
  ["get", "getStale", "put", "drop", "clear", "stats"])
const Frecency = loadModule("Frecency.js",
  ["record", "apply", "parse", "prune", "boost"])
const Settings = loadModule("Settings.js", ["merge", "providers"])

const KNOWN = ["calc", "file", "music", "format", "in", "type"]

// ------------------------------------------------------------------ Query

test("sigil rewrites to its filter", () => {
  const q = Query.parse("=2+2", 1, KNOWN)
  assert.equal(q.scope, "calc")
  assert.equal(q.filters.calc, "2+2")
  assert.equal(q.text, "")
})

test("/ with a path routes to file, not command", () => {
  const q = Query.parse("/etc/hosts", 1, KNOWN)
  assert.equal(q.scope, "file")
  const cmd = Query.parse("/reboot", 1, KNOWN)
  assert.equal(cmd.scope, "command")
})

test("a declared extra filter is parsed out of the text", () => {
  const known = KNOWN.concat("year")
  const q = Query.parse("music:blue year:1959", 1, known)
  assert.equal(q.filters.music, "blue")
  assert.equal(q.filters.year, "1959")
  assert.deepEqual(Query.extras(q, "music", []), { year: "1959" })
})

test("an undeclared filter stays literal text", () => {
  const q = Query.parse("music:blue year:1959", 1, KNOWN)
  assert.equal(q.filters.year, undefined)
  assert.match(q.text, /year:1959/)
})

test("a URL is not read as a filter", () => {
  const q = Query.parse("https://example.com", 1, KNOWN)
  assert.equal(q.filters.https, undefined)
  assert.match(q.text, /https:\/\/example\.com/)
})

test("routesTo respects scope and aliases", () => {
  const q = Query.parse("music:blue", 1, KNOWN)
  assert.equal(Query.routesTo(q, "music", []), true)
  assert.equal(Query.routesTo(q, "song", ["music"]), true)
  assert.equal(Query.routesTo(q, "file", []), false)
  assert.equal(Query.routesTo(Query.parse("blue", 1, KNOWN), "file", []), true)
})

test("argFor merges the filter value and the free text", () => {
  const q = Query.parse("file:report budget", 1, KNOWN)
  assert.equal(Query.argFor(q, "file", []), "report budget")
})

// -------------------------------------------------------------- Extensions

const EXT = {
  id: "spotify",
  keyword: "music",
  aliases: ["song"],
  filters: ["year"],
  search: "oxy-spotify search {query} --year {year}",
}

test("normalize copies the declared filters lowercased", () => {
  const e = Extensions.normalize({ ...EXT, filters: ["YEAR"] }, "/x.json")
  assert.deepEqual(e.filters, ["year"])
})

test("normalize guards hand-edited numbers", () => {
  const e = Extensions.normalize({ ...EXT, maxRows: "many", debounceMs: "fast" }, "/x.json")
  assert.equal(e.maxRows, 8)
  assert.equal(e.debounceMs, 200)
})

test("normalize drops an extension with no way to answer", () => {
  assert.equal(Extensions.normalize({ id: "x" }, "/x.json"), null)
  assert.equal(Extensions.normalize({}, "/x.json"), null)
})

test("buildCommand substitutes declared filters and query", () => {
  const e = Extensions.normalize(EXT, "/x.json")
  const cmd = Extensions.buildCommand(e, "kind of blue", { year: "1959" }, q => `'${q}'`, null)
  assert.equal(cmd, "oxy-spotify search 'kind of blue' --year '1959'")
})

test("buildCommand empties a placeholder nobody filled", () => {
  const e = Extensions.normalize(EXT, "/x.json")
  const cmd = Extensions.buildCommand(e, "blue", {}, q => `'${q}'`, null)
  assert.equal(cmd, "oxy-spotify search 'blue' --year ''")
})

test("settingsPrefix only emits valid env names", () => {
  const quote = s => `'${s}'`
  const out = Extensions.settingsPrefix({ roots: "~/src", "bad;rm": "x", "9bad": "y" }, quote)
  assert.match(out, /OXY_ROOTS='~\/src'/)
  assert.doesNotMatch(out, /bad/i)
})

test("parseRows skips a mise-style preamble line", () => {
  const rows = Extensions.parseRows('mise: activated tools\n[{"id":"a","title":"A"}]')
  assert.equal(rows.length, 1)
  assert.equal(rows[0].id, "a")
})

test("parseRows accepts one JSON object per line", () => {
  const rows = Extensions.parseRows('{"id":"a","title":"A"}\n{"id":"b","title":"B"}')
  assert.equal(rows.length, 2)
})

// ------------------------------------------------------------------- Cache

test("cache serves fresh, then stale, then nothing", () => {
  Cache.clear()
  Cache.put("x", "k", [{ id: "a" }], 1000, 100)
  assert.equal(Cache.get("x", "k", 500)[0].id, "a")
  assert.equal(Cache.get("x", "k", 2000), null)
  assert.equal(Cache.getStale("x", "k")[0].id, "a")
})

test("a zero ttl stores nothing", () => {
  Cache.clear()
  Cache.put("x", "k", [{ id: "a" }], 0, 100)
  assert.equal(Cache.get("x", "k", 500), null)
  assert.equal(Cache.getStale("x", "k"), null)
})

test("stats counts buckets and entries", () => {
  Cache.clear()
  Cache.put("x", "a", [], 1000, 0)
  Cache.put("y", "b", [], 1000, 0)
  const s = Cache.stats()
  assert.equal(s.providers, 2)
  assert.equal(s.entries, 2)
})

// --------------------------------------------------------------- Frecency

test("a launched row gets a score boost", () => {
  const store = {}
  Frecency.record(store, "b", Date.now(), "fir")
  const rows = [{ key: "a", score: 0 }, { key: "b", score: 0 }]
  Frecency.apply(rows, store, Date.now(), "fir")
  assert.ok(rows[1].score > rows[0].score)
})

test("the choosing query boosts an exact repeat", () => {
  const store = {}
  Frecency.record(store, "x", Date.now(), "doc")
  const qBoost = Frecency.boost(store, "x", Date.now(), "doc")
  const other = Frecency.boost(store, "x", Date.now(), "zzz")
  assert.ok(qBoost > other)
})

test("parse tolerates garbage", () => {
  assert.deepEqual(Frecency.parse("not json"), {})
  assert.deepEqual(Frecency.parse(""), {})
})

// --------------------------------------------------------------- Settings

test("merge fills defaults and keeps user values", () => {
  const cfg = Settings.merge('{"log": false}')
  assert.equal(cfg.log, false)
  assert.ok(Array.isArray(cfg.quicklinks))
  assert.equal(typeof cfg.defaultEngine, "string")
})
