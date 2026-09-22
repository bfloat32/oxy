//! `sys:` — the numbers you open a terminal to check: battery, uptime,
//! memory, disk, address, temperature, kernel, host. A port of
//! `bin/oxy-system`, on sysinfo plus the same /sys files it read.
//!
//! Every reading is optional: a desktop has no battery and a VM has no
//! thermal zone, so each block writes a row only when it has something true
//! to say.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::provider::{Ctx, NativeExt, NativeOutcome};
use crate::support::quote::quote;

/// sysinfo state lives behind a lock so the query body can run on
/// `spawn_blocking` — `/sys` reads, `/proc` refreshes and the odd subprocess
/// are synchronous work.
pub struct Sys {
    sys: std::sync::Arc<std::sync::Mutex<sysinfo::System>>,
    disks: std::sync::Arc<std::sync::Mutex<sysinfo::Disks>>,
}

impl Default for Sys {
    fn default() -> Self {
        Self::new()
    }
}

impl Sys {
    pub fn new() -> Sys {
        Sys {
            sys: std::sync::Arc::new(std::sync::Mutex::new(sysinfo::System::new())),
            disks: std::sync::Arc::new(std::sync::Mutex::new(sysinfo::Disks::new())),
        }
    }
}

/// The query matches when any of these words is in it — `sys:battery` and
/// `battery:` are the same question.
fn matches(query: &str, words: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    words.to_lowercase().contains(&q)
}

fn copy_exec(text: &str) -> String {
    format!("printf %s {} | wl-copy", quote(text))
}

fn read_trimmed(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// `free -h --si` for memory, `df -h` for disk — the script printed SI for
/// the first and 1024-based for the second, so the base is a parameter.
fn human_bytes(v: u64, si: bool) -> String {
    let base = if si { 1000.0 } else { 1024.0 };
    let units = ["B", "K", "M", "G", "T", "P"];
    let mut v = v as f64;
    let mut i = 0;
    while v >= base && i < units.len() - 1 {
        v /= base;
        i += 1;
    }
    if i > 0 && v < 10.0 {
        format!("{v:.1}{}", units[i])
    } else {
        format!("{v:.0}{}", units[i])
    }
}

/// The address this box would use to reach the internet: a UDP connect sends
/// nothing and reads the local end of the route, which is the whole trick
/// `ip route get 1.1.1.1` performs without a subprocess.
fn local_address() -> Option<(String, String)> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("1.1.1.1:80").ok()?;
    let addr = socket.local_addr().ok()?.ip().to_string();
    // `ip route get` printed the interface as `dev` — the same name is the
    // first field of the default route in /proc/net/route.
    let iface = std::fs::read_to_string("/proc/net/route")
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| {
                let mut f = l.split_whitespace();
                let (iface, dest, flags) = (f.next()?, f.next()?, f.next()?);
                (dest == "00000000" && flags == "0003").then(|| iface.to_string())
            })
        })
        .unwrap_or_default();
    (addr != "0.0.0.0").then_some((addr, iface))
}

