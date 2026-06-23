//! Listener binding and the per-port accept loop.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use tokio::net::TcpListener;
use tokio::sync::mpsc::UnboundedSender;
use tokio_rustls::TlsAcceptor;

use crate::message::ReceivedMessage;

/// The result of attempting to bind one port, surfaced in the TUI status bar.
pub struct BindStatus {
    pub port: u16,
    pub ok: bool,
    /// Whether this port speaks implicit TLS (SMTPS) rather than plaintext +
    /// optional STARTTLS.
    pub implicit_tls: bool,
    /// Why the bind failed (e.g. permission denied, address in use).
    pub error: Option<String>,
}

/// Attempt to bind every requested port and spawn an accept loop for each that
/// succeeds. Returns one [`BindStatus`] per port (in request order) so the UI
/// can show which listeners are live and which were skipped.
///
/// Ports listed in `implicit_tls_ports` start their TLS handshake immediately;
/// every other port begins plaintext and offers STARTTLS. A failed bind
/// (commonly a privileged port without `CAP_NET_BIND_SERVICE`) is never fatal.
pub async fn spawn_all(
    bind: &str,
    ports: &[u16],
    implicit_tls_ports: &[u16],
    tx: UnboundedSender<ReceivedMessage>,
    counter: Arc<AtomicU64>,
    acceptor: TlsAcceptor,
) -> Vec<BindStatus> {
    // Listen on the union of both sets: an implicit-TLS port is always bound,
    // even if the caller didn't also list it in `--ports`.
    let mut all_ports = ports.to_vec();
    for &port in implicit_tls_ports {
        if !all_ports.contains(&port) {
            all_ports.push(port);
        }
    }

    let mut statuses = Vec::with_capacity(all_ports.len());

    for &port in &all_ports {
        let implicit_tls = implicit_tls_ports.contains(&port);
        match TcpListener::bind((bind, port)).await {
            Ok(listener) => {
                statuses.push(BindStatus {
                    port,
                    ok: true,
                    implicit_tls,
                    error: None,
                });
                tokio::spawn(accept_loop(
                    listener,
                    port,
                    implicit_tls,
                    tx.clone(),
                    counter.clone(),
                    acceptor.clone(),
                ));
            }
            Err(e) => statuses.push(BindStatus {
                port,
                ok: false,
                implicit_tls,
                error: Some(e.to_string()),
            }),
        }
    }

    statuses
}

/// Accept connections forever, handling each in its own task. Per-connection
/// errors are swallowed; one misbehaving client must not take down the port.
async fn accept_loop(
    listener: TcpListener,
    port: u16,
    implicit_tls: bool,
    tx: UnboundedSender<ReceivedMessage>,
    counter: Arc<AtomicU64>,
    acceptor: TlsAcceptor,
) {
    loop {
        let Ok((stream, peer)) = listener.accept().await else {
            continue;
        };
        let tx = tx.clone();
        let counter = counter.clone();
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            let _ = session::handle(stream, peer, port, tx, counter, acceptor, implicit_tls).await;
        });
    }
}

use super::session;
