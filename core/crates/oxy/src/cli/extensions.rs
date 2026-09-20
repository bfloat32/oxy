use std::io::Write;

use oxy_core::{registry as extension, settings::paths as dirs};
use serde_json::{Value, json};

pub(crate) async fn run() -> i32 {
    let settings = oxy_core::settings::Settings::load(&dirs::settings_file());
    let report = extension::load_dir(&dirs::extensions_dir(), &settings.extensions);
    let mut out = Vec::new();
    for ext in &report.extensions {
        out.push(json!({
            "id": ext.id,
            "keyword": ext.keyword,
            "title": ext.title,
            "aliases": ext.aliases,
            "view": ext.view,
            "tier": oxy_core::support::rank::tier_name(ext.tier),
            "native": ext.native,
            "search": ext.search,
            "socket": ext.socket,
            "when": ext.when,
        }));
    }
    for (path, why) in &report.bad {
        out.push(json!({"bad": path.to_string_lossy(), "why": why}));
    }
    let stdout = std::io::stdout();
    let mut out_w = stdout.lock();
    let _ = writeln!(
        out_w,
        "{}",
        serde_json::to_string_pretty(&Value::Array(out)).unwrap_or_default()
    );
    0
}
