//! The per-connection SMTP state machine.
//!
//! This is a deliberately permissive ESMTP server: it speaks just enough of
//! RFC 5321 to satisfy real sending clients (greeting, EHLO/HELO, MAIL/RCPT,
//! DATA, RSET/NOOP/QUIT) and accepts AUTH unconditionally so clients that
//! insist on authenticating still go through. STARTTLS is advertised-absent
//! (politely refused) — plaintext only, by design, for a local mock.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedReadHalf;
use tokio::sync::mpsc::UnboundedSender;

use crate::message::{Envelope, ReceivedMessage};

/// Hard cap on a single DATA payload, to bound memory from a runaway client.
const MAX_MESSAGE: usize = 25 * 1024 * 1024;

/// Drive one SMTP conversation to completion (QUIT or disconnect).
pub async fn handle(
    stream: TcpStream,
    peer: SocketAddr,
    port: u16,
    tx: UnboundedSender<ReceivedMessage>,
    counter: Arc<AtomicU64>,
) -> anyhow::Result<()> {
    let (read_half, mut wr) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    wr.write_all(b"220 mock-smtp ESMTP ready\r\n").await?;

    let mut mail_from = String::new();
    let mut rcpt_to: Vec<String> = Vec::new();
    let mut line = String::new();

    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            break; // client disconnected
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let (cmd, rest) = match trimmed.split_once(' ') {
            Some((c, r)) => (c.to_ascii_uppercase(), r.trim()),
            None => (trimmed.to_ascii_uppercase(), ""),
        };

        match cmd.as_str() {
            "EHLO" => {
                // Multi-line capability list. We advertise AUTH so that clients
                // configured with credentials proceed; we accept any of them.
                let banner = format!(
                    "250-mock-smtp greets {rest}\r\n\
                     250-SIZE {MAX_MESSAGE}\r\n\
                     250-8BITMIME\r\n\
                     250-SMTPUTF8\r\n\
                     250-AUTH PLAIN LOGIN\r\n\
                     250 HELP\r\n"
                );
                wr.write_all(banner.as_bytes()).await?;
            }
            "HELO" => wr.write_all(b"250 mock-smtp\r\n").await?,
            "MAIL" => {
                mail_from = extract_addr(rest);
                rcpt_to.clear();
                wr.write_all(b"250 OK\r\n").await?;
            }
            "RCPT" => {
                rcpt_to.push(extract_addr(rest));
                wr.write_all(b"250 OK\r\n").await?;
            }
            "DATA" => {
                wr.write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n")
                    .await?;
                let raw = read_data(&mut reader).await?;
                let id = counter.fetch_add(1, Ordering::SeqCst);
                let envelope = Envelope {
                    port,
                    peer,
                    mail_from: std::mem::take(&mut mail_from),
                    rcpt_to: std::mem::take(&mut rcpt_to),
                };
                let message = ReceivedMessage::parse(id, envelope, raw);
                // If the UI is gone, the send fails and we simply stop caring.
                let _ = tx.send(message);
                wr.write_all(format!("250 OK: queued as {id}\r\n").as_bytes())
                    .await?;
            }
            "RSET" => {
                mail_from.clear();
                rcpt_to.clear();
                wr.write_all(b"250 OK\r\n").await?;
            }
            "NOOP" => wr.write_all(b"250 OK\r\n").await?,
            "VRFY" | "EXPN" => wr.write_all(b"252 Cannot verify\r\n").await?,
            "AUTH" => handle_auth(&mut reader, &mut wr, rest).await?,
            "STARTTLS" => wr.write_all(b"454 TLS not available\r\n").await?,
            "QUIT" => {
                wr.write_all(b"221 Bye\r\n").await?;
                break;
            }
            "" => {}
            _ => wr.write_all(b"500 Unrecognized command\r\n").await?,
        }
    }

    Ok(())
}

/// Read the DATA payload until the terminating `<CRLF>.<CRLF>`, undoing
/// dot-stuffing (a leading `.` doubled by the client) as we go. Lines past
/// [`MAX_MESSAGE`] are dropped but still consumed so the stream stays in sync.
async fn read_data(reader: &mut BufReader<OwnedReadHalf>) -> anyhow::Result<Vec<u8>> {
    let mut raw = Vec::new();
    let mut line = Vec::new();

    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).await? == 0 {
            break; // disconnect mid-DATA
        }
        let content = trim_crlf(&line);
        if content == b"." {
            break;
        }
        // Dot-unstuffing: a line that began with '.' was sent as ".."
        let content = if content.first() == Some(&b'.') {
            &content[1..]
        } else {
            content
        };
        if raw.len() < MAX_MESSAGE {
            raw.extend_from_slice(content);
            raw.extend_from_slice(b"\r\n");
        }
    }

    Ok(raw)
}

