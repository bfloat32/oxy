//! The pure half of `docker:` — the script's jq program taken apart into
//! functions a test can drive over canned `inspect` output. `published`
//! is the port map, `band` the sort key the view spends on the colour of
//! each tile's edge, `bytes`/`human` the unit math, and `build` the whole
//! `--slurp` pipeline from inspect lines to emitted rows.
//!
//! `None` is this port's spelling of a jq error: where the script's jq
//! would have died mid-pipe — a `sub` on a non-string, an index into a
//! scalar, a `tonumber` on "1.2.3" — these return `None` and the whole
//! answer goes with it, which is what the script produced when that
//! happened: silence. The `//` operators in the script catch their own
//! side's errors, so the fields they guard (`Status`, `Health`, the stats
//! reads that end in `// null`) degrade to their defaults instead.

use serde_json::{Map, Value, json};

use crate::support::quote::quote;

/// jq's `dbPort` deny list, verbatim: container ports that are never
/// worth a browser tab. A container publishing 7788 is far more likely a
/// dev server than a database — guessing wrong costs one tab — while an
/// allow list would leave most real services with a dead chip.
const DB_PORTS: [f64; 12] = [
    22.0, 25.0, 53.0, 123.0, 1433.0, 3306.0, 5432.0, 5672.0, 6379.0, 9092.0, 11211.0, 27017.0,
];

/// What the needle did to one container: `Row` survives to the emit,
/// `Filtered` is `select` dropping it, `Poison` the jq error that emptied
/// the whole answer.
enum Fate {
    Row(Map<String, Value>),
    Filtered,
    Poison,
}

/// A jq-shaped number value: whole values stay ints so `8080` never
/// prints `8080.0`.
fn jnum(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() <= 9e15 {
        json!(f as i64)
    } else {
        json!(f)
    }
}

/// jq's `\(x)` interpolation — the shortest decimal form.
fn jstr(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() <= 9e15 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

/// `x // d` — jq's alternative: null and false both fall through.
fn jq_or(v: Option<&Value>, default: Value) -> Value {
    match v {
        Some(Value::Null) | Some(Value::Bool(false)) | None => default,
        Some(v) => v.clone(),
    }
}

/// jq's `tonumber? // 0`: numbers pass through, strings parse or land on
/// the default, everything else is the default.
fn tonumber0(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// jq's `. > 0` is a total order — null < false < true < numbers <
/// strings < arrays < objects — so a string is always "over" and a bool
/// never is. Only numbers compare numerically.
fn over_zero(v: &Value) -> bool {
    match v {
        Value::Null | Value::Bool(_) => false,
        Value::Number(n) => n.as_f64().unwrap_or(0.0) > 0.0,
        _ => true,
    }
}

// ------------------------------------------------------------------ epoch

/// The civil-date math `recent.rs` already carries, duplicated as two
/// small functions rather than shared: the timestamp shape here is fixed
/// and needs no date library.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// `fromdateiso8601` for the stamps docker writes —
/// `YYYY-MM-DDTHH:MM:SS[Z|±HH:MM]` after the fraction is stripped. The
/// year-1 stamp on a container that never stopped lands in the negatives
/// and the caller clamps it.
fn parse_stamp(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b't') {
        return None;
    }
    let num = |a: usize, z: usize| s[a..z].parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let mut stamp = days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + sec;
    // A trailing offset: Z means UTC, ±HH:MM (or ±HHMM) shifts the other way.
    let rest = &s[19..];
    if rest.starts_with('+') || rest.starts_with('-') {
        let digits: String = rest[1..].chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.len() >= 4 {
            let oh = digits[..2].parse::<i64>().unwrap_or(0);
            let om = digits[2..4].parse::<i64>().unwrap_or(0);
            let shift = oh * 3600 + om * 60;
            stamp += if rest.starts_with('-') { shift } else { -shift };
        }
    }
    Some(stamp)
}

/// `sub("\\.[0-9]+"; "")` — the first dotted digit run gone, because
/// docker writes RFC3339 with nanoseconds and `fromdateiso8601` refuses
/// them.
fn strip_frac(s: &str) -> String {
    let b = s.as_bytes();
    for i in 0..b.len() {
        if b[i] == b'.' && i + 1 < b.len() && b[i + 1].is_ascii_digit() {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            return format!("{}{}", &s[..i], &s[j..]);
        }
    }
    s.to_string()
}

/// `def epoch`: `// ""` reads null and false as empty; the `?` on
/// `fromdateiso8601` turns an unreadable stamp into 0; and a stamp before
/// 1970 — the year-1 "never stopped" — clamps to 0. `None` is `sub` on a
/// non-string: the error that killed jq.
fn epoch(v: Option<&Value>) -> Option<i64> {
    let s = match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => "",
        Some(Value::String(s)) => s.as_str(),
        Some(_) => return None,
    };
    if s.is_empty() {
        return Some(0);
    }
    let t = parse_stamp(&strip_frac(s)).unwrap_or(0);
    Some(if t < 0 { 0 } else { t })
}

