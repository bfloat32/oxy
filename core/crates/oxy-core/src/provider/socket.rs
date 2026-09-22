//! A long-running program answers over a local socket instead of being
//! started per keystroke — the `socket` field's protocol, ported:
//!
//! ```text
//! → {"epoch": 12, "query": "text", "filters": {"year": "1959"}}\n
//! ← {"epoch": 12, "rows": [ ... ]}\n
//! ```
//!
//! One JSON line out per question, one or more back — a daemon may push again
//! for the same epoch whenever its answer moves, which is how `do:` streams
//! its card without the launcher polling — and the connection outlives the
//! launcher being closed.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::provider::worker::WorkerMsg;

#[cfg(unix)]
use interprocess::local_socket::{GenericFilePath, ToFsName, tokio::prelude::*};
#[cfg(windows)]
use interprocess::local_socket::{GenericNamespaced, ToNsName, tokio::prelude::*};

type Pipe = interprocess::local_socket::tokio::Stream;

fn name_for(path: &str) -> std::io::Result<interprocess::local_socket::Name<'static>> {
    #[cfg(unix)]
    {
        path.to_string().to_fs_name::<GenericFilePath>()
    }
    #[cfg(windows)]
    {
        // Extension sockets are a Unix feature, but if one is ever bound
        // here the name must still be per-extension — one shared pipe name
        // would cross-talk every socket provider into the same listener.
        // `DefaultHasher::new` is fixed-seed SipHash: stable across runs.
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        path.hash(&mut h);
        format!("oxy-ext-{:016x}", h.finish()).to_ns_name::<GenericNamespaced>()
    }
}

async fn connect_pipe(path: &str) -> std::io::Result<Pipe> {
    Pipe::connect(name_for(path)?).await
}

/// A question for the daemon on the other end.
pub struct SocketReq {
    pub epoch: u64,
    pub arg: String,
    pub filters: std::sync::Arc<BTreeMap<String, String>>,
}

/// One `{epoch, rows}` line the daemon sent — an answer, or a later push
/// refining one.
pub struct SocketPush {
    pub epoch: u64,
    pub rows: Vec<Value>,
}

/// The connection as an actor: questions in on one channel, every line the
/// daemon writes out on another. Reading continuously — not only while a
/// question is owed — is what makes a push between questions land: the old
/// request/response shape would have left them in the buffer until the next
/// ask, where they arrived as answers to a question they did not answer.
pub struct SocketChan {
    pub req: mpsc::UnboundedSender<SocketReq>,
    /// Bounded: a flooding daemon queues at most 64 answers before the
    /// newest ones start dropping — a push is a state refresh, and the one
    /// after it carries the same state.
    pub push: mpsc::Receiver<SocketPush>,
}

