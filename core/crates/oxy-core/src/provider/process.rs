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
    // A login profile can block on anything — a keychain prompt, a network
    // mount — so the probe itself gets a deadline rather than a free pass,
    // and kill_on_drop reaps the child when the timeout drops the wait.
    let mut probe = Command::new("bash");
    probe
        .arg("-lc")
        .arg("env -0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    probe.process_group(0);
    let out = tokio::time::timeout(Duration::from_secs(8), probe.output()).await;
    let Ok(Ok(out)) = out else { return 0 };
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
        *LOGIN_ENV.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(vars));
    }
    n
}

/// A variable the way a script child sees it: the daemon's own environment
/// first, then the captured login environment under it. Native providers ask
/// here rather than `std::env::var` so a profile-exported `OXY_*` reaches
/// them the same reach it reaches `bash -c`.
pub(crate) fn env_or_login(name: &str) -> Option<String> {
    if let Ok(v) = std::env::var(name)
        && !v.is_empty()
    {
        return Some(v);
    }
    LOGIN_ENV
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()?
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
        .filter(|v| !v.is_empty())
}

/// `bash -c body` under the captured login environment plus the daemon's
/// plugin id. Borrows from the shared map — `envs` copies at call time, so
/// nothing here allocates past the pairs the OS needs anyway.
///
/// Every child leads its own process group: `bash -c` forks for pipelines
/// and `xargs`, and killing the bash pid alone would leave those running.
fn shell_command(body: &str) -> Command {
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(body);
    if let Some(env) = &*LOGIN_ENV.read().unwrap_or_else(|e| e.into_inner()) {
        cmd.envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    }
    cmd.env("OXY_PLUGIN_ID", &*PLUGIN_ID);
    #[cfg(unix)]
    cmd.process_group(0);
    cmd
}

/// Signal the group a `process_group(0)` child leads — shelled out, the way
/// the agent runner does it, because there is no libc dep here. `kill` on
/// PATH is POSIX; `child.kill()` remains the fallback everywhere it fails.
#[cfg(unix)]
fn signal_group(pid: u32, sig: &str) {
    let _ = std::process::Command::new("kill")
        .args([sig, &format!("-{pid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// TERM the group, give it a moment, KILL what ignored it, and still call
/// `kill()` as the fallback — a child that was never a group leader (or a
/// platform without one) dies the way it always did.
async fn kill_tree(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    {
        if child.try_wait().ok().flatten().is_none() {
            signal_group(child.id().unwrap_or(0), "-TERM");
            let deadline = std::time::Instant::now() + Duration::from_millis(500);
            while child.try_wait().ok().flatten().is_none() && std::time::Instant::now() < deadline
            {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            if child.try_wait().ok().flatten().is_none() {
                signal_group(child.id().unwrap_or(0), "-KILL");
            }
        }
    }
    let _ = child.kill().await;
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
        // Bounded memory, unbounded drain: a script that floods stdout past
        // the cap keeps being read to EOF (so it exits normally and `code`
        // is real) but only the head is kept. Past the cap a row set is
        // noise anyway.
        const CAP: usize = 4 * 1024 * 1024;
        let mut buf = Vec::with_capacity(8192);
        let mut chunk = [0u8; 8192];
        while let Ok(n) = stdout.read(&mut chunk).await {
            if n == 0 {
                break;
            }
            let room = CAP.saturating_sub(buf.len());
            buf.extend_from_slice(&chunk[..n.min(room)]);
        }
        buf
    };

    match tokio::time::timeout(timeout, read).await {
        Ok(buf) => {
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
            kill_tree(&mut child).await;
            Some(Finished {
                stdout: String::new(),
                code: None,
                timed_out: true,
            })
        }
    }
}

/// A synchronous bounded probe for callers that cannot await — a
/// `OnceLock` initializer has no async in it. `try_wait` polls instead of
/// blocking on `wait`, so a wedged child dies at the deadline rather than
/// holding the thread. The block still occupies its thread: this is for
/// once-per-process probes, never anything per-keystroke.
pub fn probe(argv: &[&str], timeout: Duration) -> Option<String> {
    let mut child = std::process::Command::new(argv.first()?)
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                use std::io::Read;
                let mut out = String::new();
                child.stdout.take()?.read_to_string(&mut out).ok()?;
                return Some(out);
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
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
            kill_tree(&mut child).await;
            false
        }
    }
}

/// Parse a script's stdout into rows: a JSON array, or one object per line.
pub fn parse(text: &str) -> Vec<Value> {
    crate::model::row::parse_rows(text)
}
