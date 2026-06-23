//! The per-connection SMTP state machine.
//!
//! A deliberately permissive ESMTP server: it speaks just enough of RFC 5321 to
//! satisfy real sending clients (greeting, EHLO/HELO, MAIL/RCPT, DATA,
//! RSET/NOOP/QUIT) and accepts AUTH unconditionally so clients that insist on
//! authenticating still go through.
//!
//! TLS comes in two forms, both backed by one self-signed cert:
//! - **Implicit TLS** (SMTPS, typically port 465): the TLS handshake happens
//!   before any SMTP, so the session starts already encrypted.
//! - **STARTTLS**: the session begins in plaintext, advertises `STARTTLS`, and
//!   on the client's request upgrades the live connection in place.
//!
//! Both are modeled by [`Transport`], a duplex stream that is either plaintext
//! or TLS. Reading and writing go through a single `BufReader<Transport>`; a
//! STARTTLS upgrade swaps the inner transport without changing any types, so the
//! command loop is oblivious to which it's talking over.

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::mpsc::UnboundedSender;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;

use crate::message::{Envelope, ReceivedMessage};

/// Hard cap on a single DATA payload, to bound memory from a runaway client.
const MAX_MESSAGE: usize = 25 * 1024 * 1024;

/// A connection's byte stream, plaintext or TLS. `TlsStream` is boxed because
/// it is much larger than a bare `TcpStream`.
enum Transport {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl AsyncRead for Transport {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Transport::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Transport::Tls(s) => Pin::new(&mut **s).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Transport {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Transport::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Transport::Tls(s) => Pin::new(&mut **s).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Transport::Plain(s) => Pin::new(s).poll_flush(cx),
            Transport::Tls(s) => Pin::new(&mut **s).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Transport::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Transport::Tls(s) => Pin::new(&mut **s).poll_shutdown(cx),
        }
    }
}