// ------------------------------------------------------------------ ports

/// `def hostFor` — a port bound to every address is reachable as
/// localhost; one bound to a particular address is only reachable there,
/// and saying localhost about it would send the user somewhere that is
/// not listening.
fn host_for(v: &Value) -> Value {
    match v {
        Value::Null => json!("localhost"),
        Value::String(s) if s.is_empty() || s == "0.0.0.0" || s == "::" => json!("localhost"),
        other => other.clone(),
    }
}

/// `def published` — the ports worth opening in a browser, one entry per
/// (host port, protocol): a `-p` that binds both `0.0.0.0` and `::` is
/// one service and two map entries, collapsed on the pair so two
/// genuinely different services that share a number stay apart.
/// `None` is the jq death: `map` over a non-array, a `HostPort` on a
/// scalar, `test(":")` on a non-string address.
fn published(ports: Option<&Value>) -> Option<Vec<Value>> {
    // `(.ports // {})` — jq's `//` drops null and false both.
    let obj = match ports {
        None | Some(Value::Null) | Some(Value::Bool(false)) => return Some(Vec::new()),
        Some(Value::Object(m)) => m,
        // `to_entries` on an array yields numeric keys and `split("/")`
        // dies on those; on a scalar it dies immediately.
        Some(_) => return None,
    };
    let mut flat: Vec<Map<String, Value>> = Vec::new();
    for (key, value) in obj {
        // `select(.value != null)` — the entry a port has before it is
        // bound anywhere.
        if value.is_null() {
            continue;
        }
        let Some(bindings) = value.as_array() else {
            // `map` on a non-array is the error that killed jq.
            return None;
        };
        let parts: Vec<&str> = key.split('/').collect();
        // `($parts[0] | tonumber? // 0)` / `($parts[1] // "tcp")`.
        let container = parts
            .first()
            .and_then(|p| p.parse::<f64>().ok())
            .unwrap_or(0.0);
        let proto = parts.get(1).copied().unwrap_or("tcp").to_string();
        for b in bindings {
            let Some(b) = b.as_object() else {
                return None; // `.HostPort` on a scalar dies
            };
            let mut e = Map::new();
            e.insert("container".into(), jnum(container));
            e.insert("proto".into(), json!(proto));
            e.insert("host".into(), jnum(tonumber0(b.get("HostPort"))));
            e.insert(
                "addr".into(),
                host_for(b.get("HostIp").unwrap_or(&Value::Null)),
            );
            flat.push(e);
        }
    }
    // `group_by([.host, .proto]) | map(.[0]) | sort_by(.host)` — sorting
    // by the pair and keeping the first of each run is both steps at
    // once: the second sort is stable over an already-sorted order.
    fn num(m: &Map<String, Value>, k: &str) -> f64 {
        m.get(k).and_then(Value::as_f64).unwrap_or(0.0)
    }
    fn txt<'a>(m: &'a Map<String, Value>, k: &str) -> &'a str {
        m.get(k).and_then(Value::as_str).unwrap_or("")
    }
    flat.sort_by(|a, b| {
        num(a, "host")
            .total_cmp(&num(b, "host"))
            .then_with(|| txt(a, "proto").cmp(txt(b, "proto")))
    });
    flat.dedup_by(|a, b| a.get("host") == b.get("host") && a.get("proto") == b.get("proto"));

    let mut out = Vec::with_capacity(flat.len());
    for mut e in flat {
        let host = e.get("host").and_then(Value::as_f64).unwrap_or(0.0);
        let proto = e
            .get("proto")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let container = e.get("container").and_then(Value::as_f64).unwrap_or(0.0);
        // `label` is the bare host port plus the protocol when it is not
        // tcp; `web` is the deny list on the *container* port.
        let label = if proto == "tcp" {
            jstr(host)
        } else {
            format!("{}/{}", jstr(host), proto)
        };
        let web = !DB_PORTS.contains(&container);
        e.insert("label".into(), json!(label));
        e.insert("web".into(), json!(web));
        // An IPv6 literal in a URL needs brackets — `http://::1:8080` is
        // not an address anybody can parse. Only the URL is bracketed;
        // the copyable host:port form reads the conventional way.
        let Some(addr) = e.get("addr").and_then(Value::as_str).map(str::to_string) else {
            return None; // `test(":")` on a non-string dies
        };
        let url = if addr.contains(':') {
            if web {
                format!("http://[{addr}]:{}", jstr(host))
            } else {
                format!("[{addr}]:{}", jstr(host))
            }
        } else if web {
            format!("http://{addr}:{}", jstr(host))
        } else {
            format!("{addr}:{}", jstr(host))
        };
        let exec = if web {
            format!("omarchy-launch-browser {}", quote(&url))
        } else {
            format!("printf %s {} | wl-copy", quote(&url))
        };
        e.insert("url".into(), json!(url));
        e.insert("exec".into(), json!(exec));
        out.push(Value::Object(e));
    }
    Some(out)
}