/// Connect and spawn the actor, bounded by `timeout`. The task exits when
/// the daemon hangs up or the request channel closes; the worker sees the
/// push channel close and reconnects on the next ask, throttled the way the
/// QML was.
///
/// `log` is the worker's id and the engine's message channel: a line that is
/// not JSON lands as a `sock.bad` event — the diagnostic that used to be a
/// silent `continue`.
pub async fn connect(
    path: &str,
    timeout: Duration,
    log: Option<(Arc<str>, mpsc::Sender<WorkerMsg>)>,
) -> std::io::Result<SocketChan> {
    let stream = tokio::time::timeout(timeout, connect_pipe(path))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timed out"))??;

    let (req_tx, mut req_rx) = mpsc::unbounded_channel::<SocketReq>();
    let (push_tx, push_rx) = mpsc::channel::<SocketPush>(64);
    // Serialized requests for the writer task — bounded, so a peer that has
    // stopped reading cannot stall the reader behind a full pipe.
    let (out_tx, mut out_rx) = mpsc::channel::<String>(64);

    tokio::spawn(async move {
        let (reader, mut writer) = tokio::io::split(stream);
        // The write half is its own task: a peer that stops reading would
        // otherwise freeze this select behind a write_all that never ends.
        tokio::spawn(async move {
            while let Some(line) = out_rx.recv().await {
                if writer.write_all(line.as_bytes()).await.is_err() || writer.flush().await.is_err()
                {
                    return;
                }
            }
        });
        let mut reader = BufReader::new(reader);
        // Kept outside the loop: a cancelled select arm leaves the partial
        // line in `buf` for the next poll rather than dropping it.
        let mut buf = Vec::with_capacity(4096);

        loop {
            tokio::select! {
                req = req_rx.recv() => {
                    let Some(req) = req else { return };
                    let line = json!({
                        "epoch": req.epoch,
                        "query": req.arg,
                        "filters": req.filters.as_ref(),
                    })
                    .to_string()
                        + "\n";
                    // A full out-channel means the writer is wedged; the
                    // question's deadline answers it rather than the queue.
                    if out_tx.try_send(line).is_err() {
                        return;
                    }
                }
                line = crate::support::lines::next(
                    &mut reader, &mut buf, crate::support::lines::MAX_LINE,
                ) => {
                    let Ok(Some(line)) = line else { return };
                    let Ok(payload) = serde_json::from_str::<Value>(line.trim()) else {
                        // A daemon that writes garbage should not take the
                        // launcher down with it — the line is dropped, not
                        // the connection, but the log hears about it.
                        if let Some((id, tx)) = &log {
                            let head: String =
                                line.trim().chars().take(120).collect();
                            let _ = tx.try_send(WorkerMsg::Log {
                                id: id.clone(),
                                ev: "sock.bad".into(),
                                fields: json!({ "head": head }),
                            });
                        }
                        continue;
                    };
                    let Some(epoch) = payload.get("epoch").and_then(|e| e.as_u64()) else {
                        continue;
                    };
                    let Some(rows) = payload.get("rows").and_then(|r| r.as_array()) else {
                        continue;
                    };
                    // Full channel: the worker is behind, and the dropped
                    // push's state arrives with the next one anyway. A
                    // closed one means the worker is gone — the connection
                    // dies with it, as before.
                    if let Err(mpsc::error::TrySendError::Closed(_)) =
                        push_tx.try_send(SocketPush {
                            epoch,
                            rows: rows.clone(),
                        })
                    {
                        return;
                    }
                }
            }
        }
    });

    Ok(SocketChan {
        req: req_tx,
        push: push_rx,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use interprocess::local_socket::ListenerOptions;
    use std::sync::Arc;
    use tokio::io::AsyncBufReadExt;

    fn test_path() -> String {
        #[cfg(unix)]
        {
            std::env::temp_dir()
                .join(format!("oxy-sock-test-{}.sock", std::process::id()))
                .to_string_lossy()
                .into_owned()
        }
        #[cfg(windows)]
        {
            // The extension name maps every configured path to this
            // namespaced pipe on Windows, which is what lets the test bind
            // the other end here.
            "oxy-ext-unsupported".to_string()
        }
    }

    /// The protocol, over one connection: an answer, a refinement pushed for
    /// the same epoch — `do:` streams its card this way — then a second
    /// question whose garbage, foreign-epoch and rows-less lines are dropped
    /// before its real answer.
    #[tokio::test]
    async fn answer_then_push() {
        let path = test_path();
        let name = name_for(&path).expect("name");
        let listener = ListenerOptions::new()
            .name(name)
            .create_tokio()
            .expect("listener");

        let mut chan = connect(&path, Duration::from_secs(5), None)
            .await
            .expect("connects");

        let server = listener.accept().await.expect("accept");
        let (sr, mut sw) = tokio::io::split(server);
        let mut lines = BufReader::new(sr).lines();

        chan.req
            .send(SocketReq {
                epoch: 7,
                arg: "kind of blue".into(),
                filters: Arc::new(BTreeMap::new()),
            })
            .expect("req sends");

        let asked = lines.next_line().await.expect("line").expect("some");
        let asked: Value = serde_json::from_str(&asked).expect("json");
        assert_eq!(asked["epoch"], 7);
        assert_eq!(asked["query"], "kind of blue");

        sw.write_all(b"{\"epoch\":7,\"rows\":[{\"title\":\"a\"}]}\n")
            .await
            .expect("answer");
        let first = tokio::time::timeout(Duration::from_secs(5), chan.push.recv())
            .await
            .expect("first push")
            .expect("some");
        assert_eq!(first.epoch, 7);
        assert_eq!(first.rows[0]["title"], "a");

        // The refinement: another push, same epoch, no new question.
        sw.write_all(b"{\"epoch\":7,\"rows\":[{\"title\":\"a\"},{\"title\":\"b\"}]}\n")
            .await
            .expect("push");
        let second = tokio::time::timeout(Duration::from_secs(5), chan.push.recv())
            .await
            .expect("second push")
            .expect("some");
        assert_eq!(second.epoch, 7);
        assert_eq!(second.rows.len(), 2);

        // A second question on the same connection, answered through noise.
        // Garbage and rows-less lines never reach the channel; a well-formed
        // foreign-epoch line does — the epoch check is the worker's, the same
        // place the QML put it.
        chan.req
            .send(SocketReq {
                epoch: 8,
                arg: String::new(),
                filters: Arc::new(BTreeMap::new()),
            })
            .expect("req sends");
        let _ = lines.next_line().await;
        sw.write_all(b"not json\n{\"epoch\":2,\"rows\":[]}\n{\"epoch\":8}\n{\"epoch\":8,\"rows\":[{\"title\":\"x\"}]}\n")
            .await
            .expect("write");
        let foreign = tokio::time::timeout(Duration::from_secs(5), chan.push.recv())
            .await
            .expect("foreign push")
            .expect("some");
        assert_eq!(foreign.epoch, 2);
        let third = tokio::time::timeout(Duration::from_secs(5), chan.push.recv())
            .await
            .expect("third push")
            .expect("some");
        assert_eq!(third.epoch, 8);
        assert_eq!(third.rows[0]["title"], "x");
    }
}
