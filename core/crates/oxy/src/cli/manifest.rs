//! `oxy test --only manifest`: the checks a manifest earns before it ships.
//!
//! The launcher reads a manifest leniently on purpose — an unknown `view`
//! falls back to `list`, an unknown `tier` to `substring`, a string where a
//! number belongs to the default — because a user's config should degrade
//! rather than fail. That leniency is why a check exists: every one of those
//! fallbacks is silent, and one of them (`native:`) is worse than silent —
//! a name that is not an arm in `construct` means the port never runs and the
//! script answers instead, which looks exactly like success.
//!
//! Read-only: nothing here writes, and nothing here changes what the launcher
//! does with a manifest.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::Value;

use oxy_core::registry::known_view;
use oxy_core::settings::paths as dirs;
use oxy_core::support::rank;

/// The numbers a manifest may carry, and what each must be.
const NUMBERS: &[&str] = &[
    "minChars",
    "debounceMs",
    "timeoutMs",
    "maxRows",
    "refreshMs",
    "cacheMs",
];

pub(crate) async fn run(only: Option<&str>, json: bool) -> i32 {
    let dir = dirs::extensions_dir();
    let Ok(read) = std::fs::read_dir(&dir) else {
        eprintln!("oxy test: no extensions directory at {}", dir.display());
        return 1;
    };

    let mut files: Vec<PathBuf> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("json")
                && !p.to_string_lossy().ends_with(".cases.json")
        })
        .collect();
    files.sort();

    let mut checked = 0usize;
    let mut failed = 0usize;
    let mut ids: HashMap<String, String> = HashMap::new();
    let mut keywords: HashMap<String, String> = HashMap::new();
    let mut lines: Vec<String> = Vec::new();
    let mut problems_out: Vec<serde_json::Value> = Vec::new();

    for path in files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        // Counted before anything can go wrong: the summary says how many
        // files were read, not how many happened to parse into objects.
        checked += 1;
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                lines.push(format!("FAIL {name}: unreadable ({e})"));
                problems_out.push(
                    serde_json::json!({ "file": name, "problem": format!("unreadable ({e})") }),
                );
                failed += 1;
                continue;
            }
        };
        let value: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                lines.push(format!("FAIL {name}: {e}"));
                problems_out.push(serde_json::json!({ "file": name, "problem": e.to_string() }));
                failed += 1;
                continue;
            }
        };
        let Some(obj) = value.as_object() else {
            lines.push(format!("FAIL {name}: not a JSON object"));
            problems_out.push(serde_json::json!({ "file": name, "problem": "not a JSON object" }));
            failed += 1;
            continue;
        };

        let id = obj.get("id").and_then(Value::as_str).unwrap_or("");
        // The filter takes the id or the file's own name: `windows.json`
        // holds `"id": "win"`, and both are names a person would type.
        let stem = name.strip_suffix(".json").unwrap_or(&name);
        if let Some(only) = only
            && id != only
            && stem != only
        {
            continue;
        }

        let mut problems = Vec::new();
        if id.is_empty() {
            problems.push("no id".to_string());
        } else if let Some(other) = ids.insert(id.to_string(), name.clone()) {
            problems.push(format!("duplicate id, also in {other}"));
        }

        match obj.get("keyword").and_then(Value::as_str) {
            Some(kw) if !kw.is_empty() => {
                if let Some(other) = keywords.insert(kw.to_string(), name.clone()) {
                    problems.push(format!("duplicate keyword '{kw}', also in {other}"));
                }
            }
            _ => problems.push("no keyword".to_string()),
        }

        let has_search = obj
            .get("search")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
        let has_socket = obj
            .get("socket")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
        if !has_search && !has_socket {
            problems.push("neither search nor socket".to_string());
        }

        // A field that is there but the wrong type is a problem, not a
        // silent default: the launcher reads `view: 5` as `list`, which is
        // exactly the fallback this check exists to catch.
        match obj.get("view") {
            None | Some(Value::Null) => {}
            Some(Value::String(v)) if v.is_empty() || known_view(v) => {}
            Some(Value::String(v)) => problems.push(format!("unknown view '{v}'")),
            Some(_) => problems.push("view is not a string".to_string()),
        }

        match obj.get("tier") {
            None | Some(Value::Null) => {}
            Some(Value::String(v)) if v.is_empty() || rank::tier_name(rank::tier(v)) == v => {}
            Some(Value::String(v)) => problems.push(format!("unknown tier '{v}'")),
            Some(_) => problems.push("tier is not a string".to_string()),
        }

        for key in NUMBERS {
            if let Some(v) = obj.get(*key)
                && !v.is_u64()
            {
                problems.push(format!("{key} is not a whole number"));
            }
        }

        match obj.get("native") {
            None | Some(Value::Null) => {}
            Some(Value::String(n)) if n.is_empty() => {}
            Some(Value::String(n)) if oxy_core::provider::native::construct(n).is_none() => {
                problems.push(format!(
                    "native '{n}' is not an arm in native::construct — the port would never run"
                ));
            }
            Some(Value::String(_)) => {}
            Some(_) => problems.push("native is not a string".to_string()),
        }

        if let Some(aliases) = obj.get("aliases")
            && !aliases
                .as_array()
                .is_some_and(|a| a.iter().all(Value::is_string))
        {
            problems.push("aliases is not an array of strings".to_string());
        }

        if let Some(when) = obj.get("when")
            && !when.is_string()
        {
            problems.push("when is not a string".to_string());
        }

        if problems.is_empty() {
            lines.push(format!("ok   {name}"));
        } else {
            failed += 1;
            for problem in problems {
                lines.push(format!("FAIL {name}: {problem}"));
                problems_out.push(serde_json::json!({ "file": name, "problem": problem }));
            }
        }
    }

    if let Some(only) = only
        && checked == 0
    {
        eprintln!("oxy test: no extension '{only}'");
        return 1;
    }
    if checked == 0 {
        eprintln!("oxy test: no manifests under {}", dir.display());
        return 1;
    }
    if json {
        let summary = serde_json::json!({
            "checked": checked,
            "failed": failed,
            "problems": problems_out,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&summary).unwrap_or_default()
        );
    } else {
        for line in &lines {
            println!("{line}");
        }
        println!("{checked} checked, {failed} failed");
    }
    i32::from(failed > 0)
}
