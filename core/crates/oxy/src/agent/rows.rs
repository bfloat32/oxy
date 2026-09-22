//! The rows `search` and the daemon answer with — the draft card, the empty
//! card, the policy card and the missing-agent card.

use serde_json::{Map, Value, json};

use crate::agent::{self, Agent, plan, previews};

pub fn policy_line(agent: &Agent) -> String {
    if !agent.acts {
        return format!("{} · plans, and will not act", agent.title);
    }
    format!(
        "{} · acts here; deleting and pushing need a terminal",
        agent.title
    )
}

fn base_row() -> Map<String, Value> {
    let mut row = Map::new();
    row.insert("id".into(), json!("agent"));
    row.insert("view".into(), json!("agent"));
    row.insert("group".into(), json!("Do It"));
    row.insert("turns".into(), json!([]));
    row.insert("draft".into(), json!(""));
    row.insert("hints".into(), json!([]));
    row
}

pub fn hint_row(agent: Option<&Agent>, turns: &[Value], cwd: &str) -> Map<String, Value> {
    let Some(agent) = agent else {
        return missing_row("", turns);
    };
    let mut row = base_row();
    row.insert("state".into(), json!("idle"));
    row.insert("title".into(), json!("Say what you want done"));
    row.insert("subtitle".into(), json!(policy_line(agent)));
    row.insert("agentName".into(), json!(agent.title));
    row.insert("cwd".into(), json!(agent::tilde(cwd)));
    row.insert(
        "hints".into(),
        json!(if turns.is_empty() {
            agent::EXAMPLES
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        }),
    );
    row.insert("turns".into(), json!(turns));
    row
}

