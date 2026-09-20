//! `oxy ask doctor`: why is Ctrl+Enter not answering?
//!
//! Three tiers, in jcode's shape, because the useful thing about a
//! diagnostic is being able to run the cheap half of it without spending
//! anything:
//!
//! - `offline` (the default) — the wiring only: does `oxy.json` parse, is
//!   there an endpoint, is it a URL this client will actually connect to,
//!   does the request it would send make sense. No socket is opened.
//! - `catalog` — the same, plus a connect and a `GET /v1/models`: is anything
//!   listening, and does the server have the model that is configured.
//! - `full` — not implemented yet: it needs a real streamed turn, which is
//!   what the ask card itself does. The tier is accepted so the shape is
//!   honest and the message says so.
//!
//! A checkpoint is `PASS`, `FAIL`, or `skip` — the last one for work a lighter
//! tier deliberately does not do, so nothing is over-credited. The verdict
//! names the first failure with a next step, and the exit code is non-zero
//! when the chosen tier did not fully pass, which is what makes it usable in
//! CI.

use std::io::Write;

use oxy_core::provider::llm::models::{has_model, parse_models};
use oxy_core::provider::llm::{Local, http};
use oxy_core::settings::paths as dirs;
use serde_json::{Value, json};

/// The tier names, longest first so a prefix cannot shadow one.
const TIERS: &[&str] = &["offline", "catalog", "full"];

