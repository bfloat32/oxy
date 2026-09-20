import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import Quickshell.Hyprland
import QtQuick
import qs.Commons
import qs.Ui
import "Settings.js" as Settings
import "Accent.js" as Accent

// Oxy, thin edition: the same card and the same views as Launcher.qml, with
// every decision — what to ask, when to ask it, which answers are stale, what
// Enter does — made by oxyd over a JSON-lines socket. This file draws what it
// is sent and sends what is typed; it owns the look and the keystrokes, and
// nothing else.
//
// The shell injects `shell`, `manifest` and `omarchyPath` by name, calls
// open(payloadJson) and close(), and reads `opened`. `keepLoaded: true` in the
// manifest keeps this instance alive between summons, so close() has to reset
// state; nothing here is constructed fresh.
Item {
  id: root

  property string omarchyPath: Quickshell.env("OMARCHY_PATH")
  property var shell: null
  property var manifest: null

  property bool opened: false
  property double openedAt: 0
  property string queryText: ""
  property int selectedIndex: 0
  property bool cursorMoved: false
  property string selectedKey: ""

  property var rows: []

  // Provider ids still owed an answer — arrives on every results event and is
  // kept as a map because lookup is all this side ever does with it.
  property var waiting: ({})

  readonly property bool busy: {
    for (var key in root.waiting) {
      if (root.waiting[key]) return true
    }
    return false
  }

  // Busy for long enough that saying so is information rather than noise. A
  // word takes about 350ms to type and the debounce only starts counting when
  // typing stops, so a shorter threshold drew a skeleton over an answer that
  // was 130ms away.
  property bool slowBusy: false
  onBusyChanged: {
    if (root.busy) slowTimer.restart()
    else { slowTimer.stop(); root.slowBusy = false }
  }
  Timer {
    id: slowTimer
    interval: 700
    onTriggered: root.slowBusy = root.busy
  }

  // The query trail behind a flow — kept here rather than in the engine
  // because it is about what the user is looking at, not what was asked.
  property var flowStack: []
  // True while this file is writing the box itself: typing abandons a flow,
  // a follow-up query writing the same box does not.
  property bool navigating: false
  // True while open() writes the box before its `open` op has gone out — the
  // engine already heard the text, so a query for it would run twice.
  property bool suppressSend: false

  // Which layout the current results want. The daemon says what the first row
  // asked for; "answer" and "loading" are this side's own, because they are
  // about drawing rather than about results.
  property string wireView: "list"
  property string scopeLabel: ""
  property bool helpMode: false
  property bool recentMode: false
  // The armed confirm prompt, while an action has one armed.
  property string pendingConfirm: ""
  property bool actionPanelOpen: false

  // Ctrl+Enter: ask a model, stream the answer here. The daemon owns the
  // process; this owns the card.
  property bool answerMode: false
  property bool answerAvailable: false
  property bool answerStreaming: false
  property string answerText: ""
  property string answerQuestion: ""
  property string answerError: ""
  property string answerModel: ""

  // Say what happened without closing. A notice with nothing to show would
  // otherwise look like a key that did nothing at all. The same query is
  // re-asked on the poll cadence, because what just ran — a copied value, a
  // track that started — can take a moment to show in the answer.
  property string notice: ""
  function notify(text) {
    root.notice = String(text)
    noticeTimer.restart()
    pollText.text = root.queryText
    pollText.round = 0
    pollTimer.restart()
  }
  Timer {
    id: noticeTimer
    interval: 2400
    onTriggered: root.notice = ""
  }

  // The host's library when it gives one — kept for the views that read
  // launcher.appLibrary; the daemon's `apps` provider does not need it.
  readonly property var hostAppLibrary: root.shell ? root.shell.appLibrary : null
  readonly property var appLibrary: root.hostAppLibrary ? root.hostAppLibrary : fallbackApps.item
  Loader {
    id: fallbackApps
    active: !root.hostAppLibrary
    sourceComponent: AppLibraryFallback { omarchyPath: root.omarchyPath }
  }

  readonly property string configHome: Quickshell.env("XDG_CONFIG_HOME") || Quickshell.env("HOME") + "/.config"
  readonly property string stateHome: Quickshell.env("XDG_STATE_HOME") || Quickshell.env("HOME") + "/.local/state"

  // The visual settings only — cardWidth, maxRows, resetOnOpen. Everything
  // else in oxy.json is the daemon's to read.
  property var config: Settings.DEFAULTS

  FileView {
    id: configFile
    path: root.configHome + "/omarchy/oxy.json"
    watchChanges: true
    printErrors: false
    onLoaded: root.config = Settings.merge(text())
    onFileChanged: reload()
    onLoadFailed: root.config = Settings.DEFAULTS
  }

  // ------------------------------------------------------------ the daemon

  // `$XDG_RUNTIME_DIR/oxyd.sock`, falling back to the state dir — the same
  // name dirs::socket_name() picks, so a daemon started by hand and one the
  // launcher spawned answer on the same address.
  readonly property string socketPath: {
    var rt = Quickshell.env("XDG_RUNTIME_DIR")
    if (rt && rt !== "") return rt + "/oxyd.sock"
    return root.stateHome + "/omarchy/oxyd.sock"
  }

  property string oxydBin: Quickshell.env("OXYD") || "oxyd"
  property bool spawned: false

  Socket {
    id: daemon
    path: root.socketPath
    parser: SplitParser {
      splitMarker: "\n"
      onRead: function (line) { root.onEvent(line) }
    }
  }

  // Send one command. When the daemon is not there yet the newest open/query
  // is kept and sent on connect; everything else is safe to drop because it
  // answers something that is no longer on screen.
  property string pendingOpen: ""
  property string pendingQuery: ""

  function send(obj) {
    var line = JSON.stringify(obj) + "\n"
    if (daemon.connected) {
      daemon.write(line)
      return
    }
    if (obj.op === "open") root.pendingOpen = line
    else if (obj.op === "query") root.pendingQuery = line
    ensureDaemon()
  }

  Connections {
    target: daemon
    function onConnectedChanged() {
      if (!daemon.connected) return
      root.spawned = false
      if (root.pendingOpen !== "") {
        daemon.write(root.pendingOpen)
        root.pendingOpen = ""
        root.pendingQuery = ""
      } else if (root.pendingQuery !== "") {
        daemon.write(root.pendingQuery)
        root.pendingQuery = ""
      }
      // A connected daemon owes this client the registry; opening again is
      // how it is asked for without a second protocol verb.
      if (root.opened) root.send({ op: "open", text: root.queryText })
    }
  }

  // Down is a state the launcher has to climb out of itself: retry the socket
  // on a beat, and spawn the daemon once per outage and again on a slow beat —
  // a missing binary should cost a failed spawn every twelve seconds, not one
  // per retry and not never.
  Timer {
    id: reconnectTimer
    interval: 800
    repeat: true
    running: true
    property int misses: 0
    onTriggered: {
      if (daemon.connected) { misses = 0; return }
      misses += 1
      daemon.connected = false
      daemon.connected = true
      if (misses === 2) root.ensureDaemon()
      else if (misses % 15 === 0) { root.spawned = false; root.ensureDaemon() }
    }
  }

  function ensureDaemon() {
    if (root.spawned || daemon.connected) return
    root.spawned = true
    Util.execDetached(root.oxydBin)
  }

  // A log line, fire and forget — the daemon owns the file.
  function log(ev, fields) {
    if (daemon.connected) {
      daemon.write(JSON.stringify({ op: "log", ev: ev, fields: fields || {} }) + "\n")
    }
  }
  function clip(value, max) {
    var text = String(value === undefined ? "" : value)
    return text.length > max ? text.slice(0, max) + "…" : text
  }

  // One JSON line pushed by the daemon. A daemon that writes garbage should
  // not take the launcher down with it, so parse failures cost the line and
  // nothing more.
  function onEvent(line) {
    var text = String(line || "").trim()
    if (text === "") return
    var ev = null
    try { ev = JSON.parse(text) } catch (e) { return }
    if (!ev || typeof ev !== "object") return

    switch (String(ev.op || "")) {
    case "results": return root.applyResults(ev)
    case "type": return root.onTyped(String(ev.text || ""), ev.flow === true, ev.poll === true)
    case "notice": return root.notify(String(ev.text || ""))
    case "registry":
      root.extensions = Array.isArray(ev.extensions) ? ev.extensions : []
      var ask = ev.ask || {}
      root.answerAvailable = ask.available === true
      root.answerModel = String(ask.model || "")
      return
    case "close": return root.dismiss()
    case "answerstart":
      root.answerMode = true
      root.answerStreaming = true
      root.answerText = ""
      root.answerError = ""
      root.answerQuestion = String(ev.question || "")
      root.answerModel = String(ev.provider || root.answerModel)
      return
    case "answer":
      root.answerText += (root.answerText === "" ? "" : "\n") + String(ev.line || "")
      return
    case "answerdone":
      root.answerStreaming = false
      var err = String(ev.error || "")
      if (err !== "") root.answerError = err
      return
    }
  }

  // The extension list the last registry event carried — read for the source
  // chips and nothing else.
  property var extensions: []

  // providerId -> what a person would call it, and the colour it asked for.
  readonly property var sources: {
    var out = {
      apps: { title: "Applications", accent: "" },
      commands: { title: "Commands", accent: "" },
      quicklinks: { title: "Quicklinks", accent: "" },
      calc: { title: "Calculator", accent: "" },
      web: { title: "Web", accent: "" },
      paste: { title: "Clipboard", accent: "" },
      settings: { title: "Settings", accent: "" }
    }
    for (var i = 0; i < root.extensions.length; i++) {
      var ext = root.extensions[i]
      out[ext.id] = { title: ext.title, accent: String(ext.accent || "") }
    }
    return out
  }

  // A declared accent, walked to something legible on this card, cached per
  // provider: the contrast maths inside a delegate binding would repeat for
  // every row on every rebuild.
  property var accentCache: ({})
  onBackgroundChanged: root.accentCache = ({})
  onSourcesChanged: root.accentCache = ({})

  function accentFor(providerId) {
    var cached = root.accentCache[providerId]
    if (cached !== undefined) return cached

    var spec = root.sources[providerId]
    var declared = spec ? Accent.parse(spec.accent) : null
    var safe = Accent.readable(declared,
                               { r: root.background.r, g: root.background.g, b: root.background.b },
                               { r: Color.accent.r, g: Color.accent.g, b: Color.accent.b })

    var out = Qt.rgba(safe.r, safe.g, safe.b, 1)
    var next = root.accentCache
    next[providerId] = out
    root.accentCache = next
    return out
  }

  // Naming the source on every row is only worth the ink when there is more
  // than one place it could have come from.
  function tagSources(list) {
    var seen = {}
    var count = 0
    for (var i = 0; i < list.length; i++) {
      var id = String(list[i].providerId || "")
      if (seen[id]) continue
      seen[id] = true
      count += 1
    }

    for (var j = 0; j < list.length; j++) {
      var row = list[j]
      var spec = root.sources[String(row.providerId || "")]
      var name = spec ? spec.title : ""
      // Left undefined rather than defaulted, so a view can tell "this source
      // asked for green" from "this source asked for nothing".
      row.accent = (spec && spec.accent !== "")
        ? root.accentFor(String(row.providerId || "")) : undefined
      // A fill row is a keyword or a past query: nothing to attribute it to.
      row.source = (count > 1 && name !== "" && row.fill === undefined) ? name : ""
    }
    return list
  }

  // ------------------------------------------------------------ results in

  function applyResults(ev) {
    root.wireView = String(ev.view || "list")
    root.scopeLabel = String(ev.scopeLabel || "")
    root.helpMode = ev.helpMode === true
    root.recentMode = ev.recentMode === true
    root.pendingConfirm = String(ev.confirm || "")

    var wait = {}
    var waitingList = ev.waiting || []
    for (var i = 0; i < waitingList.length; i++) wait[String(waitingList[i])] = true
    root.waiting = wait

    var merged = ev.rows || []
    root.tagSources(merged)
    root.rows = merged

    // Selection follows the key it was on; a fresh answer resets it to the
    // top unless the cursor was deliberately moved.
    if (!root.cursorMoved) {
      root.selectedIndex = 0
    } else {
      var at = -1
      for (var r = 0; r < merged.length; r++) {
        if (merged[r].key === root.selectedKey) { at = r; break }
      }
      root.selectedIndex = at >= 0 ? at : 0
    }
    root.selectedKey = merged.length > 0 ? String(merged[root.selectedIndex].key) : ""
  }

  onRowsChanged: {
    root.measureChips()
    // Rows landing with the selection already where it was change nothing
    // about selectedIndex, so the preview hook below never fired for them.
    root.scheduleSelect()
  }

  // The daemon runs a row's previewExec; the 90ms wait is kept here because
  // it is about how the keys feel, not about what runs — holding Down should
  // not fire a preview for every row it crosses.
  Timer {
    id: selectTimer
    interval: 90
    onTriggered: {
      var row = root.rows[root.selectedIndex]
      if (row && row.previewExec) root.send({ op: "select", key: String(row.key) })
    }
  }
  function scheduleSelect() {
    var row = root.rows[root.selectedIndex]
    if (row && row.previewExec) selectTimer.restart()
  }
  onSelectedIndexChanged: root.scheduleSelect()

  // One chip per row, and only one: the source when the answer came from more
  // than one place, and otherwise whatever the row called itself.
  function chipFor(row) {
    return String((row && (row.source || row.accessory)) || "")
  }

  // How wide that column is: the widest chip in the current answer, measured
  // rather than fixed, so every chip shares a left edge and a row without one
  // leaves the slot blank rather than letting its neighbours slide across.
  property int chipColumn: 0

  TextMetrics {
    id: chipMetrics
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    font.letterSpacing: 0.2
  }

  function measureChips() {
    var widest = 0
    for (var i = 0; i < root.rows.length; i++) {
      var label = root.chipFor(root.rows[i])
      if (label === "") continue
      chipMetrics.text = label
      widest = Math.max(widest, chipMetrics.width)
    }
    root.chipColumn = widest > 0 ? Math.ceil(widest) + Style.space(16) : 0
  }

  readonly property string activeView: {
    if (root.answerMode) return "answer"
    if (root.rows.length === 0 && root.slowBusy && root.queryText.trim() !== "") return "loading"
    if (root.rows.length === 0) return "list"
    return root.knownViews.indexOf(root.wireView) >= 0 ? root.wireView : "list"
  }

  readonly property var knownViews: ["list", "hero", "cards", "split", "grid",
    "dashboard", "calendar", "player", "slider", "form", "timegrid", "zones",
    "gitrepo", "gitbranches", "gitstashes", "agent",
    "ghrepo", "ghpr", "docker", "notes", "processes", "emoji", "themes",
    "windows", "hosts", "radios", "radioplayer", "files", "repos",
    "menutree", "snippets", "vault", "shortcuts", "herdr", "marketplace", "marketplacehome", "marketplaceunit", "answer", "loading"]

  // The [menu] surface tokens, so a theme that styles the Omarchy menu styles
  // this too, with no extra work from the user.
  readonly property color background: Color.menu.background
  readonly property color foreground: Color.menu.text
  readonly property color scrim: Color.menu.scrim
  readonly property color selectedBackground: Color.menu.selectedBackground
  readonly property color selectedText: Color.menu.selectedText
  readonly property var borderSpec: Border.surfaceSpec("menu", "border", Color.menu.border, Math.max(1, Style.space(2)))
  readonly property string fontFamily: Style.font.menuFamily

  readonly property int cardWidth: Math.min(Style.space(Number(root.config.cardWidth) || 620), panel.width - Style.gapsOut * 2)
  readonly property int maxRows: Number(root.config.maxRows) || 9

  // ------------------------------------------------------------ lifecycle

  // `payloadJson` is how something outside asks for a particular screen rather
  // than the opening one: `omarchy-shell shell summon oma.oxy '{"query":"bo:"}'`.
  function open(payloadJson) {
    var wanted = ""
    if (payloadJson) {
      try {
        var payload = typeof payloadJson === "string" ? JSON.parse(payloadJson) : payloadJson
        if (payload && payload.query !== undefined) wanted = String(payload.query)
      } catch (e) {
        // A malformed payload opens the launcher empty rather than not at all.
      }
    }

    pinScreen()
    root.openedAt = Date.now()
    root.opened = true
    root.log("open", { q: root.clip(wanted, 120) })
    root.flowStack = []
    root.actionPanelOpen = false
    root.leaveAnswer()
    resetSelection()

    // The `open` op carries the query, so the box is written with sending
    // suppressed: assigning input.text fires onTextChanged either way, and
    // without this every summon asked the engine twice.
    if (wanted === "" && root.config.resetOnOpen === false) wanted = root.queryText
    root.queryText = wanted
    root.suppressSend = true
    input.text = wanted
    root.suppressSend = false

    root.send({ op: "open", text: wanted })
    if (root.appLibrary) root.appLibrary.refreshIcons()
    Qt.callLater(function () {
      input.forceActiveFocus()
      input.selectAll()
    })
  }

  function close() {
    if (root.opened) {
      root.log("close", { ms: Date.now() - root.openedAt })
      root.send({ op: "close" })
    }
    root.opened = false
    // The daemon keeps the caches; what is cleared here is what a summon
    // should not see: last visit's rows, the trail, the answer card.
    root.rows = []
    root.waiting = ({})
    root.flowStack = []
    root.actionPanelOpen = false
    pollTimer.stop()
    selectTimer.stop()
    root.leaveAnswer()
  }

  function toggle() {
    if (root.opened) dismiss()
    else open("")
  }

  // Tell the shell, do not just hide. It tracks open panels in its own set,
  // and a close that skips this leaves the entry stale.
  function dismiss() {
    close()
    if (root.shell && typeof root.shell.hide === "function") {
      root.shell.hide((root.manifest && root.manifest.id) || "oma.oxy")
    }
  }

  // A bare PanelWindow binds to Quickshell.screens[0], not the focused output.
  // Pin before `opened` goes true: reassigning `screen` on a mapped layer
  // surface recreates it and drops the keyboard grab.
  function pinScreen() {
    var monitor = Hyprland.focusedMonitor
    if (!monitor) return

    var screens = Quickshell.screens
    for (var i = 0; i < screens.length; i++) {
      if (String(screens[i].name) === String(monitor.name)) {
        panel.screen = screens[i]
        return
      }
    }
  }

  // ------------------------------------------------------------ flows

  function pushFlow(from) {
    var text = String(from || "")
    var stack = root.flowStack
    if (stack.length > 0 && String(stack[stack.length - 1]) === text) return

    var next = stack.slice()
    next.push(text)
    // Deeper than anyone walks on purpose, shallow enough that a follow-up
    // landing on itself cannot grow the trail forever.
    if (next.length > 8) next.shift()
    root.flowStack = next
  }

  function flowBack() {
    if (root.flowStack.length === 0) return
    var next = root.flowStack.slice()
    var back = String(next.pop())
    root.flowStack = next
    goTo(back)
  }

  // Land on another query at once, without the box reading it as typing.
  function goTo(text) {
    root.navigating = true
    resetSelection()
    setInput(text)
    root.navigating = false
  }

  // The daemon said to put text in the box. `flow` marks a step deeper in,
  // which pushes where the user was onto the trail — unless the step lands
  // back on the trail's top, which is leaving, not going further. The push is
  // now, not when the text lands: the trail records where the action fired
  // from, and a poll can delay the landing by most of a second.
  //
  // `poll` marks a follow-up whose effect lands later — the old
  // followUpTimer's job — so the text is written on its cadence and the query
  // re-asked until the answer catches up.
  function onTyped(text, flow, poll) {
    if (flow && text !== "" && text !== root.queryText) {
      var stack = root.flowStack
      if (stack.length > 0 && String(stack[stack.length - 1]) === text) {
        root.flowStack = stack.slice(0, stack.length - 1)
      } else {
        root.pushFlow(root.queryText)
      }
    }
    if (poll) {
      pollText.text = text
      pollText.round = 0
      pollTimer.restart()
      return
    }
    goTo(text)
  }

  QtObject {
    id: pollText
    property string text: ""
    property int round: 0
  }

  Timer {
    id: pollTimer
    interval: 900
    repeat: true
    onTriggered: {
      pollText.round += 1
      if (pollText.round === 1) {
        root.goTo(pollText.text)
        Qt.callLater(function () { input.forceActiveFocus() })
      } else {
        root.refresh()
      }
      // Four passes over roughly four seconds covers the slowest effect seen:
      // a lookup plus the player it started actually reporting a track.
      if (pollText.round >= 4) pollTimer.stop()
    }
  }

  function refresh() {
    root.send({ op: "query", text: root.queryText, opened: true })
  }

  // ------------------------------------------------------------ navigation

  // How far one press of Up or Down travels. In a grid that is a whole row,
  // because moving one cell at a time down a wall of thumbnails is maddening.
  readonly property int verticalStep: {
    if (root.activeView === "grid") {
      return Math.max(1, Math.floor((root.cardWidth - Style.space(24)) / Style.space(168)))
    }
    if (root.activeView === "dashboard") {
      return Math.max(1, Math.floor((root.cardWidth - Style.space(24)) / Style.space(190)))
    }
    if (root.activeView === "docker") {
      return Math.max(1, Math.floor((root.cardWidth - Style.space(12)) / Style.space(280)))
    }
    if (root.activeView === "emoji" || root.activeView === "themes" || root.activeView === "marketplace") {
      return (resultsArea.item && resultsArea.item.columns)
        ? Math.max(1, resultsArea.item.columns) : 1
    }
    return 1
  }

  // Move the cursor without activating. A calendar tab needs this: clicking
  // "Next" should change which month is drawn, not copy it and close.
  function select(index) {
    if (index < 0 || index >= root.rows.length) return
    root.cursorMoved = true
    root.selectedIndex = index
    root.selectedKey = String(root.rows[index].key)
  }

  function move(delta) {
    if (root.rows.length === 0) return
    root.cursorMoved = true
    // Some views draw fewer rows than they are handed without scrolling to
    // the rest; selection must stop where the drawing does.
    var limit = root.rows.length - 1
    if (resultsArea.item
        && resultsArea.item.selectableCount !== undefined
        && resultsArea.item.selectableCount >= 0) {
      limit = Math.min(limit, resultsArea.item.selectableCount - 1)
    }
    root.selectedIndex = Math.max(0, Math.min(limit, root.selectedIndex + delta))
    root.selectedKey = String(root.rows[root.selectedIndex].key)
  }

  function resetSelection() {
    root.selectedIndex = 0
    root.cursorMoved = false
    root.selectedKey = ""
  }

  // ------------------------------------------------------------ acting

  // What Enter runs. An empty key is legal: it asks the engine for whatever
  // leads once the list catches up, which is what typing `1+1` and hitting
  // Enter before qalc answers should do. Shift picks the second action —
  // "ask ChatGPT" instead of Google — resolved engine-side.
  function activateSelected(shift) {
    var row = root.rows.length > 0 ? root.rows[root.selectedIndex] : null
    root.send({
      op: "activate",
      key: row ? String(row.key || "") : "",
      shift: shift === true
    })
  }

  function activate(row, shift, ctrl) {
    root.send({
      op: "activate",
      key: row ? String(row.key || "") : "",
      shift: shift === true,
      ctrl: ctrl === true
    })
  }

  // Every row's actions. The first is the primary and already runs on Enter;
  // the panel exists for the rest.
  function currentActions() {
    var row = root.rows[root.selectedIndex]
    if (!row) return []

    var list = []
    if (row.actions) {
      for (var i = 0; i < row.actions.length; i++) list.push(row.actions[i])
    }

    // A row with a primary but no declared list still deserves one entry, so
    // Ctrl+K never opens an empty panel on a working result.
    if (list.length === 0 && (row.exec || row.fill !== undefined)) {
      list.push({ title: String(row.accessory || "Open"), shortcut: "↵", row: row })
    }
    return list
  }

  // `row` is the row the action belongs to; the panel and Enter leave it out
  // because theirs is always the selected one, while a form's submit names
  // its own.
  function runAction(action, row) {
    if (!action) return
    root.actionPanelOpen = false

    var owner = row || root.rows[root.selectedIndex]
    if (!owner) return

    // A self-reference: activate the row it names. Older frontends carried
    // the row object here; the wire carries its key.
    if (action.row !== undefined) {
      if (typeof action.row === "object") return root.activate(action.row)
      return root.send({ op: "activate", key: String(action.row) })
    }

    // A declared action is sent by index — the engine knows what it means and
    // the object does not have to survive a round trip to be recognised.
    if (owner.actions) {
      var idx = owner.actions.indexOf(action)
      if (idx < 0) {
        for (var i = 0; i < owner.actions.length; i++) {
          var a = owner.actions[i]
          if (a.title === action.title && a.exec === action.exec
              && a.id === action.id && a.query === action.query) { idx = i; break }
        }
      }
      if (idx >= 0) {
        return root.send({ op: "activate", key: String(owner.key), action: idx })
      }
    }

    // Not one of the row's declared actions — a form's synthesized submit —
    // so it travels whole.
    root.send({ op: "act", key: String(owner.key), action: action })
  }

  // The engine's settings form answers to this rather than to an exec: the
  // file write is the daemon's business and never crosses the frontend.
  function saveSettings(id, values) {
    root.send({ op: "savesettings", id: String(id), values: values })
  }

  // Ctrl+P pins the selected row so it leads every query it matches. The
  // daemon owns the pin; the cursor-follow stays here because it is about
  // where the eye is, not about the file.
  function togglePin() {
    var row = root.rows[root.selectedIndex]
    if (!row || !row.key) return
    if (row.fill !== undefined) return
    if (row.providerId === "calc" || row.providerId === "web") return

    root.send({ op: "pin", key: String(row.key) })
    root.cursorMoved = true
    root.selectedKey = String(row.key)
  }

  // Ctrl+C with no text selection copies the row itself — `copyText` where a
  // script says what it is worth as text, the detail a hero is already
  // showing, the title elsewhere.
  function copySelected() {
    var row = root.rows[root.selectedIndex]
    if (!row) return
    root.activate(row, false, true)
  }

  // ------------------------------------------------------------ asking

  function ask(question) {
    if (!root.answerAvailable || !question) return
    root.send({ op: "ask", text: String(question) })
  }

  function leaveAnswer() {
    if (root.answerStreaming) root.send({ op: "stopask" })
    root.answerMode = false
    root.answerStreaming = false
    root.answerText = ""
    root.answerError = ""
    root.answerQuestion = ""
  }

  // A view that took the keyboard is giving it back.
  function focusInput() {
    Qt.callLater(function () { input.forceActiveFocus() })
  }

  // Escape, pressed inside a view that owns the keyboard: the same ladder the
  // box's own Escape climbs.
  function escapeFrom() {
    if (root.answerMode) root.leaveAnswer()
    else if (root.flowStack.length > 0) root.flowBack()
    else if (root.queryText.length > 0) root.setInput("")
    else root.dismiss()
  }

  // Put text in the box and search it, without closing.
  function setInput(text) {
    input.text = text
    input.cursorPosition = input.text.length
    Qt.callLater(function () {
      // A view that took the keyboard keeps it: landing on a form schedules
      // the form's own focus, and the box's write was queued second, so
      // without this the box's would win.
      if (root.activeView === "form") return
      input.forceActiveFocus()
    })
  }

  Component.onCompleted: {
    root.log("sess", { v: String(root.manifest && root.manifest.version || "?") })
    daemon.connected = true
  }

  // ------------------------------------------------------------ window

  PanelWindow {
    id: panel
    visible: root.opened
    anchors { top: true; bottom: true; left: true; right: true }
    color: "transparent"
    WlrLayershell.namespace: "oxy"
    WlrLayershell.layer: WlrLayer.Overlay
    WlrLayershell.keyboardFocus: WlrKeyboardFocus.Exclusive
    exclusionMode: ExclusionMode.Ignore

    Rectangle {
      anchors.fill: parent
      color: root.scrim
    }

    MouseArea {
      anchors.fill: parent
      onClicked: root.dismiss()
    }

    // Outside the card, not inside it. The card clips, so that views cannot
    // spill over its rounded border, and this hangs below the card on purpose.
    ActionPanel {
      id: actionPanel
      launcher: root
      // Faded rather than switched, the way Omarchy's own panels close.
      readonly property bool shown: root.actionPanelOpen && root.rows.length > 0
      opacity: shown ? 1 : 0
      visible: opacity > 0.01

      Behavior on opacity {
        NumberAnimation { duration: 140; easing.type: Easing.OutCubic }
      }
      x: card.x + card.width - width - Style.space(12)
      y: card.y + card.height + Style.space(6)
      maxHeight: panel.height - (card.y + card.height + Style.space(6)) - Style.space(12)
      z: 10
    }

    BorderSurface {
      id: card
      width: root.cardWidth
      radius: Style.cornerRadius
      color: root.background
      borderSpec: root.borderSpec
      padding: 0

      anchors.horizontalCenter: parent.horizontalCenter
      y: Math.round(parent.height * 0.18)

      // Nothing else stops a view drawing past this border.
      clip: true

      height: header.height
        + ((root.rows.length > 0 || root.answerMode || root.activeView === "loading")
           ? resultsArea.height + footer.height + Style.space(14) : 0)
        + emptyState.height

      Behavior on height {
        NumberAnimation { duration: 90; easing.type: Easing.OutCubic }
      }

      // Swallow clicks so they do not reach the dismissing MouseArea behind.
      MouseArea { anchors.fill: parent; onClicked: {} }

      Item {
        id: header
        anchors.top: parent.top
        anchors.left: parent.left
        anchors.right: parent.right
        height: Style.space(58)

        Text {
          id: prompt
          anchors.left: parent.left
          anchors.leftMargin: Style.space(18)
          anchors.verticalCenter: parent.verticalCenter
          text: ""
          color: Qt.darker(root.foreground, 1.6)
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
        }

        // A chip for the active filter, so `file:` reads as a mode you are in
        // rather than as four characters to re-read in the input.
        Chip {
          id: scopeChip
          anchors.left: prompt.right
          anchors.leftMargin: Style.space(12)
          anchors.verticalCenter: parent.verticalCenter
          accented: true
          text: root.scopeLabel
          foreground: root.foreground
          fontFamily: root.fontFamily
        }

        TextInput {
          id: input
          anchors.left: scopeChip.visible ? scopeChip.right : prompt.right
          anchors.leftMargin: Style.space(12)
          anchors.right: parent.right
          anchors.rightMargin: Style.space(20)
          anchors.verticalCenter: parent.verticalCenter

          color: root.foreground
          selectionColor: Style.selectionFillFor(root.foreground, Color.accent)
          selectedTextColor: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.title
          clip: true
          focus: true

          onTextChanged: {
            if (root.answerMode) root.leaveAnswer()
            // Typing is leaving the flow, not walking back through it. Only
            // the launcher's own writes to the box keep the trail alive.
            if (!root.navigating) root.flowStack = []
            root.resetSelection()
            root.queryText = text
            if (!root.suppressSend) {
              root.send({ op: "query", text: text, opened: true })
            }
          }

          // BeforeItem so navigation keys never reach the editor, and
          // everything else does.
          Keys.priority: Keys.BeforeItem
          Keys.onPressed: function (event) {
            if (event.key === Qt.Key_K && (event.modifiers & Qt.ControlModifier)) {
              if (root.currentActions().length > 0) root.actionPanelOpen = !root.actionPanelOpen
              event.accepted = true
            } else if (root.actionPanelOpen) {
              // While the panel is up it owns navigation: Up and Down move
              // between actions rather than between results underneath it.
              if (event.key === Qt.Key_Escape) root.actionPanelOpen = false
              else if (event.key === Qt.Key_Down) actionPanel.move(1)
              else if (event.key === Qt.Key_Up) actionPanel.move(-1)
              else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) actionPanel.activate()
              else return
              event.accepted = true
            } else if (event.key === Qt.Key_C && (event.modifiers & Qt.ControlModifier)) {
              // With a selection this is the editor's own copy and stays it.
              if (input.selectedText === "") {
                root.copySelected()
                event.accepted = true
              }
            } else if (event.key === Qt.Key_Escape) {
              // A row may own Escape before any of this: `escExec` is how a
              // row says it is still running and has to be told to stop.
              var owner = root.rows[root.selectedIndex]
              if (owner && owner.escExec) Util.execDetached(String(owner.escExec))

              // Four stages: leave the answer, step back out of a flow, clear
              // the box, then close.
              if (root.answerMode) root.leaveAnswer()
              else if (root.flowStack.length > 0) root.flowBack()
              else if (input.text.length > 0) input.text = ""
              else root.dismiss()
              event.accepted = true
            } else if (event.key === Qt.Key_Down
                       || (event.key === Qt.Key_N && (event.modifiers & Qt.ControlModifier))) {
              root.move(root.verticalStep)
              event.accepted = true
            } else if (event.key === Qt.Key_Up
                       || (event.key === Qt.Key_P
                           && (event.modifiers & Qt.ControlModifier)
                           && (event.modifiers & Qt.ShiftModifier))) {
              root.move(-root.verticalStep)
              event.accepted = true
            } else if (event.key === Qt.Key_P && (event.modifiers & Qt.ControlModifier)) {
              // Ctrl+P is the pin, and moving up gained the Shift.
              root.togglePin()
              event.accepted = true
            } else if (event.key === Qt.Key_Tab) {
              root.move(1)
              event.accepted = true
            } else if (event.key === Qt.Key_Backtab) {
              root.move(-1)
              event.accepted = true
            } else if ((root.activeView === "slider" || root.activeView === "timegrid"
                        || root.activeView === "emoji" || root.activeView === "themes"
                        || root.activeView === "windows" || root.activeView === "menutree"
                        || root.activeView === "radioplayer")
                       && (event.key === Qt.Key_Left || event.key === Qt.Key_Right)) {
              // Left and right belong to the text cursor everywhere else, and
              // are taken back only while the thing on screen is a row of
              // ranges, where they are the whole point of the view.
              if (resultsArea.item && typeof resultsArea.item.nudge === "function") {
                resultsArea.item.nudge(event.key === Qt.Key_Right ? 1 : -1)
              }
              event.accepted = true
            } else if ((root.activeView === "grid" || root.activeView === "dashboard"
                        || root.activeView === "calendar" || root.activeView === "docker"
                        || root.activeView === "marketplace")
                       && event.key === Qt.Key_Right) {
              root.move(1)
              event.accepted = true
            } else if ((root.activeView === "grid" || root.activeView === "dashboard"
                        || root.activeView === "calendar" || root.activeView === "docker"
                        || root.activeView === "marketplace")
                       && event.key === Qt.Key_Left) {
              root.move(-1)
              event.accepted = true
            } else if (event.key === Qt.Key_PageDown) {
              root.move(root.maxRows)
              event.accepted = true
            } else if (event.key === Qt.Key_PageUp) {
              root.move(-root.maxRows)
              event.accepted = true
            } else if ((event.modifiers & Qt.ControlModifier)
                       && event.key >= Qt.Key_1 && event.key <= Qt.Key_9) {
              // Run the nth row without walking to it.
              var nth = event.key - Qt.Key_1
              if (nth < root.rows.length) root.activate(root.rows[nth])
              event.accepted = true
            } else if ((event.key === Qt.Key_Return || event.key === Qt.Key_Enter)
                       && (event.modifiers & Qt.ControlModifier)) {
              root.ask(input.text.trim())
              event.accepted = true
            } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
              root.activateSelected(event.modifiers & Qt.ShiftModifier)
              event.accepted = true
            }
          }

          Text {
            anchors.fill: parent
            visible: input.text.length === 0
            verticalAlignment: Text.AlignVCenter
            text: "Search apps, do maths, run a command"
            color: Qt.darker(root.foreground, 1.8)
            font.family: root.fontFamily
            font.pixelSize: Style.font.title
          }
        }
      }

      Rectangle {
        anchors.top: header.bottom
        anchors.left: parent.left
        anchors.right: parent.right
        height: root.rows.length > 0 ? Math.max(1, Style.space(1)) : 0
        color: Qt.rgba(root.foreground.r, root.foreground.g, root.foreground.b, 0.1)
      }

      // Nothing to show, and nothing still coming. Without this the card just
      // collapses to a box with a cursor in it, which reads as broken rather
      // than as no results.
      Item {
        id: emptyState
        anchors.top: header.bottom
        anchors.left: parent.left
        anchors.right: parent.right
        height: visible ? Style.space(64) : 0
        // A notice keeps this open even on an empty box: an action that clears
        // your history leaves nothing to show, and a launcher that answers a
        // keypress with a blank card has not told you it worked.
        visible: root.notice !== "" || root.pendingConfirm !== ""
          || (root.activeView !== "loading" && root.rows.length === 0 && !root.answerMode && root.queryText.trim() !== "")

        Text {
          anchors.centerIn: parent
          horizontalAlignment: Text.AlignHCenter
          color: root.notice !== "" ? Color.accent : Qt.darker(root.foreground, 1.9)
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          text: {
            if (root.notice !== "") return root.notice
            if (root.pendingConfirm !== "") return root.pendingConfirm + "   ↵ again to confirm"

            // Nothing here while something is still answering.
            if (root.busy) return ""
            if (root.scopeLabel === "") return "Nothing matches that"

            // A bare keyword has nothing to quote back.
            var term = root.queryText.replace(/^[a-z0-9_-]+:\s*/i, "")
            return term === ""
              ? "Nothing in " + root.scopeLabel
              : "Nothing in " + root.scopeLabel + " matches “" + term + "”"
          }
        }
      }

      Loader {
        id: resultsArea
        anchors.top: header.bottom
        anchors.topMargin: Style.space(8)
        anchors.left: parent.left
        anchors.right: parent.right
        active: root.rows.length > 0 || root.answerMode || root.activeView === "loading"

        // A view asks for the height its content wants, and the card grows to
        // hold it. The cap goes here rather than on the card, because the
        // footer follows this item's bottom: a shorter card would have hidden
        // the footer instead of shortening the list.
        readonly property int room: panel.height - card.y - header.height
          - footer.height - Style.space(14) - Style.space(24)

        height: Math.max(0, Math.min(item ? item.implicitHeight : 0, resultsArea.room))

        sourceComponent: {
          switch (root.activeView) {
          case "hero": return heroView
          case "cards": return cardsView
          case "split": return splitView
          case "grid": return gridView
          case "dashboard": return dashboardView
          case "calendar": return calendarView
          case "timegrid": return timeGridView
          case "zones": return zonesView
          case "gitrepo": return gitRepoView
          case "gitbranches": return gitBranchesView
          case "gitstashes": return gitStashesView
          case "agent": return agentView
          case "ghrepo": return ghRepoView
          case "ghpr": return ghPrView
          case "docker": return dockerView
          case "notes": return notesView
          case "processes": return processesView
          case "emoji": return emojiView
          case "themes": return themesView
          case "windows": return windowsView
          case "hosts": return hostsView
          case "shortcuts": return shortcutsView
          case "herdr": return herdrView
          case "marketplace": return marketplaceView
          case "marketplacehome": return marketplaceHomeView
          case "marketplaceunit": return marketplaceUnitView
          case "radios": return radiosView
          case "radioplayer": return radioPlayerView
          case "vault": return vaultView
          case "snippets": return snippetsView
          case "menutree": return menuTreeView
          case "repos": return reposView
          case "files": return filesView
          case "loading": return loadingView
          case "player": return playerView
          case "slider": return sliderView
          case "form": return formView
          case "answer": return answerView
          default: return listView
          }
        }
      }

      Component { id: listView;  ResultList  { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: heroView;  ResultHero  { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: cardsView; ResultCards { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: splitView; ResultSplit { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: gridView;  ResultGrid  { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: answerView; ResultAnswer { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: dashboardView; ResultDashboard { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: calendarView;  ResultCalendar  { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: timeGridView;  ResultTimeGrid  { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: zonesView;     ResultZones     { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: gitRepoView;   ResultGitRepo   { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: gitBranchesView; ResultGitBranches { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: gitStashesView;  ResultGitStashes  { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: agentView;     ResultAgent     { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: ghRepoView;    ResultGhRepo    { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: ghPrView;      ResultGhPr      { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: dockerView;    ResultDocker    { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: notesView;     ResultNotes     { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: processesView; ResultProcesses { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: emojiView;     ResultEmoji     { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: themesView;    ResultThemes    { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: windowsView;   ResultWindows   { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: hostsView;     ResultHosts     { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: shortcutsView; ResultShortcuts { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: herdrView;     ResultHerdr     { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: marketplaceView; ResultMarketplace { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: marketplaceHomeView; ResultMarketplaceHome { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: marketplaceUnitView; ResultMarketplaceUnit { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: radiosView;    ResultRadios    { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: radioPlayerView; ResultRadioPlayer { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: filesView;     ResultFiles     { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: reposView;     ResultRepos     { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: menuTreeView;  ResultMenuTree  { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: snippetsView;  ResultSnippets  { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: vaultView;     ResultVault     { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: loadingView;   ResultLoading   { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: playerView;    ResultPlayer    { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: sliderView;    ResultSlider    { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }
      Component { id: formView;      ResultForm      { launcher: root; width: resultsArea.width; maxHeight: resultsArea.room } }

      // The hint bar: what Enter does, and that there is more on Ctrl+K.
      Item {
        id: footer
        anchors.top: resultsArea.bottom
        anchors.left: parent.left
        anchors.right: parent.right
        height: (root.rows.length > 0 || root.answerMode) ? Style.space(30) : 0
        visible: root.rows.length > 0 || root.answerMode

        Rectangle {
          anchors.top: parent.top
          anchors.left: parent.left
          anchors.right: parent.right
          height: Math.max(1, Style.space(1))
          color: Qt.rgba(root.foreground.r, root.foreground.g, root.foreground.b, 0.08)
        }

        Text {
          // Bounded by whatever the right of the bar is drawing, and elided.
          anchors.left: parent.left
          anchors.leftMargin: Style.space(18)
          anchors.right: footerRight.visible ? footerRight.left : parent.right
          anchors.rightMargin: Style.space(12)
          elide: Text.ElideRight
          anchors.verticalCenter: parent.verticalCenter
          text: {
            // A form's Enter is its own submit.
            if (root.activeView === "form" && root.rows.length > 0) {
              return "↵  " + String(root.rows[0].submit || "Submit")
            }
            if (root.activeView === "slider") return "← →  Adjust"
            var actions = root.currentActions()
            return actions.length > 0 ? "↵  " + String(actions[0].title || "Open") : ""
          }
          color: Qt.darker(root.foreground, 1.8)
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
        }

        Text {
          id: footerRight
          anchors.right: parent.right
          anchors.rightMargin: Style.space(18)
          anchors.verticalCenter: parent.verticalCenter
          visible: root.currentActions().length > 1 || root.answerMode
                   || root.answerAvailable || root.flowStack.length > 0
          text: {
            if (root.answerMode) return root.answerStreaming ? "⎋  Stop" : "⎋  Back"
            var actions = root.currentActions()
            var parts = []
            // First, because it is the way out of somewhere you were led.
            if (root.flowStack.length > 0) parts.push("⎋  Back")
            // Truncated here rather than elided by the Text: this line is
            // three labels sharing one row.
            if (actions.length > 1) {
              var second = String(actions[1].title)
              if (second.length > 22) second = second.slice(0, 21) + "…"
              parts.push("⇧↵  " + second)
            }
            if (root.answerAvailable && input.text.trim() !== "") parts.push("⌃↵  Ask " + root.answerModel)
            if (actions.length > 1) parts.push("⌃K  Actions")
            return parts.join("     ")
          }
          color: Qt.darker(root.foreground, 1.8)
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
        }
      }

    }
  }

}