// ------------------------------------------------------------------ units

/// `def band` — paused sits with running (it still holds its ports and
/// its memory, and it is not a container you are about to start), then
/// restarting — the crash loop you came to deal with — then the rest.
/// The number is the sort key and the colour of the bar down the left
/// edge of the tile.
fn band(status: &Value) -> i64 {
    match status.as_str() {
        Some("running") | Some("paused") => 0,
        Some("restarting") => 1,
        _ => 2,
    }
}

/// `capture("^(?<n>[0-9.]+) *(?<u>[A-Z]*)$")` by hand — digits and dots,
/// spaces, capitals, end of string.
fn split_size(s: &str) -> Option<(String, String)> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    let mut j = i;
    while j < b.len() && b[j] == b' ' {
        j += 1;
    }
    let mut k = j;
    while k < b.len() && b[k].is_ascii_uppercase() {
        k += 1;
    }
    if k != b.len() {
        return None; // the `$` anchor
    }
    Some((s[..i].to_string(), s[j..k].to_string()))
}

/// `def bytes` — "12.5MiB" as a number, binary and decimal suffixes both,
/// because the daemon has used both over the years and getting it wrong
/// by a factor of 1.024 in a bar nobody can measure is the kind of error
/// that never gets found. `None` is the jq death: `ascii_upcase` on a
/// non-string, or the unguarded `tonumber` on "1.2.3".
fn bytes(v: &Value) -> Option<f64> {
    let s = match v {
        // `(. // "")` — null and false read as the empty string.
        Value::Null | Value::Bool(false) => String::new(),
        Value::String(s) => s.to_ascii_uppercase(),
        _ => return None,
    };
    let Some((n, u)) = split_size(&s) else {
        return Some(0.0); // `(capture(...) // null)` — no match is 0
    };
    // `($m.n | tonumber)` carries no `?` — a malformed number is fatal.
    let n = n.parse::<f64>().ok()?;
    let mult = match u.as_str() {
        "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        "KIB" => 1024.0,
        "MIB" => 1048576.0,
        "GIB" => 1073741824.0,
        "TIB" => 1099511627776.0,
        _ => 1.0, // "B" and everything the table never named
    };
    Some(n * mult)
}