/// The honest answer when there is no agent to hand this to. A `when` that
/// hid the keyword instead would make typing `do:` produce nothing at all,
/// which reads as the launcher being broken rather than as the CLI being
/// absent.
pub fn missing_row(named: &str, turns: &[Value]) -> Map<String, Value> {
    let here = agent::agents_present()
        .iter()
        .map(|a| a.title.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let (title, subtitle) = if !named.is_empty() {
        let subtitle = if !here.is_empty() {
            format!("Installed here: {here}")
        } else {
            "No agent CLI found. Install claude or codex.".to_string()
        };
        (format!("{named} is not installed"), subtitle)
    } else {
        // Not the whole truth any more: windows, workspaces, volume,
        // brightness and themes are done here and need nothing installed.
        // Only the sentences that have to be understood need an agent.
        (
            "No agent CLI found".to_string(),
            "Windows, workspaces and settings still work. Install claude or codex for the rest."
                .to_string(),
        )
    };
    let mut row = base_row();
    row.insert("state".into(), json!("missing"));
    row.insert("title".into(), json!(title));
    row.insert("subtitle".into(), json!(subtitle));
    row.insert("agentName".into(), json!(named));
    row.insert("turns".into(), json!(turns));
    row
}

/// `do: /policy` prints what this may do without asking and what it cannot.
pub fn policy_row(agent: Option<&Agent>, turns: &[Value], cwd: &str) -> Map<String, Value> {
    // `Bash(rm:*)` -> `rm` — the first colon ends the name.
    let mut denied: Vec<String> = agent::DENIED
        .iter()
        .map(|d| {
            d.strip_prefix("Bash(")
                .and_then(|rest| rest.split(':').next())
                .unwrap_or(d)
                .to_string()
        })
        .collect();
    denied.sort();
    denied.dedup();
    let mut row = base_row();
    row.insert("state".into(), json!("policy"));
    row.insert("title".into(), json!("What it may do here"));
    row.insert(
        "subtitle".into(),
        json!(agent.map(policy_line).unwrap_or_default()),
    );
    row.insert(
        "agentName".into(),
        json!(agent.map(|a| a.title.clone()).unwrap_or_default()),
    );
    row.insert("cwd".into(), json!(agent::tilde(cwd)));
    row.insert("turns".into(), json!(turns));
    row.insert("allows".into(), json!(agent::ALLOWED.join(", ")));
    row.insert("denies".into(), json!(denied.join(", ")));
    row.insert(
        "hints".into(),
        json!([
            "Anything under 'needs a terminal' comes back refused, named,",
            "and Ctrl+K re-runs the same sentence where you can approve it.",
            "Windows, workspaces and applications never reach a model:",
            "those sentences are run here, the same way every time."
        ]),
    );
    row
}

/// The two instructions that are about this keyword rather than about the
/// machine. Both are typed the way a chat takes a command, because that is
/// what the box already looks like.
pub fn special_row(
    agent: Option<&Agent>,
    instruction: &str,
    turns: &[Value],
    cwd: &str,
) -> Option<Map<String, Value>> {
    match instruction {
        "/new" | "/clear" => {
            // The row about starting over draws no transcript: there is
            // nothing to continue yet.
            let mut row = hint_row(agent, &[], cwd);
            row.insert("state".into(), json!("draft"));
            row.insert("title".into(), json!("Start over"));
            row.insert("subtitle".into(), json!("Forget what was said before this"));
            row.insert("hints".into(), json!([]));
            row.insert("exec".into(), json!("oxy-agent new"));
            row.insert("clearTo".into(), json!("do: "));
            row.insert("keepOpen".into(), json!(true));
            Some(row)
        }
        "/policy" | "/what" | "/can" => Some(policy_row(agent, turns, cwd)),
        _ => None,
    }
}

/// What typing produces. Nothing has run: this is the sentence read back,
/// with the agent and the directory named, and Enter is what starts it.
pub fn draft_row(
    agent: Option<&Agent>,
    instruction: &str,
    cwd: &str,
    token: &str,
    turns: &[Value],
    plan: Option<&plan::Plan>,
) -> Map<String, Value> {
    let mut row = base_row();
    row.insert("state".into(), json!("draft"));
    row.insert("title".into(), json!(instruction));
    row.insert(
        "subtitle".into(),
        json!(agent.map(policy_line).unwrap_or_default()),
    );
    row.insert(
        "agentName".into(),
        json!(
            agent
                .map(|a| a.title.clone())
                .unwrap_or_else(|| agent::direct_agent().title)
        ),
    );
    row.insert("cwd".into(), json!(agent::tilde(cwd)));
    row.insert("draft".into(), json!(instruction));
    row.insert("turns".into(), json!(turns));
    row.insert("exec".into(), json!(format!("oxy-agent send {token}")));
    // Enter sends and empties the box, the way a chat does. Without this the
    // sentence stayed put and Enter could be pressed again, and again, each
    // press starting another run of the same thing.
    row.insert("clearTo".into(), json!("do: "));
    row.insert("escExec".into(), json!("oxy-agent stop"));
    row.insert("keepOpen".into(), json!(true));
    row.insert(
        "actions".into(),
        json!([
            {
                "title": "Run in a terminal, where you answer the prompts",
                "subtitle": "Deleting, pushing and sudo live there",
                "exec": format!("oxy-agent term {token}"),
            },
            {
                "title": "Copy the instruction",
                "exec": format!(
                    "printf %s {} | wl-copy",
                    agent::shlex_quote(instruction)
                ),
            },
        ]),
    );
    if let Some(plan) = plan {
        row.insert("direct".into(), json!(true));
        row.insert("plan".into(), json!(plan::plan_labels(Some(plan))));
        row.insert("agentName".into(), json!(agent::direct_agent().title));
        row.insert(
            "subtitle".into(),
            json!("Done here, step by step, with no model involved"),
        );
        if agent.is_some() {
            if let Some(first) = row
                .get_mut("actions")
                .and_then(|a| a.as_array_mut())
                .and_then(|a| a.first_mut())
            {
                first["title"] = json!("Hand it to the agent in a terminal");
                first["subtitle"] = json!("For the version of this that thinks");
            }
        } else if let Some(actions) = row.get_mut("actions").and_then(|a| a.as_array_mut()) {
            actions.remove(0);
        }
    }
    row
}

/// `rows()`/`local_rows()` in the script — one body: the daemon passes the
/// live transcript and whether a run is going; `search` passes empties.
pub async fn answer(query: &str, transcript: Vec<Value>, busy: bool) -> Vec<Value> {
    let (named, instruction) = agent::split_query(query);
    let agent = agent::pick_agent(&named);
    // A sentence with one reading is answered before the agent is, because
    // it does not need one: naming an agent (`@codex open four terminals`)
    // is the way to ask for the thinking version instead.
    let plan = if named.is_empty() {
        plan::plan_for(&instruction).await
    } else {
        None
    };

    if plan.is_none() && agent.as_ref().is_none_or(|a| a.is_missing()) {
        return vec![Value::Object(missing_row(&named, &transcript))];
    }

    let cwd = agent::workdir().await;
    if let Some(special) = special_row(agent.as_ref(), &instruction, &transcript, &cwd) {
        return vec![Value::Object(special)];
    }

    if instruction.is_empty() {
        let mut row = hint_row(agent.as_ref(), &transcript, &cwd);
        if busy {
            row.insert("escExec".into(), json!("oxy-agent stop"));
        }
        return vec![Value::Object(row)];
    }

    let who = if plan.is_some() {
        agent::direct_agent().id
    } else {
        agent.as_ref().map(|a| a.id.clone()).unwrap_or_default()
    };
    let token = agent::token_for(&who, &instruction, &cwd);
    previews::remember(
        &token,
        json!({
            "agent": who,
            "instruction": instruction,
            "cwd": cwd,
            "at": agent::now() as i64,
        }),
    );
    vec![Value::Object(draft_row(
        agent.as_ref(),
        &instruction,
        &cwd,
        &token,
        &transcript,
        plan.as_ref(),
    ))]
}
