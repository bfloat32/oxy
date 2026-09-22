//! A minimal HTTP/1.1 client for local model endpoints.
//!
//! Local servers speak plain HTTP on loopback, so this is deliberately small:
//! no TLS, no redirects, no cookies, no keep-alive. What it does have to get
//! right is the thing the whole feature is for — a response that arrives in
//! pieces and must be handed on as it arrives, which means reading
//! `Transfer-Encoding: chunked` incrementally rather than buffering the body.

use std::io;

use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedReadHalf;

/// A parsed `http://host:port/path` URL. Nothing else is accepted: an
/// endpoint that needs TLS is not a local model, and silently connecting
/// plaintext to a remote host is the one mistake worth refusing outright.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl Url {
    /// An endpoint must be loopback — `localhost`, any `127.x`, or `::1`.
    /// The module is loopback-only on purpose: a remote `http://` endpoint
    /// would take the question — and the bearer key — in plaintext.
    fn loopback(host: &str) -> bool {
        let host = host.trim_matches(|c| c == '[' || c == ']');
        if host.eq_ignore_ascii_case("localhost") || host == "::1" {
            return true;
        }
        host.parse::<std::net::Ipv4Addr>()
            .map(|a| a.octets()[0] == 127)
            .unwrap_or(false)
    }

    pub fn parse(raw: &str) -> Option<Url> {
        let rest = raw.trim().strip_prefix("http://")?;
        // Nothing that lands in the request head may carry a control
        // character — a `\r\n` here is a smuggled header.
        if rest.bytes().any(|b| b < 0x20 || b == 0x7f) {
            return None;
        }
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        if authority.is_empty() || authority.contains('@') {
            return None;
        }
        let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
            // `[::1]` or `[::1]:8080` — the v6 colons are not the port's.
            let (h, tail) = rest.split_once(']')?;
            match tail.strip_prefix(':') {
                Some(p) => (h, p.parse::<u16>().ok()?),
                None if tail.is_empty() => (h, 80),
                None => return None,
            }
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h, p.parse::<u16>().ok()?),
                None => (authority, 80),
            }
        };
        if host.is_empty() || !Self::loopback(host) {
            return None;
        }
        Some(Url {
            host: host.to_string(),
            port,
            path: path.to_string(),
        })
    }

    /// `host:port` for a chip or an error line — never the whole URL, which
    /// is the user's configuration and does not belong in a row.
    pub fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// A response whose body is read as it arrives.
pub struct Response {
    pub status: u16,
    /// The server's own delay before a retry, when it sent one. The policy
    /// lives in `retry`; this is only the header, parsed.
    pub retry_after: Option<std::time::Duration>,
    reader: BufReader<OwnedReadHalf>,
    chunked: bool,
    buf: Vec<u8>,
    pos: usize,
    chunk_left: usize,
    eof: bool,
}

impl Response {
    /// The next line of the body, without its newline. `Ok(None)` at the end.
    pub async fn next_line(&mut self) -> io::Result<Option<String>> {
        loop {
            if let Some(i) = self.buf[self.pos..].iter().position(|b| *b == b'\n') {
                let end = self.pos + i;
                let line = String::from_utf8_lossy(&self.buf[self.pos..end])
                    .trim_end_matches('\r')
                    .to_string();
                self.pos = end + 1;
                if self.pos > 64 * 1024 {
                    self.buf.drain(..self.pos);
                    self.pos = 0;
                }
                return Ok(Some(line));
            }
            if self.buf.len() - self.pos > 16 * 1024 * 1024 {
                // One line past 16 MiB is not an answer, it is a runaway —
                // a model's whole reply does not come down as a single line.
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "response line exceeded 16 MiB",
                ));
            }
            if !self.read_more().await? {
                // A body whose last line has no newline is still a line: a
                // JSON reply from a server that does not end it with one
                // would otherwise read as empty.
                if self.pos < self.buf.len() {
                    let rest = String::from_utf8_lossy(&self.buf[self.pos..])
                        .trim_end_matches('\r')
                        .to_string();
                    self.pos = self.buf.len();
                    return Ok(Some(rest));
                }
                return Ok(None);
            }
        }
    }

    /// Append one piece of the body to the buffer; `false` means the body
    /// ended. Chunked bodies are de-chunked here so callers see plain lines.
    ///
    /// Every socket read carries the stall deadline: a server that answers
    /// the head and then goes silent would otherwise park the turn task —
    /// and its connection — forever, unreachable even by `stopask`. The
    /// bound is per read, not per body: a slow model pausing between tokens
    /// is fine; two minutes without a single byte is not a pause, it is a
    /// dead socket.
    async fn read_more(&mut self) -> io::Result<bool> {
        /// One read's worth of patience. Generous for a model thinking on
        /// CPU, fatal for a peer that is gone.
        const STALL: std::time::Duration = std::time::Duration::from_secs(120);
        fn stalled() -> io::Error {
            io::Error::new(io::ErrorKind::TimedOut, "response body stalled")
        }
        if self.eof {
            return Ok(false);
        }
        if self.chunked {
            if self.chunk_left == 0 {
                // The size line is a few hex digits; anything past 4 KiB of
                // it is garbage, and a truncated read fails the parse below
                // into `size == 0` — fails closed, at EOF.
                let mut size_buf = Vec::with_capacity(64);
                let size_line = tokio::time::timeout(
                    STALL,
                    crate::support::lines::next(&mut self.reader, &mut size_buf, 4096),
                )
                .await
                .map_err(|_| stalled())??
                .unwrap_or_default();
                if size_buf.is_empty() && size_line.is_empty() {
                    self.eof = true;
                    return Ok(false);
                }
                let hex = size_line.trim().split(';').next().unwrap_or("").trim();
                let size = usize::from_str_radix(hex, 16).unwrap_or(0);
                if size == 0 {
                    self.eof = true;
                    return Ok(false);
                }
                self.chunk_left = size;
            }
            let want = self.chunk_left.min(8192);
            let mut tmp = vec![0u8; want];
            tokio::time::timeout(STALL, self.reader.read_exact(&mut tmp))
                .await
                .map_err(|_| stalled())??;
            self.buf.extend_from_slice(&tmp);
            self.chunk_left -= want;
            if self.chunk_left == 0 {
                // The CRLF that closes the chunk, read and discarded.
                let mut crlf = [0u8; 2];
                let _ = tokio::time::timeout(STALL, self.reader.read_exact(&mut crlf)).await;
            }
            return Ok(true);
        }
        let mut tmp = [0u8; 8192];
        let n = tokio::time::timeout(STALL, self.reader.read(&mut tmp))
            .await
            .map_err(|_| stalled())??;
        if n == 0 {
            self.eof = true;
            return Ok(false);
        }
        self.buf.extend_from_slice(&tmp[..n]);
        Ok(true)
    }
}

