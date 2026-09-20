use std::sync::Arc;

use interprocess::local_socket::tokio::Listener;
use interprocess::local_socket::traits::tokio::{Listener as _, Stream as _};
use oxy_core::engine::EngineCmd;
use oxy_core::provider::worker::Shared;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc};

use crate::{clipboard, wire};

pub(crate) async fn serve(
    listener: Listener,
    engine_cmd: mpsc::Sender<EngineCmd>,
    bcast: broadcast::Sender<Arc<[u8]>>,
    hello_src: Arc<Shared>,
) {
    loop {
        let Ok(stream) = listener.accept().await else {
            continue;
        };
        let cmd_tx = engine_cmd.clone();
        let events = bcast.subscribe();
        let hello_src = hello_src.clone();
        tokio::spawn(async move {
            let (reader, mut writer) = stream.split();
            // The hello first: a client that knows the keyword set parses the
            // first query correctly even before the registry event lands.
            let hello = wire::hello_line(&hello_src);
            if writer.write_all(hello.as_bytes()).await.is_err() {
                return;
            }
            let _ = writer.flush().await;
            serve_with(reader, &mut writer, cmd_tx, events).await;
        });
    }
}

/// `serve`, with the split already done so `hello` could be written first.
async fn serve_with<R, W>(
    reader: R,
    writer: &mut W,
    cmd_tx: mpsc::Sender<EngineCmd>,
    mut events: broadcast::Receiver<Arc<[u8]>>,
) where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { return };
                let line = line.trim();
                // One parse per line — `op` is read off the same `Value` the
                // command is built from.
                let Ok(v) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                // Once per open, never per keystroke: a URL on the clipboard
                // is the first row of an empty box.
                let is_open =
                    v.get("op").and_then(|x| x.as_str()) == Some("open");
                if let Some(cmd) = wire::parse_cmd(v) {
                    if is_open {
                        let tx = cmd_tx.clone();
                        tokio::spawn(async move {
                            if let Some(url) = clipboard::read_clipboard().await {
                                let _ = tx.send(EngineCmd::Clipboard { url: Some(url) }).await;
                            }
                        });
                    }
                    if cmd_tx.send(cmd).await.is_err() {
                        return;
                    }
                }
            }
            event = events.recv() => {
                match event {
                    // A slow client skips what it missed; a closed channel
                    // means the engine is gone and there is nothing to serve.
                    Ok(event) => {
                        if writer.write_all(&event).await.is_err() {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    }
}