/// `def human` — the size a meter label reads: `1.5GiB`, `512MiB`,
/// `640KiB`, `97B`.
fn human(v: f64) -> String {
    if v >= 1073741824.0 {
        format!("{}GiB", jstr((v / 1073741824.0 * 10.0).round() / 10.0))
    } else if v >= 1048576.0 {
        format!("{}MiB", jstr((v / 1048576.0).round()))
    } else if v >= 1024.0 {
        format!("{}KiB", jstr((v / 1024.0).round()))
    } else {
        format!("{}B", jstr(v.round()))
    }
}

/// The `select` haystack — `name image id service project host:port…`,
/// lowercased. Ports are in it because `docker:8080` is a question people
/// actually ask: you know which port is answering and you want the
/// container behind it, which is the one thing the name never tells you.
/// `None` is jq's `+` error — a field that is neither string nor null.
fn haystack(
    name: &str,
    image: Option<&Value>,
    id: &Value,
    service: Option<&Value>,
    project: Option<&Value>,
    open: &[Value],
) -> Option<String> {
    // `(.image // "")` etc. — null and false read as "", then `+` needs
    // a string or it dies.
    let word = |v: Option<&Value>| -> Option<String> {
        match v {
            None | Some(Value::Null) | Some(Value::Bool(false)) => Some(String::new()),
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => None,
        }
    };
    let id_part = match id {
        // `+ null` is jq's identity — a missing id contributes nothing.
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        _ => return None,
    };
    let port_part = open
        .iter()
        .map(|p| {
            let host = p.get("host").and_then(Value::as_f64).unwrap_or(0.0);
            let cont = p.get("container").and_then(Value::as_f64).unwrap_or(0.0);
            format!("{} {}", jstr(host), jstr(cont))
        })
        .collect::<Vec<_>>()
        .join(" ");
    Some(
        format!(
            "{} {} {} {} {} {}",
            name,
            word(image)?,
            id_part,
            word(service)?,
            word(project)?,
            port_part
        )
        .to_ascii_lowercase(),
    )
}

// ------------------------------------------------------------------ stats

/// `jq -Rn '[inputs | split("\t") | select(length >= 4)
/// | {id: .[0], cpu: .[1], mem: .[2], memPct: .[3]}] | INDEX(.id)'` — the
/// sampler's TSV keyed on the 12-char id `docker stats` prints, the last
/// line for an id winning the way INDEX keeps the last.
pub fn stats_map(tsv: &str) -> Map<String, Value> {
    let mut m = Map::new();
    for line in tsv.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 4 {
            continue;
        }
        m.insert(
            f[0].to_string(),
            json!({ "id": f[0], "cpu": f[1], "mem": f[2], "memPct": f[3] }),
        );
    }
    m
}

// ------------------------------------------------------------------ build