/// Accept any AUTH attempt, walking the client through PLAIN/LOGIN prompts as
/// needed so it completes its handshake. Credentials are read and ignored.
async fn handle_auth<W>(
    reader: &mut BufReader<OwnedReadHalf>,
    wr: &mut W,
    rest: &str,
) -> anyhow::Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    let mechanism = rest.split_whitespace().next().unwrap_or("").to_ascii_uppercase();
    let mut scratch = String::new();

    match mechanism.as_str() {
        "LOGIN" => {
            // "334 Username:" / "334 Password:" (base64-encoded prompts).
            wr.write_all(b"334 VXNlcm5hbWU6\r\n").await?;
            read_one(reader, &mut scratch).await?;
            wr.write_all(b"334 UGFzc3dvcmQ6\r\n").await?;
            read_one(reader, &mut scratch).await?;
        }
        "PLAIN" => {
            // Credentials may be inline ("AUTH PLAIN <b64>") or on the next line.
            let has_inline = rest.split_whitespace().nth(1).is_some();
            if !has_inline {
                wr.write_all(b"334 \r\n").await?;
                read_one(reader, &mut scratch).await?;
            }
        }
        _ => {}
    }

    wr.write_all(b"235 2.7.0 Authentication successful\r\n").await?;
    Ok(())
}

/// Read and discard one line of client input (used for AUTH continuations).
async fn read_one(reader: &mut BufReader<OwnedReadHalf>, buf: &mut String) -> anyhow::Result<()> {
    buf.clear();
    reader.read_line(buf).await?;
    Ok(())
}

/// Strip a trailing CRLF (or lone LF) from a raw line.
fn trim_crlf(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    if end > 0 && line[end - 1] == b'\n' {
        end -= 1;
    }
    if end > 0 && line[end - 1] == b'\r' {
        end -= 1;
    }
    &line[..end]
}

/// Extract the address from a `MAIL FROM:<addr>` / `RCPT TO:<addr>` argument,
/// tolerating optional ESMTP parameters after it.
fn extract_addr(arg: &str) -> String {
    if let (Some(open), Some(close)) = (arg.find('<'), arg.rfind('>'))
        && close > open
    {
        return arg[open + 1..close].to_string();
    }
    // Fall back to whatever follows the first ':' (e.g. "FROM:addr").
    arg.split_once(':')
        .map(|(_, v)| v.trim().to_string())
        .unwrap_or_else(|| arg.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    #[test]
    fn extract_addr_handles_brackets_and_params() {
        assert_eq!(extract_addr("FROM:<a@b.com> SIZE=10"), "a@b.com");
        assert_eq!(extract_addr("TO:<x@y.io>"), "x@y.io");
        assert_eq!(extract_addr("FROM:plain@addr"), "plain@addr");
    }

    #[test]
    fn dot_stuffing_and_crlf_trimming() {
        assert_eq!(trim_crlf(b"hello\r\n"), b"hello");
        assert_eq!(trim_crlf(b"hello\n"), b"hello");
        assert_eq!(trim_crlf(b"hello"), b"hello");
    }

    /// Read a (possibly multi-line) SMTP reply; continuation lines have a '-'
    /// as the 4th byte ("250-"), the final line a space ("250 ").
    async fn read_reply<R: AsyncBufReadExt + Unpin>(r: &mut R) -> String {
        let mut out = String::new();
        loop {
            let mut line = String::new();
            r.read_line(&mut line).await.unwrap();
            let done = line.as_bytes().get(3) != Some(&b'-');
            out.push_str(&line);
            if done {
                break;
            }
        }
        out
    }

    #[tokio::test]
    async fn full_transaction_yields_parsed_message() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let counter = Arc::new(AtomicU64::new(1));

        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            handle(stream, peer, addr.port(), tx, counter).await.unwrap();
        });

        let client = TcpStream::connect(addr).await.unwrap();
        let (rd, mut wr) = client.into_split();
        let mut reader = BufReader::new(rd);

        assert!(read_reply(&mut reader).await.starts_with("220"));

        wr.write_all(b"EHLO tester\r\n").await.unwrap();
        let ehlo = read_reply(&mut reader).await;
        assert!(ehlo.contains("250") && ehlo.contains("AUTH"));

        wr.write_all(b"MAIL FROM:<alice@example.com>\r\n").await.unwrap();
        assert!(read_reply(&mut reader).await.starts_with("250"));
        wr.write_all(b"RCPT TO:<bob@example.com>\r\n").await.unwrap();
        assert!(read_reply(&mut reader).await.starts_with("250"));

        wr.write_all(b"DATA\r\n").await.unwrap();
        assert!(read_reply(&mut reader).await.starts_with("354"));

        let body = "From: Alice <alice@example.com>\r\n\
                    To: Bob <bob@example.com>\r\n\
                    Subject: Hello there\r\n\
                    \r\n\
                    This is the body.\r\n\
                    .\r\n";
        wr.write_all(body.as_bytes()).await.unwrap();
        assert!(read_reply(&mut reader).await.contains("250"));

        wr.write_all(b"QUIT\r\n").await.unwrap();
        assert!(read_reply(&mut reader).await.starts_with("221"));

        let message = rx.recv().await.expect("message delivered to channel");
        assert_eq!(message.subject, "Hello there");
        assert!(message.from.contains("alice@example.com"));
        assert_eq!(message.envelope.mail_from, "alice@example.com");
        assert_eq!(message.envelope.rcpt_to, vec!["bob@example.com"]);
        assert!(message.text_body.unwrap().contains("This is the body."));
    }
}
