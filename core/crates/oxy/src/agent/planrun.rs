//! The `plan` subcommand — the deterministic half of Enter. Same token, same
//! promise as `send`: it can only run what was previewed, and what it runs is
//! the list the card drew before anybody pressed anything. Its stdout is the
//! `step`/`step_end`/`result` event stream the daemon's Run reads.

use std::io::Write;
use std::time::Duration;

use serde_json::json;

use crate::agent::{self, desk, plan, previews};

fn emit(event: serde_json::Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{event}");
    let _ = out.flush();
}

/// One step, with its own output caught. The desk verbs print for a person
/// reading a terminal; here the only thing on stdout is the event stream the
/// card is reading, so what they say is captured and reported as the step's
/// own result. Returns `(rc, out, err)`.
async fn run_step(step: &plan::Step) -> (i32, String, String) {
    match &step.kind {
        plan::StepKind::Run(run) => {
            if desk::dry_run() {
                return (0, format!("would run: {}", run.join(" ")), String::new());
            }
            let p = tokio::time::timeout(
                Duration::from_secs(90),
                tokio::process::Command::new(&run[0])
                    .args(&run[1..])
                    .stdin(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .output(),
            )
            .await;
            match p {
                Ok(Ok(o)) => (
                    o.status.code().unwrap_or(1),
                    String::from_utf8_lossy(&o.stdout).trim().to_string(),
                    String::from_utf8_lossy(&o.stderr).trim().to_string(),
                ),
                Ok(Err(e)) => (1, String::new(), e.to_string()),
                Err(_) => (1, String::new(), "timed out".to_string()),
            }
        }
        plan::StepKind::Mute(want) => {
            // Said twice, this leaves the speakers where the sentence asked
            // for them: the toggle underneath is only reached when the state
            // is not already the one that was asked for.
            if desk::dry_run() {
                return (0, format!("would set muted={want}"), String::new());
            }
            if desk::muted_now().await == Some(*want) {
                return (0, "already".to_string(), String::new());
            }
            let p = tokio::time::timeout(
                Duration::from_secs(30),
                tokio::process::Command::new("omarchy-audio-output-volume")
                    .arg("mute-toggle")
                    .stdin(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .output(),
            )
            .await;
            match p {
                Ok(Ok(o)) => (
                    o.status.code().unwrap_or(1),
                    String::from_utf8_lossy(&o.stdout).trim().to_string(),
                    String::from_utf8_lossy(&o.stderr).trim().to_string(),
                ),
                Ok(Err(e)) => (1, String::new(), e.to_string()),
                Err(_) => (1, String::new(), "timed out".to_string()),
            }
        }
        plan::StepKind::Argv(args) => desk::desk_capture(args).await,
        plan::StepKind::Tile { split, app } => desk::tile_capture(*split, app).await,
    }
}

pub async fn cmd_plan(token: &str) -> i32 {
    let Some(entry) = previews::read().get(token).cloned() else {
        return 2;
    };
    let instruction = entry
        .get("instruction")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let Some(plan) = plan::plan_for(instruction).await else {
        return 2;
    };
    let cwd = entry
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| {
            oxy_core::settings::paths::home()
                .to_string_lossy()
                .into_owned()
        });
    let _ = std::env::set_current_dir(&cwd);

    let steps = &plan.steps;
    for (i, step) in steps.iter().enumerate() {
        emit(json!({"type": "step", "text": step.say}));
        let (rc, _said, why) = run_step(step).await;
        emit(json!({"type": "step_end", "ok": rc == 0, "why": agent::short(&why, 200)}));
        if rc != 0 {
            // Half a plan is still worth reporting: the windows that opened
            // are on screen whether or not this says so, and a card that ends
            // on "failed" with no count leaves somebody counting by hand.
            let why = agent::short(&why, 120);
            emit(json!({
                "type": "result",
                "result": format!("stopped after {} of {}: {}", i, steps.len(),
                    if why.is_empty() { step.say.as_str() } else { &why }),
            }));
            return 1;
        }
    }
    let ws = desk::active_workspace()
        .await
        .map(|w| w.to_string())
        .unwrap_or_else(|| "None".into());
    emit(json!({
        "type": "result",
        "result": plan.done.replace("{ws}", &ws),
    }));
    0
}
