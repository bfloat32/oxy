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
    let (eof_tx, mut eof_rx) = tokio::sync::watch::channel(());
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
                break;
            }
        }
        let _ = eof_tx.send(());
    });

    // While stdin is open the session is the user's — no deadline: a daemon
    // that goes quiet is theirs to wait on or interrupt, the way netcat
    // leaves an idle socket alone. Once stdin closes the grace clock arms,
    // and GRACE is how long the last command's events get to arrive.
    const GRACE: Duration = Duration::from_secs(30);
    loop {
        let next = tokio::select! {
            biased;
            _ = eof_rx.changed() => None,
            line = out_lines.next_line() => Some(line),
        };
        match next {
            Some(Ok(Some(line))) => {
                println!("{line}");
                let _ = std::io::Write::flush(&mut std::io::stdout());
            }
            // The daemon hung up — done, whatever stdin had left to say.
            Some(Ok(None)) | Some(Err(_)) => break,
            // stdin closed: from here every read is inside the grace window.
            None => {
                while let Ok(Ok(Some(line))) =
                    tokio::time::timeout(GRACE, out_lines.next_line()).await
                {
                    println!("{line}");
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                }
                break;
            }
        }
    }
    0
}