impl NativeExt for Sys {
    fn query<'a>(
        &'a mut self,
        ctx: Ctx,
        _progress: UnboundedSender<Vec<Value>>,
    ) -> Pin<Box<dyn Future<Output = NativeOutcome> + Send + 'a>> {
        let sys = self.sys.clone();
        let disks = self.disks.clone();
        let arg = ctx.arg.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let q = arg.trim().to_lowercase();
                let mut rows: Vec<Value> = Vec::new();

                // Refreshes are gated by the same match as the rows that read
                // them — a `sys:ip` question pays no /proc walk.
                let mut sys = sys.lock().unwrap_or_else(|e| e.into_inner());
                let mut disks = disks.lock().unwrap_or_else(|e| e.into_inner());

            // ---- battery
            // The script matched `battery power charge $state` — the live
            // state is part of the haystack, so `sys:charging` hits it.
            {
                let state = std::fs::read_dir("/sys/class/power_supply")
                    .into_iter()
                    .flatten()
                    .flatten()
                    .find(|e| e.file_name().to_string_lossy().starts_with("BAT"))
                    .and_then(|e| {
                        read_trimmed(&e.path().join("status").to_string_lossy())
                    })
                    .unwrap_or_default()
                    .to_lowercase();
                if matches(&q, &format!("battery power charge {state}")) {
                    for entry in std::fs::read_dir("/sys/class/power_supply")
                        .into_iter()
                        .flatten()
                        .flatten()
                    {
                        let supply = entry.path();
                        if !entry.file_name().to_string_lossy().starts_with("BAT") {
                            continue;
                        }
                        let Some(capacity) = read_trimmed(&supply.join("capacity").to_string_lossy())
                        else {
                            continue;
                        };
                        let state =
                            read_trimmed(&supply.join("status").to_string_lossy()).unwrap_or_default();
                        let fraction = capacity.parse::<f64>().unwrap_or(0.0) / 100.0;
                        rows.push(json!({
                            "id": "battery",
                            "title": format!("{capacity}%"),
                            "subtitle": "Battery",
                            "detail": state,
                            "accessory": entry.file_name().to_string_lossy(),
                            "exec": copy_exec(&format!("{capacity}%")),
                            "score": 95000,
                            "progress": fraction,
                        }));
                        break;
                    }
                }
            }

            // ---- uptime
            if matches(&q, "uptime running since boot") {
                let secs = sysinfo::System::uptime();
                // `uptime -p` lists each nonzero unit — weeks, days, hours,
                // minutes — not just the top two.
                let mut parts: Vec<String> = Vec::new();
                for (n, unit) in [
                    (secs / 604800, "week"),
                    ((secs % 604800) / 86400, "day"),
                    ((secs % 86400) / 3600, "hour"),
                    ((secs % 3600) / 60, "minute"),
                ] {
                    if n > 0 {
                        parts.push(format!("{n} {unit}{}", if n == 1 { "" } else { "s" }));
                    }
                }
                let up = if parts.is_empty() {
                    "0 minutes".to_string()
                } else {
                    parts.join(", ")
                };
                // `uptime -s` — the boot stamp as local "YYYY-MM-DD HH:MM:SS".
                let since = jiff::Timestamp::from_second(sysinfo::System::boot_time() as i64)
                    .map(|t| {
                        t.to_zoned(jiff::tz::TimeZone::system())
                            .strftime("%Y-%m-%d %H:%M:%S")
                            .to_string()
                    })
                    .unwrap_or_else(|_| sysinfo::System::boot_time().to_string());
                rows.push(json!({
                    "id": "uptime",
                    "title": up,
                    "subtitle": "Uptime",
                    "detail": format!("since {since}"),
                    "exec": copy_exec(&up),
                    "score": 94000,
                }));
            }

            // ---- memory
            if matches(&q, "memory ram used free") {
                sys.refresh_memory();
                let total = sys.total_memory();
                let used = sys.used_memory();
                if total > 0 {
                    let percent = used as f64 / total as f64 * 100.0;
                    rows.push(json!({
                        "id": "memory",
                        "title": format!("{} of {}", human_bytes(used, true), human_bytes(total, true)),
                        "subtitle": "Memory",
                        "detail": format!("{percent:.0}% used"),
                        "exec": copy_exec(&format!("{} of {}", human_bytes(used, true), human_bytes(total, true))),
                        "score": 93000,
                        "progress": used as f64 / total as f64,
                    }));
                }
            }

            // ---- disk
            if matches(&q, "disk storage space root free") {
                disks.refresh(true);
                if let Some(disk) = disks.iter().find(|d| d.mount_point() == Path::new("/"))
                {
                    let total = disk.total_space();
                    let avail = disk.available_space();
                    let used = total.saturating_sub(avail);
                    let percent = if total > 0 {
                        used as f64 / total as f64 * 100.0
                    } else {
                        0.0
                    };
                    rows.push(json!({
                        "id": "disk",
                        "title": format!("{} free", human_bytes(avail, false)),
                        "subtitle": "Disk",
                        "detail": format!("{} of {} used", human_bytes(used, false), human_bytes(total, false)),
                        "accessory": format!("{percent:.0}%"),
                        "exec": copy_exec(&format!("{} free", human_bytes(avail, false))),
                        "score": 92000,
                        "progress": used as f64 / total.max(1) as f64,
                    }));
                }
            }

            // ---- address
            if matches(&q, "ip address network local")
                && let Some((addr, iface)) = local_address()
            {
                rows.push(json!({
                    "id": "ip",
                    "title": addr,
                    "subtitle": "Address",
                    "detail": iface,
                    "exec": copy_exec(&addr),
                    "score": 91000,
                }));
            }

            // ---- temperature: first Celsius a thermal zone offers.
            if matches(&q, "temperature heat cpu thermal") {
                let mut temp: Option<f64> = None;
                for entry in std::fs::read_dir("/sys/class/thermal")
                    .into_iter()
                    .flatten()
                    .flatten()
                {
                    let zone = entry.path();
                    if !entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("thermal_zone")
                    {
                        continue;
                    }
                    if let Some(milli) = read_trimmed(&zone.join("temp").to_string_lossy())
                        .and_then(|t| t.parse::<f64>().ok())
                    {
                        temp = Some(milli / 1000.0);
                        break;
                    }
                }
                if let Some(t) = temp {
                    let label = format!("{t:.1}°C");
                    rows.push(json!({
                        "id": "temp",
                        "title": label,
                        "subtitle": "Temperature",
                        "exec": copy_exec(&label),
                        "score": 90000,
                    }));
                }
            }

            // ---- kernel and host
            if matches(&q, "kernel version host hostname os arch") {
                if let Some(kernel) = sysinfo::System::kernel_version() {
                    rows.push(json!({
                        "id": "kernel",
                        "title": kernel,
                        "subtitle": "Kernel",
                        "detail": std::env::consts::ARCH,
                        "exec": copy_exec(&kernel),
                        "score": 89000,
                    }));
                }
                if let Some(host) = sysinfo::System::host_name() {
                    rows.push(json!({
                        "id": "host",
                        "title": host,
                        "subtitle": "Host",
                        "exec": copy_exec(&host),
                        "score": 88000,
                    }));
                }
            }

            // ---- omarchy
            if matches(&q, "omarchy version")
                && let Some(version) = crate::provider::process::probe(
                    &["omarchy", "version"],
                    std::time::Duration::from_secs(2),
                )
                .and_then(|o| o.lines().next().map(|l| l.trim().to_string()))
                .filter(|v| !v.is_empty())
            {
                rows.push(json!({
                    "id": "omarchy",
                    "title": version,
                    "subtitle": "Omarchy",
                    "exec": copy_exec(&version),
                    "score": 87000,
                }));
            }

                NativeOutcome::Rows(rows)
            })
            .await
            // A panic inside the blocking task is not "no answer" — the
            // script leg gets its own bounded try.
            .unwrap_or(NativeOutcome::Fallback)
        })
    }
}