/// POST a JSON body and return the response with its body still streaming.
pub async fn post_json(url: &Url, body: &str) -> io::Result<Response> {
    post_json_with(url, body, &[]).await
}

/// The same POST with extra request headers — a `Authorization: Bearer …`
/// for a server that wants a key.
pub async fn post_json_with(
    url: &Url,
    body: &str,
    headers: &[(&str, &str)],
) -> io::Result<Response> {
    send(url, "POST", Some(body), headers).await
}

/// A GET, for the model list the doctor reads. Same reader, no body.
pub async fn get_json(url: &Url, headers: &[(&str, &str)]) -> io::Result<Response> {
    send(url, "GET", None, headers).await
}

/// One request. `body` present means POST with a JSON content type; absent
/// means GET, which wants a JSON `Accept` instead of an event stream.
async fn send(
    url: &Url,
    method: &str,
    body: Option<&str>,
    headers: &[(&str, &str)],
) -> io::Result<Response> {
    // Header fields land verbatim in the request head: a name outside the
    // token set or a value carrying a control character is a smuggled
    // header, not a config.
    for (name, value) in headers {
        let bad_name = name.is_empty()
            || name
                .bytes()
                .any(|b| !(b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)));
        let bad_value = value.bytes().any(|b| (b < 0x20 && b != b'\t') || b == 0x7f);
        if bad_name || bad_value {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "control characters in a request header",
            ));
        }
    }
    // A hung endpoint must not hold the turn forever: connect is the first
    // read in disguise, and it gets the same deadline the head does.
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        TcpStream::connect((url.host.as_str(), url.port)),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connect timed out"))??;
    stream.set_nodelay(true).ok();
    let (read, mut write) = stream.into_split();

    let mut head = format!(
        "{method} {} HTTP/1.1\r\nHost: {}\r\n",
        url.path,
        url.authority()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some(body) = body {
        head.push_str(&format!(
            "Content-Type: application/json\r\nAccept: text/event-stream\r\n\
             Content-Length: {}\r\n",
            body.len()
        ));
    } else {
        head.push_str("Accept: application/json\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");

    write.write_all(head.as_bytes()).await?;
    if let Some(body) = body {
        write.write_all(body.as_bytes()).await?;
    }
    write.flush().await?;

    let mut reader = BufReader::new(read);
    // The head is read with the same bounded line reader the sockets use:
    // `read_line` would have let one header line grow to whatever the peer
    // sent, and a silent endpoint gets a deadline rather than a stall.
    let head = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut head_buf = Vec::new();
        let status_line =
            match crate::support::lines::next(&mut reader, &mut head_buf, 16 * 1024).await? {
                None => {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "no response"));
                }
                Some(l) => l,
            };
        let status = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad status line"))?;

        // Headers: `Transfer-Encoding` decides how the body is framed; a
        // `Content-Length` body is read to EOF, which is what a streamed
        // answer with `Connection: close` does anyway. `Retry-After` is kept
        // for the caller that decides whether to send the question again.
        let mut chunked = false;
        let mut retry_after = None;
        for _ in 0..128 {
            let Some(line) =
                crate::support::lines::next(&mut reader, &mut head_buf, 16 * 1024).await?
            else {
                break;
            };
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                break;
            }
            if let Some((name, value)) = trimmed.split_once(':') {
                if name.eq_ignore_ascii_case("transfer-encoding") {
                    chunked = value.to_ascii_lowercase().contains("chunked");
                } else if name.eq_ignore_ascii_case("retry-after") {
                    retry_after = crate::provider::llm::retry::parse_retry_after(value);
                }
            }
        }
        Ok((status, chunked, retry_after, reader))
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "response head timed out"))?;
    let (status, chunked, retry_after, reader) = head?;

    Ok(Response {
        status,
        retry_after,
        reader,
        chunked,
        buf: Vec::new(),
        pos: 0,
        chunk_left: 0,
        eof: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn urls_are_parsed_strictly() {
        let u = Url::parse("http://127.0.0.1:11434/v1/chat/completions").unwrap();
        assert_eq!(u.host, "127.0.0.1");
        assert_eq!(u.port, 11434);
        assert_eq!(u.path, "/v1/chat/completions");
        assert_eq!(u.authority(), "127.0.0.1:11434");

        let u = Url::parse("http://localhost/v1/chat").unwrap();
        assert_eq!(u.port, 80);
        assert_eq!(u.path, "/v1/chat");

        // `[::1]` and any `127.x` are loopback; a bare `[::1]` gets port 80.
        let u = Url::parse("http://[::1]:8080/v1").unwrap();
        assert_eq!(u.host, "::1");
        assert_eq!(u.port, 8080);
        assert_eq!(Url::parse("http://127.0.0.2:11434/").unwrap().port, 11434);

        // https, a missing scheme, credentials, a bad port — and anything
        // that is not loopback, because plaintext to a remote host would
        // carry the bearer key with it.
        assert!(Url::parse("https://api.example.com/v1").is_none());
        assert!(Url::parse("api.example.com:1234/v1").is_none());
        assert!(Url::parse("http://user:pw@localhost/v1").is_none());
        assert!(Url::parse("http://localhost:notaport/v1").is_none());
        assert!(Url::parse("http://").is_none());
        assert!(Url::parse("http://box:8080/v1").is_none());
        assert!(Url::parse("http://192.168.1.5:11434/v1").is_none());
        // A header smuggled through the path is refused at parse.
        assert!(Url::parse("http://localhost/x\r\nX: 1").is_none());
    }

    /// One stub server, both framings: the point of the client is that a
    /// streamed answer arrives line by line, not in one buffer at the end.
    async fn serve(reply: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            // Read the request head so the client is not writing into a
            // closing socket.
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            sock.write_all(reply.as_bytes()).await.unwrap();
            sock.flush().await.unwrap();
        });
        port
    }

    #[tokio::test]
    async fn reads_a_chunked_stream_line_by_line() {
        let port = serve(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
             f\r\ndata: {\"a\":1}\n\n\r\n\
             f\r\ndata: {\"a\":2}\n\n\r\n\
             0\r\n\r\n",
        )
        .await;
        let url = Url::parse(&format!("http://127.0.0.1:{port}/v1/chat")).unwrap();
        let mut resp = post_json(&url, "{}").await.unwrap();
        assert_eq!(resp.status, 200);
        let mut lines = Vec::new();
        while let Some(line) = resp.next_line().await.unwrap() {
            lines.push(line);
        }
        assert_eq!(lines, vec!["data: {\"a\":1}", "", "data: {\"a\":2}", ""]);
    }

    #[tokio::test]
    async fn reads_an_unframed_body_to_eof() {
        let port = serve("HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\n{\"x\":1}\n{}\n").await;
        let url = Url::parse(&format!("http://127.0.0.1:{port}/v1/chat")).unwrap();
        let mut resp = post_json(&url, "{}").await.unwrap();
        let mut lines = Vec::new();
        while let Some(line) = resp.next_line().await.unwrap() {
            lines.push(line);
        }
        assert_eq!(lines, vec!["{\"x\":1}", "{}"]);
    }

    #[tokio::test]
    async fn extra_headers_ride_along() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 2048];
            let n = sock.read(&mut buf).await.unwrap();
            let head = String::from_utf8_lossy(&buf[..n]).to_string();
            let auth = head
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                .unwrap_or("no header")
                .to_string();
            let body = format!("{{\"echo\":\"{auth}\"}}");
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            sock.write_all(reply.as_bytes()).await.unwrap();
            sock.flush().await.unwrap();
        });
        let url = Url::parse(&format!("http://127.0.0.1:{port}/v1/chat")).unwrap();
        let mut resp = post_json_with(&url, "{}", &[("Authorization", "Bearer sekrit")])
            .await
            .unwrap();
        let line = resp.next_line().await.unwrap().unwrap();
        assert_eq!(line, r#"{"echo":"Authorization: Bearer sekrit"}"#);

        // …and no header at all when the caller passes none, rather than an
        // empty bearer.
        let port2 = serve("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}").await;
        let url2 = Url::parse(&format!("http://127.0.0.1:{port2}/v1/chat")).unwrap();
        assert!(post_json(&url2, "{}").await.is_ok());
    }

    #[tokio::test]
    async fn a_refused_connection_is_an_error_not_a_panic() {
        let url = Url::parse("http://127.0.0.1:1/v1/chat").unwrap();
        assert!(post_json(&url, "{}").await.is_err());
    }
}
