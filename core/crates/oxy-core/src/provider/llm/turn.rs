//! One question, streamed: the request, the retry policy, the deltas.
//!
//! The engine answers the card with this and the `oxy ask` verb answers a
//! terminal with it, so the loop lives here once instead of twice. What
//! arrives is raw: `Piece::Text` is one delta as the server sent it (the card
//! buffers those into whole lines itself, a terminal prints them as they
//! come), `Piece::Notice` is something worth saying out loud while waiting,
//! and `Piece::Error` ends the turn.
//!
//! The task is detached from its caller: dropping the receiver ends the
//! stream at the next send, which is how `stop_ask` cancels a turn.

use tokio::sync::mpsc;

use super::{Local, http, retry, stream::Delta};

/// What a streamed turn produces, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// A delta, exactly as the server sent it.
    Text(String),
    /// Something the user should see while the turn waits.
    Notice(String),
    /// The turn is over, and this is why. Empty means it finished.
    Error(String),
}

/// Send one question and stream the answer back. The receiver ends after the
/// final `Piece::Error`, and ends early if the caller drops it.
pub fn spawn(
    local: Local,
    history: Vec<(String, String)>,
    question: String,
) -> mpsc::Receiver<Piece> {
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(async move {
        let body = local.chat_request(&history, &question);

        // A server that wants a key gets it; one that does not gets no header
        // at all rather than an empty bearer.
        let mut owned: Vec<(&str, String)> = Vec::new();
        if !local.key.is_empty() {
            owned.push(("Authorization", format!("Bearer {}", local.key)));
        }
        let headers: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();

        // Send, and send again while the failure is the kind that goes away —
        // a server still loading a model (503) or a rate limit (429). Nothing
        // is re-sent once the answer has started: a stream that breaks
        // mid-way is the card's problem, not the retry's.
        let mut attempt = 1u32;
        let mut response = None;
        let mut error = String::new();
        loop {
            match http::post_json_with(&local.url, &body, &headers).await {
                Ok(resp) if resp.status == 200 => {
                    response = Some(resp);
                    break;
                }
                Ok(resp) if retry::retryable(resp.status) && attempt < retry::MAX_ATTEMPTS => {
                    let wait = retry::wait_before(attempt, resp.retry_after);
                    // A quiet minute looks like a hang, so a wait long enough
                    // to notice says what it is waiting for.
                    if wait >= std::time::Duration::from_secs(2)
                        && tx
                            .send(Piece::Notice(format!(
                                "· {} answered {} — trying again in {}s",
                                local.url.authority(),
                                resp.status,
                                wait.as_secs()
                            )))
                            .await
                            .is_err()
                    {
                        return;
                    }
                    tokio::time::sleep(wait).await;
                }
                Ok(resp) => {
                    error = format!("{} answered {}.", local.url.authority(), resp.status);
                    break;
                }
                Err(_) if attempt < retry::MAX_ATTEMPTS => {
                    // A refused or dropped connection is worth one more try: a
                    // server starting up refuses for a moment, and half a
                    // second later it does not.
                    tokio::time::sleep(retry::wait_before(attempt, None)).await;
                }
                Err(e) => {
                    error = format!(
                        "Could not reach {} ({e}). Is the model server running?",
                        local.url.authority()
                    );
                    break;
                }
            }
            attempt += 1;
        }

        if let Some(mut response) = response {
            loop {
                match response.next_line().await {
                    Ok(Some(line)) => match super::stream::parse_line(&line) {
                        Some(Delta::Text(text)) => {
                            if tx.send(Piece::Text(text)).await.is_err() {
                                return;
                            }
                        }
                        Some(Delta::Error(why)) => error = why,
                        Some(Delta::Done) => break,
                        None => {}
                    },
                    Ok(None) => break,
                    Err(e) => {
                        error = format!("The stream broke: {e}");
                        break;
                    }
                }
            }
        }
        // An error after some text is reported, not thrown away: the caller
        // keeps what arrived and shows why it stopped.
        let _ = tx.send(Piece::Error(error)).await;
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::LocalAsk;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn local(port: u16) -> Local {
        Local::from_settings(&LocalAsk {
            endpoint: format!("http://127.0.0.1:{port}/v1/chat/completions"),
            model: "stub".into(),
            system: String::new(),
            max_tokens: 16,
            temperature: 0.0,
            key: String::new(),
        })
        .unwrap()
    }

    async fn serve(reply: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            sock.write_all(reply.as_bytes()).await.unwrap();
            sock.flush().await.unwrap();
        });
        port
    }

    /// Drive a turn to its end and collect the pieces.
    async fn collect(mut rx: mpsc::Receiver<Piece>) -> Vec<Piece> {
        let mut out = Vec::new();
        while let Some(piece) = rx.recv().await {
            let done = matches!(piece, Piece::Error(_));
            out.push(piece);
            if done {
                break;
            }
        }
        out
    }

    #[tokio::test]
    async fn deltas_arrive_then_a_clean_end() {
        let port = serve(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
             30\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n\r\n\
             e\r\ndata: [DONE]\n\n\r\n\
             0\r\n\r\n",
        )
        .await;
        let pieces = collect(spawn(local(port), vec![], "q".into())).await;
        assert_eq!(
            pieces,
            vec![Piece::Text("hi".into()), Piece::Error(String::new())]
        );
    }

    #[tokio::test]
    async fn a_refused_server_ends_with_the_error_after_retrying() {
        // Port 1 refuses: the retry policy runs out and the error names the
        // address, which is what the card shows.
        let pieces = collect(spawn(local(1), vec![], "q".into())).await;
        match pieces.last() {
            Some(Piece::Error(why)) => assert!(why.contains("Could not reach")),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dropping_the_receiver_ends_the_task() {
        let port = serve(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
             30\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n\r\n\
             0\r\n\r\n",
        )
        .await;
        let rx = spawn(local(port), vec![], "q".into());
        drop(rx); // the task notices at its next send and returns
    }
}
