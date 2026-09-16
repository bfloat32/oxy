import QtQuick
import Quickshell.Io
import qs.Commons
import "Query.js" as Query
import "Rank.js" as Rank
import "Extensions.js" as Extensions
import "Cache.js" as Cache
import "Availability.js" as Availability

// One instance per extension. It owns a Process, a debounce timer, and the
// epoch bookkeeping that keeps a slow answer from arriving after the question
// changed.
Item {
  id: prov

  property var launcher: null
  property var ext: null

  // A `when` condition is checked on load, not per keystroke: an extension for
  // software you do not have should cost nothing at all. But a machine changes
  // under a shell that stays up for days. Writing an ~/.ssh/config, logging in
  // with gh, starting a docker daemon: each of those makes a keyword real, and
  // the keyword stayed silent until the next restart, which reads as broken
  // rather than as unavailable.
  //
  // So a failed check is re-run when you actually type the keyword, at most
  // every fifteen seconds. A passing check is never re-run: software rarely goes
  // away mid-session, and the cost of being wrong there is one empty answer.
  //
  // Both of those rules now live in Availability.js rather than here, because
  // this object is destroyed and rebuilt on every launcher open and the answer
  // is not. See the header of that file.
  property bool available: true
  property bool checked: false

  property int inflightEpoch: -1
  property int pendingEpoch: -1
  property string pendingArg: ""
  property var pendingFilters: ({})

  // The question that is about to be asked, and the one that was asked. Both
  // are carried because the cache is keyed by the exact command, and by the
  // time an answer arrives the pending fields have already moved on.
  property string pendingCommand: ""
  property string pendingKey: ""
  property string inflightCommand: ""
  property string inflightKey: ""

  // The question whose answer is on screen right now. Background refresh asks
  // it again; anything else about the query changing invalidates it.
  property string liveCommand: ""
  property string liveKey: ""
  property int liveEpoch: -1

  // Set while the process running is a refresh rather than an answer to a
  // keystroke. A refresh is quieter in both directions: it never raises the
  // spinner, and it never blanks the rows it failed to replace.
  property bool refreshing: false

  // The cache key whose expired rows were drawn while the real answer is
  // fetched. A run that comes back with nothing while this matches its own
  // key failed rather than answered, so the stale rows stay.
  property string staleShownKey: ""

  // When the current run began, so its answer carries a duration into the log.
  property double inflightStart: 0
  property double availStart: 0

  readonly property string id: ext ? ext.id : ""

  // Every provider event carries its extension id, so the launcher-side helper
  // never has to remember it.
  function plog(ev, fields) {
    if (!prov.launcher) return
    var f = fields || {}
    f.id = prov.id
    prov.launcher.log(ev, f)
  }

  // A `when` that hangs — a probe waiting on the network, a command that
  // reads stdin — would hold the extension in "checking" for the life of the
  // shell. `timeout` turns a wedged probe into a plain "not available" after
  // eight seconds, which is the honest reading of a check that cannot finish.
  function whenArgv(check) {
    return ["timeout", "8s"].concat(
      prov.launcher ? prov.launcher.shellArgv(check) : ["bash", "-lc", check])
  }

  function claims(query) {
    if (!ext) return false
    return Query.routesTo(query, ext.keyword, ext.aliases)
  }

  function query(q) {
    // Whatever happens below, the answer on screen is about to be replaced, so
    // the timer that keeps re-asking the old question stops here. Only a path
    // that actually produces rows arms it again.
    refresher.stop()

    // And this provider is not waiting for anything until it decides otherwise
    // a few lines down. Every early return below is a decision not to run, and
    // each of them used to leave a flag set from the previous query: typing
    // `tz:` and then deleting it left the launcher spinning forever, because
    // the provider that raised the flag no longer claimed the query and never
    // reached the code that lowers it.
    if (prov.launcher) prov.launcher.markWaiting(prov.id, false)

    if (!ext) return emit(q, [])

    if (!prov.available) {
      // Only a query that names this keyword pays for a re-check, so an
      // unavailable extension still costs nothing while you type anything else.
      // Availability.get returning null is the library saying the last failure
      // is old enough to be worth asking about again.
      if (ext.when !== "" && claims(q) && !availability.running
          && Availability.get(ext.when, Date.now()) === null) {
        prov.recheckQuery = q
        availability.command = prov.whenArgv(ext.when)
        prov.availStart = Date.now()
        availability.running = true
      }
      return emit(q, [])
    }

    // Unscoped and not opted in: stay quiet. Shelling out to every extension on
    // every keystroke is how a launcher becomes slow enough to abandon.
    if (q.scope === "" && !ext.always) return emit(q, [])
    if (!claims(q)) return emit(q, [])

    var arg = Query.argFor(q, ext.keyword, ext.aliases)
    if (arg.length < ext.minChars) return emit(q, [])

    var filters = Query.extras(q, ext.keyword, ext.aliases)
    var command = ext.search === "" ? ""
      : Extensions.buildCommand(ext, arg, filters, Util.shellQuote,
                                prov.launcher ? prov.launcher.settingsFor(ext.id) : null)
    var key = Extensions.cacheKey(ext, command, arg, filters)

    // Recorded before the cache check, not after: a refresh over a socket sends
    // the question again rather than a command, and a cache hit that skipped
    // this would have it re-asking the question before last.
    prov.pendingArg = arg
    prov.pendingFilters = filters

    // A hit skips the debounce as well as the process. Waiting out a 250ms
    // debounce before redrawing something already in memory is not what
    // "instant" means, and the debounce is there to protect a shell-out that is
    // not going to happen.
    var hit = ext.cacheMs > 0 ? Cache.get(prov.id, key, Date.now()) : null
    if (hit) {
      debounce.stop()
      prov.pendingEpoch = -1
      prov.liveKey = key
      prov.liveCommand = command
      prov.liveEpoch = q.epoch
      emit(q, prov.build(hit))
      plog("prov.done", { ep: q.epoch, via: "cache", ms: 0, rows: hit.length })
      prov.armRefresh()
      return
    }

    // An expired answer is still worth drawing. It goes up now, with no
    // spinner and no skeleton, and the real answer replaces it in place when
    // the run below finishes: stable row keys are what make the swap invisible.
    // markWaiting is deliberately skipped — there is already an answer up.
    var stale = ext.cacheMs > 0 ? Cache.getStale(prov.id, key) : null
    // Which key's stale rows are on screen, so a failed run can tell an empty
    // answer that replaces them from an empty answer that replaces nothing.
    prov.staleShownKey = stale ? key : ""
    if (stale) {
      emit(q, prov.build(stale))
      plog("prov.done", { ep: q.epoch, via: "stale", ms: 0, rows: stale.length })
    }

    // Start connecting now rather than when the debounce fires, so the socket
    // is usually up by the time there is something to send down it.
    if (ext.socket !== "") prov.connect()

    if (prov.launcher && !stale) prov.launcher.markWaiting(prov.id, true)

    prov.pendingEpoch = q.epoch
    prov.pendingCommand = command
    prov.pendingKey = key
    debounce.interval = ext.debounceMs
    debounce.restart()
  }

  function cancel() {
    if (prov.launcher) prov.launcher.markWaiting(prov.id, false)
    prov.pendingEpoch = -1
    prov.refreshing = false
    // Not a cache invalidation: the point of the cache is that this survives
    // the launcher closing. Only the claim that these rows are on screen dies.
    prov.liveEpoch = -1
    debounce.stop()
    killer.stop()
    refresher.stop()
    if (process.running) process.running = false
  }

  // Set while a process run's stdout has not been fully delivered, and the
  // deferred restart a killed run leaves behind. The exit and the stream
  // finish have no guaranteed order; starting the next query before the old
  // stream's text arrived let that text be delivered under the NEW query's
  // epoch — a stale answer wearing the current question's name, and a busy
  // indicator that vanished while the new run was still going. Restarting only
  // after the collector has spoken closes the window in both orderings.
  property bool streamDone: true
  property bool restartAfterStream: false

  function emit(q, rows) {
    if (!prov.launcher) return
    prov.launcher.put(prov.id, q, rows)
  }

  function start() {
    // Whatever armed the debounce has now been asked, so the timer must not
    // fire again over the top of it.
    debounce.stop()
    if (prov.pendingEpoch < 0) return
    prov.inflightEpoch = prov.pendingEpoch
    prov.pendingEpoch = -1
    prov.refreshing = false
    prov.inflightCommand = prov.pendingCommand
    prov.inflightKey = prov.pendingKey
    prov.inflightStart = Date.now()
    plog("prov.start", { ep: prov.inflightEpoch,
      via: ext.socket !== "" && pipe.connected ? "sock" : "proc",
      cmd: prov.launcher ? prov.launcher.clip(prov.inflightCommand, 240)
        : prov.inflightCommand.slice(0, 240) })

    // Taken now, because a keystroke that arrives between the query and this
    // call has already overwritten them, and a socket extension would have been
    // sent one query's argument against another query's epoch.
    var arg = prov.pendingArg
    var filters = prov.pendingFilters

    if (prov.ask(prov.inflightEpoch, arg, filters)) return

    // A socket-only extension whose daemon is not listening answers nothing
    // rather than leaving the spinner up until the next keystroke — unless
    // this key's stale rows are already up, which are a better answer than
    // an empty one a dead daemon did not really give.
    if (prov.staleShownKey !== "" && prov.staleShownKey === prov.inflightKey) return
    if (prov.launcher) prov.launcher.putRaw(prov.id, prov.inflightEpoch, [])
  }

  // Send the question, by whichever route this extension has. A connected
  // socket wins over a command: the whole point of the socket is that the
  // program is already running and does not want to be started again. A socket
  // that is not up falls through to `search`, so an extension whose daemon died
  // degrades to the slow path instead of going silent.
  function ask(epoch, arg, filters) {
    if (ext.socket !== "" && pipe.connected) {
      pipe.write(JSON.stringify({ epoch: epoch, query: arg, filters: filters || {} }) + "\n")
      killer.interval = ext.timeoutMs
      killer.restart()
      return true
    }

    if (prov.inflightCommand === "") return false

    // The captured login environment rather than a login shell: same PATH and
    // the same profile exports, without sourcing the profile per keystroke.
    process.command = prov.launcher ? prov.launcher.shellArgv(prov.inflightCommand)
      : ["bash", "-lc", prov.inflightCommand]
    process.running = true
    // This run's stdout has not been delivered yet — the collector turns the
    // flag back on. Every launch path goes through here (start, the refresher),
    // so the flag is never stale about which stream is still owed.
    prov.streamDone = false
    killer.interval = ext.timeoutMs
    killer.restart()
    return true
  }

  // Parsed rows in, launcher rows out. Shared by the process, the cache and the
  // socket, so all three produce rows that are identical in every respect
  // including their keys, which is what lets a refresh replace rows under a
  // selection without moving it.
  function build(parsed) {
    var rows = []
    for (var i = 0; i < parsed.length && i < ext.maxRows; i++) {
      var row = Extensions.toRow(ext, parsed[i], i)
      if (row.title === "") continue
      row.score = Rank.score(row.tier, row.local, 0)
      row.run = (function (exec) {
        return function () { if (exec) Util.execDetached(exec) }
      })(row.exec)
      rows.push(row)
    }
    return rows
  }

  function finish(text) {
    killer.stop()
    if (!prov.launcher) return

    var wasRefresh = prov.refreshing
    prov.refreshing = false

    // The process is over, so this provider is not waiting for it any more,
    // whatever we decide to do with what it said. Clearing this only on the
    // path that keeps the answer meant a run whose epoch had moved on left the
    // flag raised for good: `stash:flows` typed one character at a time sat on
    // a skeleton forever, while the same query pasted in one go was fine.
    //
    // But a newer query may already be pending its turn: this finish belongs
    // to the run it came from, and clearing the flag now would drop the
    // spinner while that pending query has not even started yet.
    if (prov.pendingEpoch < 0) prov.launcher.markWaiting(prov.id, false)

    // Whatever else happened, the restart a killed run deferred to this
    // moment is owed now that the stream has spoken.
    if (prov.restartAfterStream) {
      prov.restartAfterStream = false
      if (prov.pendingEpoch >= 0) Qt.callLater(prov.start)
    }

    if (prov.inflightEpoch !== prov.launcher.epoch) {
      plog("prov.drop", { at: "finish", ep: prov.inflightEpoch,
        ms: Date.now() - prov.inflightStart })
      return
    }

    var parsed = Extensions.parseRows(text)

    // An empty answer that had something on screen is nearly always a timeout
    // or a transient failure, not the answer genuinely becoming "no rows":
    // that is true for a timed refresh and just as true for a keystroke that
    // was already showing this key's stale rows. Blanking either for it is
    // worse than one stale tick, so the rows stay and the cache keeps what it
    // had rather than recording the failure as the answer.
    if (parsed.length === 0
        && (wasRefresh
            || (prov.staleShownKey !== "" && prov.staleShownKey === prov.inflightKey))) {
      plog("prov.stale", { ep: prov.inflightEpoch, refresh: wasRefresh,
        ms: Date.now() - prov.inflightStart })
      return prov.armRefresh()
    }

    plog("prov.done", { ep: prov.inflightEpoch, via: "proc", refresh: wasRefresh,
      rows: parsed.length, ms: Date.now() - prov.inflightStart })
    prov.deliver(parsed)
  }

  // The one place an answer becomes rows on screen, so the cache write and the
  // refresh arming cannot be forgotten by one of the three callers.
  function deliver(parsed) {
    // A real answer is what retires the stale marker: only an empty one is
    // allowed to leave it in place, and that case never reaches this.
    prov.staleShownKey = ""
    // Trimmed before it is cached, not only when it is drawn. `maxRows` is the
    // only thing bounding how big one answer is, and a script that prints five
    // thousand rows had all five thousand held in the library cache for the
    // life of the shell while only the first `maxRows` were ever shown.
    if (parsed.length > ext.maxRows) parsed = parsed.slice(0, ext.maxRows)

    Cache.put(prov.id, prov.inflightKey, parsed, ext.cacheMs, Date.now())
    prov.liveKey = prov.inflightKey
    prov.liveCommand = prov.inflightCommand
    prov.liveEpoch = prov.inflightEpoch
    prov.launcher.putRaw(prov.id, prov.inflightEpoch, prov.build(parsed))
    prov.armRefresh()
  }

  // Are this extension's rows actually on screen? An extension that answered
  // with nothing, or whose rows lost to something that outranks them, has
  // nothing to refresh, and refreshing it anyway is a process a second spent on
  // rows nobody can see.
  function showing() {
    if (!prov.launcher) return false
    var rows = prov.launcher.rows
    for (var i = 0; i < rows.length; i++) {
      if (rows[i].providerId === prov.id) return true
    }
    return false
  }

  // Off unless the extension asked for it. Every condition here is checked
  // again when the timer fires, because all of them can change in between.
  function armRefresh() {
    if (!ext || ext.refreshMs <= 0) return
    if (prov.liveCommand === "" && ext.socket === "") return
    if (!prov.launcher || !prov.launcher.opened) return
    if (prov.liveEpoch !== prov.launcher.epoch) return
    if (!prov.showing()) return
    refresher.interval = ext.refreshMs
    refresher.restart()
  }

  // Connecting is driven by queries rather than by a retry timer: a daemon that
  // is not running should cost nothing until somebody types the keyword, and
  // retrying forever for an extension nobody uses is the per-keystroke cost
  // this route exists to remove.
  readonly property int reconnectMs: 3000
  property double lastConnect: 0

  function connect() {
    if (!ext || ext.socket === "") return
    if (pipe.connected) return
    if (Date.now() - prov.lastConnect < prov.reconnectMs) return
    prov.lastConnect = Date.now()
    pipe.connected = true
  }

  // One JSON line pushed by the daemon. It echoes the epoch it was asked under,
  // which is the only thing that makes a pushed answer safe: a program that
  // answers whenever it likes would otherwise repopulate the list two
  // keystrokes later, which is the exact failure the epoch exists to prevent.
  function onPushed(line) {
    if (!ext || !prov.launcher) return

    var text = String(line || "").trim()
    if (text === "") return

    var payload = null
    try {
      payload = JSON.parse(text)
    } catch (e) {
      // A daemon that writes garbage should not take the launcher down with it.
      plog("sock.bad", { head: prov.launcher ? prov.launcher.clip(text, 120) : text.slice(0, 120) })
      return
    }
    if (!payload || typeof payload !== "object") return
    if (!Array.isArray(payload.rows)) return

    var epoch = Number(payload.epoch)
    if (!isFinite(epoch) || epoch !== prov.launcher.epoch) {
      plog("prov.drop", { at: "push", got: isFinite(epoch) ? epoch : -1,
        ep: prov.launcher.epoch })
      return
    }

    killer.stop()

    var wasRefresh = prov.refreshing
    prov.refreshing = false
    if (wasRefresh && payload.rows.length === 0) {
      plog("prov.stale", { ep: epoch, refresh: true, via: "sock",
        ms: Date.now() - prov.inflightStart })
      return prov.armRefresh()
    }

    prov.inflightEpoch = epoch
    plog("prov.done", { ep: epoch, via: "sock", refresh: wasRefresh,
      rows: payload.rows.length, ms: Date.now() - prov.inflightStart })
    prov.deliver(payload.rows)
  }

  // The query that triggered a re-check, replayed if the check now passes so
  // the keystroke that made the extension available is also the one it answers.
  property var recheckQuery: null

  Component.onCompleted: {
    if (!ext) return
    if (ext.when === "") {
      prov.checked = true
      return
    }

    // Asked once per distinct check per session, not once per provider per
    // summon. Without this the launcher forked one login shell per extension
    // with a `when` every single time it opened.
    var known = Availability.get(ext.when, Date.now())
    if (known !== null) {
      prov.available = known
      prov.checked = true
      return
    }

    availability.command = prov.whenArgv(ext.when)
    prov.availStart = Date.now()
    availability.running = true
  }

  // The socket, the process and the timers all belong to this object, and this
  // object is destroyed on every launcher open. Closing the connection here
  // rather than leaving it to collection means the daemon on the other end
  // sees the hangup at a moment we chose.
  Component.onDestruction: {
    prov.cancel()
    if (pipe.connected) pipe.connected = false
  }

  Process {
    id: availability
    onExited: function (code) {
      prov.available = code === 0
      prov.checked = true
      Availability.put(ext ? ext.when : "", prov.available, Date.now())
      plog("avail", { ok: prov.available, recheck: prov.recheckQuery !== null,
        ms: Date.now() - prov.availStart })

      var replay = prov.recheckQuery
      prov.recheckQuery = null
      if (prov.available && replay && prov.launcher
          && replay.epoch === prov.launcher.epoch) prov.query(replay)
    }
  }

  Timer {
    id: debounce
    onTriggered: {
      // Nothing newer to ask. This timer is armed by every keystroke, but the
      // run it was armed for may already have been started from onExited, and
      // killing it here answered the query with zero bytes and nothing to
      // restart from. It looked random because it only bit when a script
      // finished fast enough to be restarted inside the debounce window, which
      // is exactly what the fast ones do.
      if (prov.pendingEpoch < 0) return

      // running = false sends SIGTERM; onExited restarts with the newer query.
      // Assigning command while running does not restart on its own.
      if (process.running) process.running = false
      else prov.start()
    }
  }

  // A script that never returns must not hold the slot forever.
  Timer {
    id: killer
    onTriggered: {
      plog("prov.timeout", { ep: prov.inflightEpoch, ms: ext ? ext.timeoutMs : 0,
        via: process.running ? "proc" : "sock", refresh: prov.refreshing })
      if (process.running) {
        process.running = false
        // finish() still runs, off the collector, and reads `refreshing` there
        // to decide whether a killed refresh is allowed to blank the rows. So
        // the flag is deliberately not cleared here.
        return
      }
      if (!ext || ext.socket === "") return
      // A socket has no process to kill, so an unanswered question has to be
      // closed out here or the spinner stays up until the next keystroke.
      if (prov.refreshing) {
        prov.refreshing = false
        prov.armRefresh()
        return
      }
      // An unanswered socket question over stale rows is a failure, not an
      // answer: the rows stay, the way a timed-out process would leave them.
      if (prov.staleShownKey === prov.inflightKey && prov.inflightKey !== "") return
      if (prov.launcher && prov.inflightEpoch === prov.launcher.epoch) {
        prov.launcher.putRaw(prov.id, prov.inflightEpoch, [])
      }
    }
  }

  // Re-ask the same question on an interval while its answer is on screen.
  // `spotify:` is why: a progress bar drawn once is wrong a second later, and
  // there is no way to know it moved without asking again.
  Timer {
    id: refresher
    repeat: true
    onTriggered: {
      if (!prov.launcher || !prov.launcher.opened) return refresher.stop()
      if (prov.liveEpoch !== prov.launcher.epoch) return refresher.stop()
      if (!prov.showing()) return refresher.stop()

      // A real query is queued or running. It will re-arm this when it lands,
      // and two processes for the same extension is exactly the pile-up the
      // debounce exists to prevent.
      if (prov.pendingEpoch >= 0 || process.running) return

      prov.refreshing = true
      prov.inflightEpoch = prov.liveEpoch
      prov.inflightCommand = prov.liveCommand
      prov.inflightKey = prov.liveKey

      // No markWaiting. A spinner appearing every second over an answer that is
      // already on screen is the flicker this whole feature is meant to avoid.
      if (!prov.ask(prov.liveEpoch, prov.pendingArg, prov.pendingFilters)) {
        prov.refreshing = false
        refresher.stop()
      }
    }
  }

  Process {
    id: process
    stdout: StdioCollector {
      waitForEnd: true
      // The flag first, then the payload: finish() may schedule the restart
      // that was waiting on this moment, and that restart must see a stream
      // that has already spoken.
      onStreamFinished: {
        prov.streamDone = true
        prov.finish(text)
      }
    }
    // onExited and onStreamFinished have no guaranteed order, so the payload is
    // read above and this only ever restarts.
    onExited: function (code) {
      // A nonzero exit whose answer arrives anyway is not a failure worth a
      // line — scripts that warn on stderr exit fine. One that exits badly AND
      // says nothing is the case that used to pass for "no results".
      if (code !== 0) plog("prov.fail", { ep: prov.inflightEpoch, code: code,
        ms: Date.now() - prov.inflightStart })
      if (prov.pendingEpoch >= 0) {
        // Only restart once the old stream has delivered. Starting before it
        // did would overwrite inflightEpoch, and the old text would then pass
        // the epoch check in finish() as if it answered the new question.
        if (prov.streamDone) Qt.callLater(prov.start)
        else prov.restartAfterStream = true
      }
    }
  }

  // A long-running program answers over a unix socket instead of being started
  // per keystroke. One JSON line goes out per query, one or more come back, and
  // the connection outlives the launcher being closed so the next open costs
  // nothing to reconnect.
  Socket {
    id: pipe
    path: ext ? ext.socket : ""
    parser: SplitParser {
      splitMarker: "\n"
      onRead: function (line) { prov.onPushed(line) }
    }
  }
}