pub(crate) async fn run(args: &[String]) -> i32 {
    let mut tier = "offline".to_string();
    let mut json_out = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--tier" => {
                if let Some(t) = args.get(i + 1) {
                    tier = t.clone();
                }
                i += 2;
                continue;
            }
            "--json" => json_out = true,
            _ => {}
        }
        i += 1;
    }
    if !TIERS.contains(&tier.as_str()) {
        eprintln!("oxy ask doctor: --tier must be one of {}", TIERS.join(", "));
        return 2;
    }

    let mut checks: Vec<Value> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    let mut first_failure: Option<String> = None;

    let pass = |checks: &mut Vec<Value>, lines: &mut Vec<String>, name: &str, detail: &str| {
        checks.push(json!({"name": name, "state": "pass", "detail": detail}));
        lines.push(format!("[ PASS] {name:<38} {detail}"));
    };
    let skip = |checks: &mut Vec<Value>, lines: &mut Vec<String>, name: &str, why: &str| {
        checks.push(json!({"name": name, "state": "skip", "detail": why}));
        lines.push(format!("[ skip] {name:<38} {why}"));
    };
    let fail = |checks: &mut Vec<Value>,
                lines: &mut Vec<String>,
                name: &str,
                detail: &str,
                next: &str,
                first: &mut Option<String>| {
        checks.push(json!({"name": name, "state": "fail", "detail": detail, "next": next}));
        lines.push(format!("[ FAIL] {name:<38} {detail}"));
        lines.push(format!("         next: {next}"));
        if first.is_none() {
            *first = Some(format!("{name}: {detail}"));
        }
    };

    // ---- offline: the wiring, without opening anything
    let settings_path = dirs::settings_file();
    let settings = oxy_core::settings::Settings::load(&settings_path);
    if settings.recovered.is_empty() {
        pass(
            &mut checks,
            &mut lines,
            "settings_parsed",
            &format!("{}", settings_path.display()),
        );
    } else {
        fail(
            &mut checks,
            &mut lines,
            "settings_parsed",
            "oxy.json did not parse and was moved aside",
            "fix the file it left beside it, or write a new one",
            &mut first_failure,
        );
    }

    let ask = &settings.ask;
    let local = Local::from_settings(ask);
    match &local {
        None if ask.endpoint.trim().is_empty() => {
            skip(
                &mut checks,
                &mut lines,
                "endpoint_configured",
                "no ask.endpoint — the CLI list answers instead",
            );
        }
        None => {
            fail(
                &mut checks,
                &mut lines,
                "endpoint_configured",
                &format!(
                    "'{}' is not an http:// URL this client will use",
                    ask.endpoint
                ),
                "use a plain http:// URL on loopback, e.g. http://127.0.0.1:11434/v1/chat/completions",
                &mut first_failure,
            );
        }
        Some(local) => {
            pass(
                &mut checks,
                &mut lines,
                "endpoint_configured",
                &format!("{} ({})", local.url.authority(), model_of(local)),
            );
        }
    }

    if let Some(local) = &local {
        if local.key.is_empty() {
            skip(
                &mut checks,
                &mut lines,
                "key_resolved",
                "no ask.key — no header is sent",
            );
        } else {
            pass(
                &mut checks,
                &mut lines,
                "key_resolved",
                &format!("{} bytes", local.key.len()),
            );
        }
        let body = local.chat_request(&[], "ping");
        if body.contains("\"messages\"") && body.contains("\"stream\":true") {
            pass(
                &mut checks,
                &mut lines,
                "request_shape",
                &format!("{} bytes", body.len()),
            );
        } else {
            fail(
                &mut checks,
                &mut lines,
                "request_shape",
                "the request it would send is missing messages or stream",
                "this is a bug in oxy, not in your config",
                &mut first_failure,
            );
        }
    } else {
        skip(
            &mut checks,
            &mut lines,
            "request_shape",
            "no endpoint to build one for",
        );
    }

    // ---- catalog: reachable, and does it have the model
    let Some(local) = &local else {
        let verdict =
            "no endpoint configured: the CLI list answers, and the doctor has nothing to check";
        return report(json_out, &checks, &lines, first_failure, verdict);
    };

    if tier == "offline" {
        skip(
            &mut checks,
            &mut lines,
            "server_reachable",
            "offline tier: run --tier catalog to connect",
        );
        skip(
            &mut checks,
            &mut lines,
            "model_listed",
            "offline tier: run --tier catalog to ask for the list",
        );
    } else {
        if local.probe().await {
            pass(
                &mut checks,
                &mut lines,
                "server_reachable",
                &format!("connected to {}", local.url.authority()),
            );
        } else {
            fail(
                &mut checks,
                &mut lines,
                "server_reachable",
                &format!("nothing listening on {}", local.url.authority()),
                "start the model server (ollama serve, or LM Studio's local server)",
                &mut first_failure,
            );
        }

        if first_failure.is_none() {
            let mut models_url = local.url.clone();
            models_url.path = "/v1/models".to_string();
            let mut headers: Vec<(&str, String)> = Vec::new();
            if !local.key.is_empty() {
                headers.push(("Authorization", format!("Bearer {}", local.key)));
            }
            let headers: Vec<(&str, &str)> =
                headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
            match http::get_json(&models_url, &headers).await {
                Ok(mut response) if response.status == 200 => {
                    let mut text = String::new();
                    while let Ok(Some(line)) = response.next_line().await {
                        text.push_str(&line);
                        text.push('\n');
                    }
                    let models = parse_models(&text);
                    if models.is_empty() {
                        fail(
                            &mut checks,
                            &mut lines,
                            "model_listed",
                            "the server listed no models",
                            "pull one (ollama pull llama3.2), or check the server's own log",
                            &mut first_failure,
                        );
                    } else if local.model.is_empty() || has_model(&models, &local.model) {
                        pass(
                            &mut checks,
                            &mut lines,
                            "model_listed",
                            &format!("{} model(s), {}", models.len(), model_of(local)),
                        );
                    } else {
                        fail(
                            &mut checks,
                            &mut lines,
                            "model_listed",
                            &format!("'{}' is not in the server's list", local.model),
                            &format!("set ask.model to one of: {}", models.join(", ")),
                            &mut first_failure,
                        );
                    }
                }
                Ok(response) => {
                    // The status says what to do about it: a rejected key and
                    // a wrong path are different problems, and a doctor that
                    // gives one hint for both sends people the wrong way.
                    let (detail, next) = match response.status {
                        401 | 403 => (
                            format!(
                                "/v1/models answered {} — the key was rejected",
                                response.status
                            ),
                            "check ask.key: a literal, or env:NAME to read a variable",
                        ),
                        404 => (
                            "/v1/models answered 404".to_string(),
                            "the model list lives at /v1/models; some servers put it elsewhere",
                        ),
                        other => (
                            format!("/v1/models answered {other}"),
                            "check the server's own log",
                        ),
                    };
                    fail(
                        &mut checks,
                        &mut lines,
                        "model_listed",
                        &detail,
                        next,
                        &mut first_failure,
                    );
                }
                Err(e) => {
                    fail(
                        &mut checks,
                        &mut lines,
                        "model_listed",
                        &format!("/v1/models failed: {e}"),
                        "check the endpoint's path — the model list lives at /v1/models",
                        &mut first_failure,
                    );
                }
            }
        } else {
            skip(
                &mut checks,
                &mut lines,
                "model_listed",
                "the server was not reachable",
            );
        }
    }

    if tier == "full" {
        skip(
            &mut checks,
            &mut lines,
            "stream_completed",
            "not implemented: run Ctrl+Enter, which does exactly this",
        );
    }

    let verdict = match &first_failure {
        Some(first) => format!("tier `{tier}` failed at {first}"),
        None if tier == "offline" => {
            "offline wiring is sound. Run --tier catalog to check the server.".to_string()
        }
        None => format!("tier `{tier}` passed"),
    };
    report(json_out, &checks, &lines, first_failure, &verdict)
}

fn model_of(local: &Local) -> String {
    if local.model.is_empty() {
        "model not set".to_string()
    } else {
        format!("model '{}'", local.model)
    }
}

/// Print what was collected and turn it into an exit code: the chosen tier
/// fully passing is 0, anything else is 1.
fn report(
    json_out: bool,
    checks: &[Value],
    lines: &[String],
    first_failure: Option<String>,
    verdict: &str,
) -> i32 {
    if json_out {
        let out = json!({
            "checks": checks,
            "verdict": verdict,
            "passed": first_failure.is_none(),
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    } else {
        let stdout = std::io::stdout();
        let mut w = stdout.lock();
        for line in lines {
            let _ = writeln!(w, "{line}");
        }
        let _ = writeln!(w, "Verdict: {verdict}");
    }
    i32::from(first_failure.is_some())
}
