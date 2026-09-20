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

fn human_bytes(v: u64) -> String {
    let units = ["B", "K", "M", "G", "T", "P"];
    let mut v = v as f64;
    let mut i = 0;
    while v >= 1000.0 && i < units.len() - 1 {
        v /= 1000.0;
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
    (addr != "0.0.0.0").then_some((addr, String::new()))
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
                let mut sys = sys.lock().unwrap();
                let mut disks = disks.lock().unwrap();

            // ---- battery
            if matches(&q, "battery power charge") {
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

            // ---- uptime
            if matches(&q, "uptime running since boot") {
                let secs = sysinfo::System::uptime();
                let up = if secs >= 86400 {
                    format!("{} days, {} hours", secs / 86400, (secs % 86400) / 3600)
                } else if secs >= 3600 {
                    format!("{} hours, {} minutes", secs / 3600, (secs % 3600) / 60)
                } else {
                    format!("{} minutes", secs / 60)
                };
                let since = sysinfo::System::boot_time();
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
                        "title": format!("{} of {}", human_bytes(used), human_bytes(total)),
                        "subtitle": "Memory",
                        "detail": format!("{percent:.0}% used"),
                        "exec": copy_exec(&format!("{} of {}", human_bytes(used), human_bytes(total))),
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
                        "title": format!("{} free", human_bytes(avail)),
                        "subtitle": "Disk",
                        "detail": format!("{} of {} used", human_bytes(used), human_bytes(total)),
                        "accessory": format!("{percent:.0}%"),
                        "exec": copy_exec(&format!("{} free", human_bytes(avail))),
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
                && let Some(version) = std::process::Command::new("omarchy")
                    .arg("version")
                    .output()
                    .ok()
                    .and_then(|o| {
                        String::from_utf8_lossy(&o.stdout)
                            .lines()
                            .next()
                            .map(|l| l.trim().to_string())
                    })
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
            .unwrap_or(NativeOutcome::Empty)
        })
    }
}