/// One inspect line → the `$c` object the emit reads. `Fate::Poison` is
/// jq's death — a type error mid-pipe — which empties the whole answer in
/// the script too.
fn container(
    item: &Value,
    stats: &Map<String, Value>,
    now: i64,
    cores: f64,
    hostmem: f64,
    needle: &str,
) -> Fate {
    let Some(obj) = item.as_object() else {
        return Fate::Poison; // `.name` on a scalar dies
    };

    // `(.name | sub("^/"; ""))` — docker writes the leading slash.
    let name = match obj.get("name") {
        Some(Value::String(s)) => s.strip_prefix('/').unwrap_or(s).to_string(),
        _ => return Fate::Poison, // `sub` on a non-string — null included
    };
    // `.id[0:12]` — jq slices strings by character; a non-string id dies
    // here or at the emit's `@sh`, so poison either way.
    let id = obj.get("id").cloned().unwrap_or(Value::Null);
    let Some(short) = id.as_str().map(|s| s.chars().take(12).collect::<String>()) else {
        return Fate::Poison;
    };

    // `.state` — the guarded reads (`Status`, `Health`, `ExitCode`) fall
    // to their `//` defaults when it is a scalar, but `OOMKilled` and the
    // `StartedAt`/`FinishedAt` indexes carry no `//`: a non-object state
    // dies either way, so it dies here.
    let state = obj.get("state");
    match state {
        None | Some(Value::Null) | Some(Value::Object(_)) => {}
        Some(_) => return Fate::Poison,
    }
    let field =
        |k: &str| -> Option<&Value> { state.and_then(Value::as_object).and_then(|m| m.get(k)) };

    let status = jq_or(field("Status"), json!("unknown"));
    // `.state.Health.Status // ""` — a scalar Health's index error lands
    // on the default the same as a missing one.
    let health = match field("Health") {
        Some(Value::Object(h)) => jq_or(h.get("Status"), json!("")),
        _ => json!(""),
    };
    let restarts = jq_or(obj.get("restarts"), json!(0));
    let exit_code = jq_or(field("ExitCode"), json!(0));
    let oom = field("OOMKilled") == Some(&Value::Bool(true));

    let open = match published(obj.get("ports")) {
        Some(p) => p,
        None => return Fate::Poison,
    };

    // `select($needle == "" or (haystack | ascii_downcase |
    // contains($needle)))` — jq's `or` short-circuits, so an unjoinable
    // field only kills a narrowed query.
    if !needle.is_empty() {
        match haystack(
            &name,
            obj.get("image"),
            &id,
            obj.get("service"),
            obj.get("project"),
            &open,
        ) {
            Some(h) if h.contains(needle) => {}
            Some(_) => return Fate::Filtered,
            None => return Fate::Poison,
        }
    }

    let b = band(&status);
    let up = b == 0;
    // `($stats[$short] // null)` — absent, null and false all mean "no
    // live numbers"; a non-object entry dies the moment `.mem` is split.
    let live = match stats.get(&short) {
        None | Some(Value::Null) | Some(Value::Bool(false)) => None,
        Some(v) => Some(v),
    };

    // Seconds — the view decides whether that reads as uptime or as how
    // long ago it died: `if $up then StartedAt else FinishedAt | epoch`
    // then `if . > 0 then $now - . else 0`.
    let at = field(if up { "StartedAt" } else { "FinishedAt" });
    let since = match epoch(at) {
        Some(t) if t > 0 => now - t,
        Some(_) => 0,
        None => return Fate::Poison,
    };

    let project = jq_or(obj.get("project"), json!(""));
    let service = jq_or(obj.get("service"), json!(""));
    let image = jq_or(obj.get("image"), json!(""));

    // `if $live` gates every stat read: absent stats are null fields —
    // `memBytes` the script's odd 0 — until the sampler has answered once.
    let (cpu, mem, mem_bytes, mem_pct) = match live {
        None => (Value::Null, Value::Null, json!(0), Value::Null),
        Some(l) => {
            let Some(lo) = l.as_object() else {
                return Fate::Poison;
            };
            // `rtrimstr("%") | tonumber? // null` — every failure lands
            // on null; only a string ever reaches the number.
            let pct = |v: Option<&Value>| {
                v.and_then(Value::as_str)
                    .and_then(|s| s.strip_suffix('%').unwrap_or(s).parse::<f64>().ok())
                    .map(jnum)
                    .unwrap_or(Value::Null)
            };
            let cpu = pct(lo.get("cpu"));
            let mem = match lo.get("mem") {
                Some(Value::String(s)) => s.split(" / ").next().unwrap_or("").to_string(),
                // `split` on a non-string is the stats read with no `//`
                // guard — the error that killed jq.
                _ => return Fate::Poison,
            };
            let Some(mem_bytes) = bytes(&Value::String(mem.clone())) else {
                return Fate::Poison;
            };
            (cpu, json!(mem), jnum(mem_bytes), pct(lo.get("memPct")))
        }
    };

    // `(.memCap // 0) | if . > 0 then . else $hostmem` — a container with
    // no cap is drawn against the whole machine, and the tile says so.
    let mem_cap_in = jq_or(obj.get("memCap"), json!(0));
    let mem_cap = if over_zero(&mem_cap_in) {
        mem_cap_in.clone()
    } else {
        jnum(hostmem)
    };
    let mem_cap_text = if over_zero(&mem_cap_in) {
        match mem_cap_in.as_f64() {
            Some(n) => format!("{} limit", human(n)),
            None => return Fate::Poison, // `human` on a non-number dies
        }
    } else {
        format!("{} host", human(hostmem))
    };

    // CPU the same way: `docker stats` reports one busy core as 100%, so
    // on a sixteen-core box a container with no quota is drawn against
    // 1600 and a container held to 0.15 of a core is drawn against 15.
    let nano_in = jq_or(obj.get("nanoCpus"), json!(0));
    let denom = if over_zero(&nano_in) {
        match nano_in.as_f64() {
            Some(n) => n / 1e9,
            None => return Fate::Poison, // `. / 1e9` on a non-number dies
        }
    } else {
        cores
    };
    let cpu_full = jnum(denom * 100.0);
    let cpu_full_text = if denom == 1.0 {
        "1 cpu".to_string()
    } else {
        format!("{} cpus", jstr((denom * 100.0).round() / 100.0))
    };

    // The script's object, in its own key order.
    let mut c = Map::new();
    c.insert("name".into(), json!(name));
    c.insert("cid".into(), json!(short));
    c.insert("full".into(), id);
    c.insert("image".into(), image);
    c.insert("status".into(), status);
    c.insert("band".into(), json!(b));
    c.insert("health".into(), health);
    c.insert("restarts".into(), restarts);
    c.insert("exitCode".into(), exit_code);
    c.insert("oom".into(), json!(oom));
    c.insert("since".into(), json!(since));
    c.insert("project".into(), project);
    c.insert("service".into(), service);
    c.insert("ports".into(), Value::Array(open));
    c.insert("cpu".into(), cpu);
    c.insert("mem".into(), mem);
    c.insert("memBytes".into(), mem_bytes);
    c.insert("memPct".into(), mem_pct);
    c.insert("memCap".into(), mem_cap);
    c.insert("memCapText".into(), json!(mem_cap_text));
    c.insert("cpuFull".into(), cpu_full);
    c.insert("cpuFullText".into(), json!(cpu_full_text));
    Fate::Row(c)
}

