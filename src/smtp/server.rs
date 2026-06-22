//! Listener binding and the per-port accept loop.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use tokio::net::TcpListener;
use tokio::sync::mpsc::UnboundedSender;

use crate::message::ReceivedMessage;

/// The result of attempting to bind one port, surfaced in the TUI status bar.
pub struct BindStatus {
    pub port: u16,
    pub ok: bool,
    /// Why the bind failed (e.g. permission denied, address in use).
    pub error: Option<String>,
}

/// Attempt to bind every requested port and spawn an accept loop for each that
/// succeeds. Returns one [`BindStatus`] per port (in request order) so the UI
/// can show which listeners are live and which were skipped.
///
/// A failed bind (commonly a privileged port without `CAP_NET_BIND_SERVICE`) is
/// never fatal — the mock keeps running on whatever ports it got.
pub async fn spawn_all(
    bind: &str,
    ports: &[u16],
    tx: UnboundedSender<ReceivedMessage>,
    counter: Arc<AtomicU64>,
) -> Vec<BindStatus> {
    let mut statuses = Vec::with_capacity(ports.len());

    for &port in ports {
        match TcpListener::bind((bind, port)).await {
            Ok(listener) => {
                statuses.push(BindStatus {
                    port,
                    ok: true,
                    error: None,
                });
                tokio::spawn(accept_loop(listener, port, tx.clone(), counter.clone()));
            }
            Err(e) => statuses.push(BindStatus {
                port,
                ok: false,
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
    tx: UnboundedSender<ReceivedMessage>,
    counter: Arc<AtomicU64>,
) {
    loop {
        let Ok((stream, peer)) = listener.accept().await else {
            continue;
        };
        let tx = tx.clone();
        let counter = counter.clone();
        tokio::spawn(async move {
            let _ = session::handle(stream, peer, port, tx, counter).await;
        });
    }
}

use super::session;
