//! Running a `search` command and reading its rows — the process path every
//! JSON extension already has.

use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Run `body` through the daemon's own environment — which is the session's
/// login environment, so a `bash -c` here is what `bash -lc` was from the
/// launcher, minus the profile re-source on every keystroke.
///
/// Returns whatever stdout said before the timeout; a killed or failed run
/// answers `None`.
pub async fn run(body: &str, timeout: Duration) -> Option<String> {
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(body)
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
            // Let the exit status be collected without blocking on it.
            tokio::spawn(async move { child.wait().await.ok() });
            Some(String::from_utf8_lossy(&buf).into_owned())
        }
        _ => {
            let _ = child.kill().await;
            None
        }
    }
}

/// Spawn `body` with stdout piped, for the caller that wants the lines as
/// they arrive — `ask`'s whole point is that the answer streams. Killed on
/// drop, so a forgotten handle can never outlive the engine.
pub fn spawn_stream(body: &str) -> Option<tokio::process::Child> {
    Command::new("bash")
        .arg("-c")
        .arg(body)
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
    let _ = Command::new("bash")
        .arg("-c")
        .arg(body)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// A `when` check: true when the command exits 0 inside eight seconds.
/// `timeout(8s)` turns a wedged probe into a plain "not available".
pub async fn check(body: &str) -> bool {
    let spawned = Command::new("bash")
        .arg("-c")
        .arg(body)
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
    let child = std::process::Command::new("bash")
        .arg("-c")
        .arg(body)
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
    crate::row::parse_rows(text)
}
