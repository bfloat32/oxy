import Quickshell
import Quickshell.Io
import QtQuick

// The event log: one JSON object per line in
// ~/.local/state/omarchy/oxy-log.jsonl.
//
// Every line stands alone — timestamp, session id, and the query epoch where
// one matters — so the file can be handed to an agent later and each event
// means something without its neighbours. The epoch is the important part:
// `prov.start` and `prov.done` for the same keystroke share it, which is what
// lets a reader reconstruct who answered late, who timed out, and whose answer
// was dropped as stale.
//
// Writes are buffered and appended by one process per flush — a printf whose
// arguments are the lines — because a launcher that forked per event would
// spend more logging than launching. A crash between flushes loses at most a
// few hundred milliseconds of tail, which is a better trade than a fork per
// keystroke. The file rotates into a single .old at 1MB, so it can never hold
// more than about 2MB of history, and what it does hold is exactly the recent
// past a bug report needs.
Item {
  id: logger

  property bool enabled: true
  property string path: (Quickshell.env("XDG_STATE_HOME") || Quickshell.env("HOME") + "/.local/state") + "/omarchy/oxy-log.jsonl"
  property int maxBytes: 1048576

  // One id per shell lifetime. `open` and `close` events carry the per-summon
  // story; this only has to tell one boot's events from the next boot's.
  property string sid: ""
  property var pending: []
  property int dropped: 0
  property bool broken: false

  // A field that could be arbitrarily long (a query, a command, a title) is
  // truncated before it is logged: the log's job is to explain behavior, and a
  // ten-thousand-character query pasted by accident explains nothing more than
  // its first two hundred characters do.
  function clip(value, max) {
    var s = String(value === undefined || value === null ? "" : value)
    return s.length > (max || 200) ? s.slice(0, max || 200) + "…" : s
  }

  function log(ev, fields) {
    if (!enabled || broken) return
    var e = { ts: Date.now(), sid: sid, ev: String(ev) }
    if (fields) for (var k in fields) e[k] = fields[k]
    var line
    try {
      line = JSON.stringify(e)
    } catch (err) {
      return
    }
    // A fresh array, never the same one mutated and put back: QML compares by
    // identity, so an in-place push changes nothing the `running` binding on
    // the flush timer can see, and the log would simply never be written.
    var next = pending.concat([line])
    // A wedged writer must not let this grow without bound: keep the newest
    // tail and count what was thrown away, so the gap is visible in the file
    // rather than a silent hole.
    if (next.length > 2000) {
      dropped += next.length - 2000
      next = next.slice(-2000)
    }
    pending = next
  }

  function flush() {
    if (!enabled) {
      pending = []
      return
    }
    if (pending.length === 0 || writer.running || broken) return
    var lines = pending
    pending = []
    if (dropped > 0) {
      lines.push(JSON.stringify({ ts: Date.now(), sid: sid, ev: "log.drop", n: dropped }))
      dropped = 0
    }
    // The path and the cap travel as $1 and $2 so the lines can sit in "$@"
    // untouched: nothing a query contains can become an option or a redirect,
    // because it is never inside the command text.
    writer.command = ["bash", "-c",
      'f="$1"; cap="$2"; shift 2; ' +
      'd="${f%/*}"; [ -d "$d" ] || mkdir -p "$d"; ' +
      'if [ -f "$f" ]; then ' +
      '  s=$(stat -c%s "$f" 2>/dev/null || echo 0); ' +
      '  [ "${s:-0}" -gt "$cap" ] && mv -f "$f" "$f.old"; ' +
      'fi; ' +
      'printf "%s\\n" "$@" >>"$f"',
      "oxy-log", path, String(maxBytes)].concat(lines)
    writer.running = true
  }

  Component.onCompleted: sid = Date.now().toString(36)

  Component.onDestruction: flush()

  Timer {
    interval: 400
    repeat: true
    running: logger.pending.length > 0
    onTriggered: logger.flush()
  }

  Process {
    id: writer
    onExited: function (code) {
      // A writer that fails once (a full disk, a state dir that cannot be
      // made) would fail every flush: better to go quiet than to fork a
      // failing process every 400ms forever.
      if (code !== 0) logger.broken = true
    }
  }
}