/// The emit block — `id`, `view`, the list fields the launcher still
/// reads, `score`, the tile fields spread over, and the actions. The exec
/// strings are the script's verbatim: `omarchy-launch-tui` for logs and
/// shell, `docker <verb>` wrapped in a notification so a failure is seen
/// rather than silent. `None` is the jq death — an `@sh` on a non-string.
fn emit(c: &Map<String, Value>, i: usize, cores: f64, hostmem: f64) -> Option<Value> {
    // `($c.full | @sh)` — jq's shell quoting is the single-quote word
    // `support::quote::quote` produces.
    let full = c.get("full").and_then(Value::as_str)?;
    let name = c.get("name").and_then(Value::as_str)?;
    let q = quote(full);
    let logs =
        format!("omarchy-launch-tui --app-id=org.omarchy.docker docker logs -f --tail 200 {q}");
    // TERM travels into the container because foot sets TERM=foot and no
    // image ships foot terminfo; the name printed first keeps the window
    // from reading as hung while `docker exec` answers, and the trailing
    // `read` keeps a failure on screen.
    let inner = format!(
        "printf {} {}; docker exec -it -e TERM=xterm-256color {q} sh -c {} \
         || {{ printf {} \"$?\"; read -rsn1; }}",
        quote("%s\\n"),
        quote(name),
        quote("command -v bash >/dev/null && exec bash || exec sh"),
        quote("\\n[exit %s] press any key\\n"),
    );
    let shell = format!(
        "omarchy-launch-tui --app-id=org.omarchy.docker bash -c {}",
        quote(&inner)
    );

    let b = c.get("band").and_then(Value::as_i64).unwrap_or(2);
    let ports = c
        .get("ports")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let status = c.get("status").cloned().unwrap_or(Value::Null);
    let image = c.get("image").cloned().unwrap_or(Value::Null);
    let cid = c.get("cid").and_then(Value::as_str).unwrap_or("");

    let mut actions = vec![json!({
        "title": "Follow Logs",
        "shortcut": "↵",
        "exec": logs,
    })];
    if b == 0 {
        actions.push(json!({ "title": "Shell Inside", "exec": shell }));
    }
    // One container publishing four ports is four services, and these are
    // how the keyboard reaches what the view draws as chips.
    for p in &ports {
        let url = p.get("url").and_then(Value::as_str)?;
        let web = p.get("web").and_then(Value::as_bool).unwrap_or(false);
        let exec = p.get("exec").and_then(Value::as_str)?;
        actions.push(json!({
            "title": format!("{}{}", if web { "Open " } else { "Copy " }, url),
            "exec": exec,
        }));
    }
    // Stop, not start, for a container in a crash loop: it is already
    // trying to run, and stopping it is the thing you came here to do.
    // The notification keeps a failed verb from failing invisibly — nothing
    // is watching for a non-zero exit once the launcher has closed.
    for verb in [if b <= 1 { "stop" } else { "start" }, "restart"] {
        let title = format!("{}{}", verb[..1].to_uppercase(), &verb[1..]);
        let verbed = match verb {
            "stop" => "stopped",
            "start" => "started",
            _ => "restarted",
        };
        actions.push(json!({
            "title": title,
            "exec": format!(
                "out=$(docker {verb} {q} 2>&1) && omarchy-notification-send {} \
                 || omarchy-notification-send -u normal {} \"$out\"",
                quote(&format!("{name} {verbed}")),
                quote(&format!("docker {verb} failed")),
            ),
        }));
    }
    actions.push(json!({
        "title": "Copy Container ID",
        "exec": format!("printf %s {q} | wl-copy"),
    }));
    actions.push(json!({
        "title": "Copy Name",
        "exec": format!("printf %s {} | wl-copy", quote(name)),
    }));

    // `map(.label) | join(" ")` — a non-string label is the join error
    // that killed jq.
    let accessory = ports
        .iter()
        .map(|p| p.get("label").and_then(Value::as_str).map(str::to_string))
        .collect::<Option<Vec<String>>>()?
        .join(" ");

    // The script's row, in its own key order.
    let mut row = Map::new();
    row.insert("id".into(), json!(format!("docker:{cid}")));
    row.insert("view".into(), json!("docker"));
    // The docker view draws the band fields, but a row still needs a
    // title and a subtitle: the launcher reads them for recents, for
    // frecency and for the heading on the action panel.
    row.insert("title".into(), json!(name));
    row.insert("subtitle".into(), status);
    row.insert("detail".into(), image.clone());
    row.insert("accessory".into(), json!(accessory));
    row.insert("exec".into(), json!(logs));
    row.insert("score".into(), json!(90000 - i as i64 * 100));
    for k in [
        "cid",
        "full",
        "image",
        "status",
        "band",
        "health",
        "restarts",
        "exitCode",
        "oom",
        "since",
        "project",
        "service",
        "ports",
        "cpu",
        "cpuFull",
        "cpuFullText",
        "mem",
        "memBytes",
        "memPct",
        "memCap",
        "memCapText",
    ] {
        row.insert(k.into(), c.get(k).cloned().unwrap_or(Value::Null));
    }
    row.insert("hostCores".into(), jnum(cores));
    row.insert("hostMem".into(), jnum(hostmem));
    row.insert("actions".into(), Value::Array(actions));
    Some(Value::Object(row))
}

/// The whole pipeline: inspect lines plus the stats map in, emitted rows
/// out. `None` is jq's death — where the script's jq would have died
/// mid-pipe, the script produced silence and so does this.
pub fn build(
    items: &[Value],
    stats: &Map<String, Value>,
    now: i64,
    cores: f64,
    hostmem: f64,
    needle: &str,
) -> Option<Vec<Value>> {
    let mut cs = Vec::with_capacity(items.len());
    for item in items {
        match container(item, stats, now, cores, hostmem, needle) {
            Fate::Row(c) => cs.push(c),
            Fate::Filtered => {}
            Fate::Poison => return None,
        }
    }
    // `sort_by(.band)` — jq sorts stably, so within a band the containers
    // keep the order `docker ps` gave them, which is newest first.
    cs.sort_by(|a, b| {
        a.get("band")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .cmp(&b.get("band").and_then(Value::as_i64).unwrap_or(0))
    });
    cs.truncate(20); // `.[0:20]`
    let mut out = Vec::with_capacity(cs.len());
    for (i, c) in cs.iter().enumerate() {
        out.push(emit(c, i, cores, hostmem)?);
    }
    Some(out)
}
