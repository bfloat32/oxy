//! One bounded question about the network: is anything listening there?
//!
//! Used by the ask endpoint's probe and by the setup hint that notices a
//! local Ollama, so the timeout and the "a refusal is a false, not an error"
//! rule live in one place.

use std::time::Duration;

/// Connect, with a deadline. `false` for a refusal, a timeout, or a host
/// that does not resolve — all of which mean "nothing is there to talk to".
pub async fn port_open(host: &str, port: u16, timeout: Duration) -> bool {
    matches!(
        tokio::time::timeout(timeout, tokio::net::TcpStream::connect((host, port))).await,
        Ok(Ok(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_listening_port_is_open_and_a_closed_one_is_not() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let short = Duration::from_millis(500);
        assert!(port_open("127.0.0.1", port, short).await);

        // Nothing listens on port 1 as an unprivileged user.
        assert!(!port_open("127.0.0.1", 1, short).await);
    }
}
