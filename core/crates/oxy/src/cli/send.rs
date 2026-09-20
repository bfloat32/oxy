use std::time::Duration;

use crate::cli::connect_daemon;

/// A raw client for the daemon: commands from stdin, events to stdout, one
/// JSON line each way — the same wire `Shell.qml` speaks, for debugging the
/// protocol and for integrators poking at it.
pub(crate) async fn run() -> i32 {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let stream = match connect_daemon().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("oxy send: {e} (is oxyd running?)");
            return 1;
        }
    };
    let (reader, mut writer) = tokio::io::split(stream);
    let mut out_lines = BufReader::new(reader).lines();

    // Forward stdin lines to the socket; when stdin closes, keep reading for
    // a moment so the last command's events still print.
    tokio::spawn(async move {
        let stdin = tokio::io::stdin();
        let mut in_lines = BufReader::new(stdin).lines();
        while let Ok(Some(line)) = in_lines.next_line().await {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if writer.write_all(line.as_bytes()).await.is_err()
                || writer.write_all(b"\n").await.is_err()
                || writer.flush().await.is_err()
            {
                return;
            }
        }
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while let Ok(Ok(Some(line))) =
        tokio::time::timeout_at(deadline.into(), out_lines.next_line()).await
    {
        println!("{line}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
    0
}
