//! A long-running program answers over a local socket instead of being
//! started per keystroke — the `socket` field's protocol, ported:
//!
//! ```text
//! → {"epoch": 12, "query": "text", "filters": {"year": "1959"}}\n
//! ← {"epoch": 12, "rows": [ ... ]}\n
//! ```
//!
//! One JSON line out per question, one or more back, and the connection
//! outlives the launcher being closed.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[cfg(unix)]
use interprocess::local_socket::{tokio::prelude::*, GenericFilePath, ToFsName};
#[cfg(windows)]
use interprocess::local_socket::{tokio::prelude::*, GenericNamespaced, ToNsName};

type Pipe = interprocess::local_socket::tokio::Stream;

fn name_for(path: &str) -> std::io::Result<interprocess::local_socket::Name<'static>> {
    #[cfg(unix)]
    {
        path.to_string().to_fs_name::<GenericFilePath>()
    }
    #[cfg(windows)]
    {
        // A configured path is still a path on Windows for now; the daemon's
        // own name is namespaced. Extension sockets are a Unix feature.
        let _ = path;
        "oxy-ext-unsupported"
            .to_string()
            .to_ns_name::<GenericNamespaced>()
    }
}

async fn connect(path: &str) -> std::io::Result<Pipe> {
    Pipe::connect(name_for(path)?).await
}

/// A persistent connection to one extension's daemon.
pub struct SocketConn {
    reader: BufReader<tokio::io::ReadHalf<Pipe>>,
    writer: tokio::io::WriteHalf<Pipe>,
}

impl SocketConn {
    pub async fn connect(path: &str) -> std::io::Result<SocketConn> {
        let stream = connect(path).await?;
        let (reader, writer) = tokio::io::split(stream);
        Ok(SocketConn {
            reader: BufReader::new(reader),
            writer,
        })
    }

    /// Ask one question, wait up to `timeout` for a line answering it, and
    /// return its `rows`. A dead daemon or a timeout answers `None`.
    pub async fn ask(
        &mut self,
        epoch: u64,
        query: &str,
        filters: &BTreeMap<String, String>,
        timeout: Duration,
    ) -> Option<Vec<Value>> {
        let line = json!({
            "epoch": epoch,
            "query": query,
            "filters": filters,
        })
        .to_string()
            + "\n";
        if self.writer.write_all(line.as_bytes()).await.is_err() {
            return None;
        }

        let mut buf = String::new();
        loop {
            buf.clear();
            let read = self.reader.read_line(&mut buf);
            match tokio::time::timeout(timeout, read).await {
                Ok(Ok(0)) | Ok(Err(_)) | Err(_) => return None,
                Ok(Ok(_)) => {
                    let Ok(payload) = serde_json::from_str::<Value>(buf.trim()) else {
                        continue;
                    };
                    // The daemon echoes the epoch it was asked under — the only
                    // thing that makes a pushed answer safe.
                    if payload.get("epoch").and_then(|e| e.as_u64()) != Some(epoch) {
                        continue;
                    }
                    return payload.get("rows").and_then(|r| r.as_array()).cloned();
                }
            }
        }
    }
}
