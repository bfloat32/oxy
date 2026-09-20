//! Running a `search` command and reading its rows — the process path every
//! JSON extension already has.

use std::process::Stdio;
use std::sync::{Arc, LazyLock, RwLock};
use std::time::Duration;

use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// `KEY=VALUE` pairs as the kernel wants them handed to a child.
type EnvPairs = Arc<Vec<(String, String)>>;

/// The login shell's environment, captured once and replayed onto every
/// spawned command — the port of the QML's `bash -lc env -0` probe. A daemon
/// spawned by the shell carries the *session's* environment, not the login
/// shell's: profile exports (mise/nix/cargo PATH entries, `OXY_REPO_ROOTS`)
/// never reach scripts without this. While it is absent, commands inherit
/// the daemon's environment — never worse than the probe not existing.
static LOGIN_ENV: RwLock<Option<EnvPairs>> = RwLock::new(None);

/// The plugin id this daemon's launcher registers — exported to every
/// command so a script that summons the launcher back (a row that closes
/// the window and reopens it on a query) returns to the build that asked.
/// The script build leaves the variable unset, so a script that needs the
/// id falls back to `oma.oxy`.
static PLUGIN_ID: LazyLock<String> = LazyLock::new(|| {
    std::env::var("OXY_PLUGIN_ID")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "oma.oxyrs".to_string())
});

/// Run `bash -lc env -0` once — the daemon calls this at start, not per
/// command. Returns how many variables landed, for the `env` log line.
pub async fn capture_login_env() -> usize {
    let out = Command::new("bash")
        .arg("-lc")
        .arg("env -0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await;
    let Ok(out) = out else { return 0 };
    let text = String::from_utf8_lossy(&out.stdout);
    let vars: Vec<(String, String)> = text
        .split('\0')
        .filter_map(|kv| {
            // Only K=V with a sane K: exported shell functions arrive as
            // BASH_FUNC_x%%=() { ... } and a stray byte is not a variable —
            // the same filter the QML ran.
            let (k, v) = kv.split_once('=')?;
            let mut b = k.bytes();
            if matches!(b.next(), Some(c) if c.is_ascii_alphabetic() || c == b'_')
                && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
            {
                Some((k.to_string(), v.to_string()))
            } else {
                None
            }
        })
        .collect();
    let n = vars.len();
    if n > 0 {
        *LOGIN_ENV.write().unwrap() = Some(Arc::new(vars));
    }
    n
}

/// `bash -c body` under the captured login environment plus the daemon's
/// plugin id. Borrows from the shared map — `envs` copies at call time, so
/// nothing here allocates past the pairs the OS needs anyway.
fn shell_command(body: &str) -> Command {
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(body);
    if let Some(env) = &*LOGIN_ENV.read().unwrap() {
        cmd.envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    }
    cmd.env("OXY_PLUGIN_ID", &*PLUGIN_ID);
    cmd
}

/// The same environment for the blocking `std::process::Command` callers.
fn shell_command_sync(body: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new("bash");
    cmd.arg("-c").arg(body);
    if let Some(env) = &*LOGIN_ENV.read().unwrap() {
        cmd.envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    }
    cmd.env("OXY_PLUGIN_ID", &*PLUGIN_ID);
    cmd
}

/// What a run left behind: stdout, the exit code when the wait returned one,
/// and whether the deadline killed it — the `code`/`timeout` fields the
/// event log reports, which `Option<String>` could not carry.
pub struct Finished {
    pub stdout: String,
    pub code: Option<i32>,
    pub timed_out: bool,
}

/// Returns whatever stdout said before the timeout; a killed run answers
/// `Some(timed_out)`, a command that never spawned answers `None`.
pub async fn run(body: &str, timeout: Duration) -> Option<Finished> {
    let mut child = shell_command(body)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;

    let mut stdout = child.stdout.take()?;
    let read = async move {
        let mut buf = Vec::with_capacity(8192);
        stdout.read_to_end(&mut buf).await.ok().map(|_| buf)
    };

    match tokio::time::timeout(timeout, read).await {
        Ok(Some(buf)) => {
            // The read finished at EOF, so the child has already exited or is
            // about to — `wait` returns promptly and the code is real.
            let code = child.wait().await.ok().and_then(|s| s.code());
            Some(Finished {
                stdout: String::from_utf8_lossy(&buf).into_owned(),
                code,
                timed_out: false,
            })
        }
        _ => {
            let _ = child.kill().await;
            Some(Finished {
                stdout: String::new(),
                code: None,
                timed_out: true,
            })
        }
    }
}

/// Spawn `body` with stdout piped, for the caller that wants the lines as
/// they arrive — `ask`'s whole point is that the answer streams. Killed on
/// drop, so a forgotten handle can never outlive the engine.
pub fn spawn_stream(body: &str) -> Option<tokio::process::Child> {
    shell_command(body)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()
}

/// Run a detached command — fire and forget, nothing read back. What Enter
/// does to a row's `exec`.
pub fn run_detached(body: &str) {
    let _ = shell_command(body)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// A `when` check: true when the command exits 0 inside eight seconds.
/// `timeout(8s)` turns a wedged probe into a plain "not available".
pub async fn check(body: &str) -> bool {
    let spawned = shell_command(body)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let Ok(mut child) = spawned else { return false };
    match tokio::time::timeout(Duration::from_secs(8), child.wait()).await {
        Ok(Ok(status)) => status.success(),
        _ => {
            let _ = child.kill().await;
            false
        }
    }
}

/// Command for the availability probe at load and for `oxy test` preflight.
pub fn run_sync(body: &str, timeout: Duration) -> Option<String> {
    let child = shell_command_sync(body)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = std::time::Instant::now();
    let mut out = child;
    loop {
        match out.try_wait() {
            Ok(Some(_)) => {
                let mut buf = Vec::new();
                use std::io::Read;
                out.stdout.take()?.read_to_end(&mut buf).ok()?;
                return Some(String::from_utf8_lossy(&buf).into_owned());
            }
            Ok(None) if start.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(5));
            }
            _ => {
                let _ = out.kill();
                return None;
            }
        }
    }
}

/// Parse a script's stdout into rows: a JSON array, or one object per line.
pub fn parse(text: &str) -> Vec<Value> {
    crate::model::row::parse_rows(text)
}