/// Drive one SMTP conversation to completion (QUIT or disconnect).
///
/// When `implicit_tls` is set, the TLS handshake runs before any SMTP. Otherwise
/// the session starts plaintext and advertises STARTTLS, upgrading on request.
pub async fn handle(
    tcp: TcpStream,
    peer: SocketAddr,
    port: u16,
    tx: UnboundedSender<ReceivedMessage>,
    counter: Arc<AtomicU64>,
    acceptor: TlsAcceptor,
    implicit_tls: bool,
) -> anyhow::Result<()> {
    let transport = if implicit_tls {
        Transport::Tls(Box::new(acceptor.accept(tcp).await?))
    } else {
        Transport::Plain(tcp)
    };
    let mut stream = BufReader::new(transport);
    let mut is_tls = implicit_tls;

    reply(&mut stream, b"220 mock-smtp ESMTP ready\r\n").await?;

    let mut mail_from = String::new();
    let mut rcpt_to: Vec<String> = Vec::new();
    let mut line = String::new();

    loop {
        line.clear();
        if stream.read_line(&mut line).await? == 0 {
            break; // client disconnected
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let (cmd, rest) = match trimmed.split_once(' ') {
            Some((c, r)) => (c.to_ascii_uppercase(), r.trim()),
            None => (trimmed.to_ascii_uppercase(), ""),
        };

        match cmd.as_str() {
            "EHLO" => reply(&mut stream, ehlo_banner(rest, is_tls).as_bytes()).await?,
            "HELO" => reply(&mut stream, b"250 mock-smtp\r\n").await?,
            "MAIL" => {
                mail_from = extract_addr(rest);
                rcpt_to.clear();
                reply(&mut stream, b"250 OK\r\n").await?;
            }
            "RCPT" => {
                rcpt_to.push(extract_addr(rest));
                reply(&mut stream, b"250 OK\r\n").await?;
            }
            "DATA" => {
                reply(&mut stream, b"354 End data with <CR><LF>.<CR><LF>\r\n").await?;
                let raw = read_data(&mut stream).await?;
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
                reply(&mut stream, format!("250 OK: queued as {id}\r\n").as_bytes()).await?;
            }
            "STARTTLS" if !is_tls => {
                // Guard against the STARTTLS plaintext-injection attack: nothing
                // the client sent before the handshake may be trusted afterward.
                if !stream.buffer().is_empty() {
                    reply(&mut stream, b"501 Pipelining after STARTTLS rejected\r\n").await?;
                    break;
                }
                reply(&mut stream, b"220 Ready to start TLS\r\n").await?;

                let Transport::Plain(tcp) = stream.into_inner() else {
                    unreachable!("STARTTLS only reachable while plaintext");
                };
                stream = BufReader::new(Transport::Tls(Box::new(acceptor.accept(tcp).await?)));
                is_tls = true;
                // RFC 3207: discard all state; the client must EHLO again.
                mail_from.clear();
                rcpt_to.clear();
            }
            "STARTTLS" => reply(&mut stream, b"503 TLS already active\r\n").await?,
            "RSET" => {
                mail_from.clear();
                rcpt_to.clear();
                reply(&mut stream, b"250 OK\r\n").await?;
            }
            "NOOP" => reply(&mut stream, b"250 OK\r\n").await?,
            "VRFY" | "EXPN" => reply(&mut stream, b"252 Cannot verify\r\n").await?,
            "AUTH" => handle_auth(&mut stream, rest).await?,
            "QUIT" => {
                reply(&mut stream, b"221 Bye\r\n").await?;
                break;
            }
            "" => {}
            _ => reply(&mut stream, b"500 Unrecognized command\r\n").await?,
        }
    }

    Ok(())
}

/// The multi-line EHLO capability list. `STARTTLS` is offered only while still
/// plaintext; AUTH is always offered (and always accepted).
fn ehlo_banner(client: &str, is_tls: bool) -> String {
    let mut banner = format!(
        "250-mock-smtp greets {client}\r\n\
         250-SIZE {MAX_MESSAGE}\r\n\
         250-8BITMIME\r\n\
         250-SMTPUTF8\r\n\
         250-AUTH PLAIN LOGIN\r\n"
    );
    if !is_tls {
        banner.push_str("250-STARTTLS\r\n");
    }
    banner.push_str("250 HELP\r\n");
    banner
}

/// Write a response and flush it. Flushing matters for TLS, whose encrypted
/// bytes may otherwise linger in the rustls buffer instead of reaching the wire.
async fn reply<S>(stream: &mut S, bytes: &[u8]) -> anyhow::Result<()>
where
    S: AsyncWriteExt + Unpin,
{
    stream.write_all(bytes).await?;
    stream.flush().await?;
    Ok(())
}

/// Read the DATA payload until the terminating `<CRLF>.<CRLF>`, undoing
/// dot-stuffing (a leading `.` doubled by the client) as we go. Lines past
/// [`MAX_MESSAGE`] are dropped but still consumed so the stream stays in sync.
async fn read_data<S>(stream: &mut S) -> anyhow::Result<Vec<u8>>
where
    S: AsyncBufReadExt + Unpin,
{
    let mut raw = Vec::new();
    let mut line = Vec::new();

    loop {
        line.clear();
        if stream.read_until(b'\n', &mut line).await? == 0 {
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
async fn handle_auth<S>(stream: &mut S, rest: &str) -> anyhow::Result<()>
where
    S: AsyncBufReadExt + AsyncWriteExt + Unpin,
{
    let mechanism = rest
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    let mut scratch = String::new();

    match mechanism.as_str() {
        "LOGIN" => {
            // "334 Username:" / "334 Password:" (base64-encoded prompts).
            reply(stream, b"334 VXNlcm5hbWU6\r\n").await?;
            read_one(stream, &mut scratch).await?;
            reply(stream, b"334 UGFzc3dvcmQ6\r\n").await?;
            read_one(stream, &mut scratch).await?;
        }
        "PLAIN" => {
            // Credentials may be inline ("AUTH PLAIN <b64>") or on the next line.
            let has_inline = rest.split_whitespace().nth(1).is_some();
            if !has_inline {
                reply(stream, b"334 \r\n").await?;
                read_one(stream, &mut scratch).await?;
            }
        }
        _ => {}
    }

    reply(stream, b"235 2.7.0 Authentication successful\r\n").await?;
    Ok(())
}

/// Read and discard one line of client input (used for AUTH continuations).
async fn read_one<S>(stream: &mut S, buf: &mut String) -> anyhow::Result<()>
where
    S: AsyncBufReadExt + Unpin,
{
    buf.clear();
    stream.read_line(buf).await?;
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

    #[test]
    fn ehlo_advertises_starttls_only_in_plaintext() {
        assert!(ehlo_banner("c", false).contains("STARTTLS"));
        assert!(!ehlo_banner("c", true).contains("STARTTLS"));
        assert!(ehlo_banner("c", true).contains("AUTH"));
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

    /// Run one MAIL→DATA exchange over an already-open stream and assert the
    /// `250` acknowledgements come back.
    async fn send_message<S>(stream: &mut S, body: &[u8])
    where
        S: AsyncBufReadExt + AsyncWriteExt + Unpin,
    {
        stream.write_all(b"MAIL FROM:<alice@example.com>\r\n").await.unwrap();
        stream.flush().await.unwrap();
        assert!(read_reply(stream).await.starts_with("250"));
        stream.write_all(b"RCPT TO:<bob@example.com>\r\n").await.unwrap();
        stream.flush().await.unwrap();
        assert!(read_reply(stream).await.starts_with("250"));
        stream.write_all(b"DATA\r\n").await.unwrap();
        stream.flush().await.unwrap();
        assert!(read_reply(stream).await.starts_with("354"));
        stream.write_all(body).await.unwrap();
        stream.flush().await.unwrap();
        assert!(read_reply(stream).await.contains("250"));
    }

    const BODY: &[u8] = b"From: Alice <alice@example.com>\r\n\
                          To: Bob <bob@example.com>\r\n\
                          Subject: Hello there\r\n\
                          \r\n\
                          This is the body.\r\n\
                          .\r\n";

    fn assert_parsed(message: &ReceivedMessage) {
        assert_eq!(message.subject, "Hello there");
        assert!(message.from.contains("alice@example.com"));
        assert_eq!(message.envelope.mail_from, "alice@example.com");
        assert_eq!(message.envelope.rcpt_to, vec!["bob@example.com"]);
        assert!(message.text_body.as_deref().unwrap().contains("This is the body."));
    }

    #[tokio::test]
    async fn full_transaction_yields_parsed_message() {
        let acceptor = crate::tls::self_signed_acceptor().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let counter = Arc::new(AtomicU64::new(1));

        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            handle(stream, peer, addr.port(), tx, counter, acceptor, false)
                .await
                .unwrap();
        });

        let client = TcpStream::connect(addr).await.unwrap();
        let mut stream = BufReader::new(client);
        assert!(read_reply(&mut stream).await.starts_with("220"));
        stream.write_all(b"EHLO tester\r\n").await.unwrap();
        stream.flush().await.unwrap();
        let ehlo = read_reply(&mut stream).await;
        assert!(ehlo.contains("250") && ehlo.contains("AUTH"));

        send_message(&mut stream, BODY).await;
        stream.write_all(b"QUIT\r\n").await.unwrap();
        stream.flush().await.unwrap();
        assert!(read_reply(&mut stream).await.starts_with("221"));

        assert_parsed(&rx.recv().await.expect("message delivered"));
    }

    #[tokio::test]
    async fn starttls_upgrade_then_receive() {
        use tokio_rustls::TlsConnector;
        use tokio_rustls::rustls::ClientConfig;
        use tokio_rustls::rustls::crypto::ring;
        use tokio_rustls::rustls::pki_types::ServerName;

        let acceptor = crate::tls::self_signed_acceptor().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let counter = Arc::new(AtomicU64::new(1));

        tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            handle(stream, peer, addr.port(), tx, counter, acceptor, false)
                .await
                .unwrap();
        });

        // --- plaintext phase ---
        let tcp = TcpStream::connect(addr).await.unwrap();
        let mut stream = BufReader::new(tcp);
        assert!(read_reply(&mut stream).await.starts_with("220"));
        stream.write_all(b"EHLO tester\r\n").await.unwrap();
        stream.flush().await.unwrap();
        assert!(read_reply(&mut stream).await.contains("STARTTLS"));
        stream.write_all(b"STARTTLS\r\n").await.unwrap();
        stream.flush().await.unwrap();
        assert!(read_reply(&mut stream).await.starts_with("220"));
        assert!(stream.buffer().is_empty(), "no plaintext should remain buffered");

        // --- TLS handshake (trusting the self-signed cert) ---
        let config = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(no_verify::NoVerify::new()))
            .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(config));
        let name = ServerName::try_from("localhost").unwrap();
        let tls = connector.connect(name, stream.into_inner()).await.unwrap();

        // --- encrypted phase: re-EHLO, then send ---
        let mut stream = BufReader::new(tls);
        stream.write_all(b"EHLO tester\r\n").await.unwrap();
        stream.flush().await.unwrap();
        let ehlo = read_reply(&mut stream).await;
        assert!(ehlo.contains("250"));
        assert!(!ehlo.contains("STARTTLS"), "STARTTLS must not be re-offered over TLS");

        send_message(&mut stream, BODY).await;
        stream.write_all(b"QUIT\r\n").await.unwrap();
        stream.flush().await.unwrap();

        assert_parsed(&rx.recv().await.expect("message delivered over TLS"));
    }

    /// A client-side verifier that trusts any certificate — only for tests
    /// against our own self-signed server.
    mod no_verify {
        use tokio_rustls::rustls::client::danger::{
            HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
        };
        use tokio_rustls::rustls::crypto::ring;
        use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
        use tokio_rustls::rustls::{DigitallySignedStruct, Error, SignatureScheme};

        #[derive(Debug)]
        pub struct NoVerify {
            schemes: Vec<SignatureScheme>,
        }

        impl NoVerify {
            pub fn new() -> Self {
                Self {
                    schemes: ring::default_provider()
                        .signature_verification_algorithms
                        .supported_schemes(),
                }
            }
        }

        impl ServerCertVerifier for NoVerify {
            fn verify_server_cert(
                &self,
                _end_entity: &CertificateDer<'_>,
                _intermediates: &[CertificateDer<'_>],
                _server_name: &ServerName<'_>,
                _ocsp: &[u8],
                _now: UnixTime,
            ) -> Result<ServerCertVerified, Error> {
                Ok(ServerCertVerified::assertion())
            }

            fn verify_tls12_signature(
                &self,
                _message: &[u8],
                _cert: &CertificateDer<'_>,
                _dss: &DigitallySignedStruct,
            ) -> Result<HandshakeSignatureValid, Error> {
                Ok(HandshakeSignatureValid::assertion())
            }

            fn verify_tls13_signature(
                &self,
                _message: &[u8],
                _cert: &CertificateDer<'_>,
                _dss: &DigitallySignedStruct,
            ) -> Result<HandshakeSignatureValid, Error> {
                Ok(HandshakeSignatureValid::assertion())
            }

            fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
                self.schemes.clone()
            }
        }
    }
}
