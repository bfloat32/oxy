//! A bounded line for the socket readers. `BufRead::lines` grows without a
//! ceiling on a peer that never sends a newline — fine for a file that is
//! what it is, wrong for a pipe another process writes.

use tokio::io::{AsyncBufReadExt, BufReader};

/// The biggest line a socket peer may send before the rest of it is noise:
/// far past any real row, command or event, small enough that a peer that
/// never sends `\n` cannot grow the buffer without bound.
pub const MAX_LINE: usize = 256 * 1024;

/// The next line off `reader`, without its `\n` (and the `\r` before it,
/// the way `lines()` reads). `Ok(None)` at EOF. `buf` is the caller's —
/// it survives a cancelled `select!` arm with the partial line intact, so
/// a question landing mid-read loses nothing.
///
/// A line that outgrows `max` is cut there and the remainder becomes the
/// next line's problem: the read stays bounded rather than holding the
/// whole runaway write.
pub async fn next<R>(
    reader: &mut BufReader<R>,
    buf: &mut Vec<u8>,
    max: usize,
) -> std::io::Result<Option<String>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    loop {
        if let Some(i) = buf.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = buf.drain(..=i).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
        }
        if buf.len() > max {
            let line: Vec<u8> = buf.drain(..max).collect();
            return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
        }
        // `read_until` would hold the whole line before returning — a peer
        // that never sends `\n` would grow it without bound. `fill_buf`
        // hands over the reader's own window instead: at most its capacity
        // lands per pass, and the cap above fires while a runaway line is
        // still being accumulated rather than after.
        let avail = reader.fill_buf().await?;
        if avail.is_empty() {
            if buf.is_empty() {
                return Ok(None);
            }
            // A final line the peer never ended still counts as one.
            let mut line = std::mem::take(buf);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
        }
        let take = avail
            .iter()
            .position(|b| *b == b'\n')
            .map(|i| i + 1)
            .unwrap_or(avail.len())
            .min(max + 1 - buf.len());
        buf.extend_from_slice(&avail[..take]);
        reader.consume(take);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reader(bytes: &'static [u8]) -> BufReader<&'static [u8]> {
        BufReader::new(bytes)
    }

    #[tokio::test]
    async fn lines_come_off_one_at_a_time() {
        let mut r = reader(b"one\ntwo\r\nthree");
        let mut buf = Vec::new();
        assert_eq!(
            next(&mut r, &mut buf, MAX_LINE).await.unwrap().as_deref(),
            Some("one")
        );
        assert_eq!(
            next(&mut r, &mut buf, MAX_LINE).await.unwrap().as_deref(),
            Some("two")
        );
        // The unterminated tail is still a line, and then EOF.
        assert_eq!(
            next(&mut r, &mut buf, MAX_LINE).await.unwrap().as_deref(),
            Some("three")
        );
        assert!(next(&mut r, &mut buf, MAX_LINE).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_runaway_line_is_cut_at_the_cap() {
        let mut body = vec![b'x'; 64];
        body.extend_from_slice(b"\nnext\n");
        let mut r = BufReader::new(&body[..]);
        let mut buf = Vec::new();
        let line = next(&mut r, &mut buf, 16).await.unwrap().unwrap();
        assert_eq!(line.len(), 16);
        // The cut remainder goes on being read — nothing past the cap held.
        assert!(next(&mut r, &mut buf, 16).await.unwrap().is_some());
    }
}
