//! mock-smtp — a local, in-memory SMTP sink with a TUI inbox.
//!
//! Wiring: CLI → bind listeners → run the TUI. The SMTP listeners and the TUI
//! communicate over an unbounded channel; an `AtomicU64` hands out message ids
//! across all connections.

mod app;
mod cli;
mod message;
mod smtp;
mod store;
mod tls;
mod ui;

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use clap::Parser;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();

    let (tx, rx) = mpsc::unbounded_channel();
    let counter = Arc::new(AtomicU64::new(1));
    let acceptor = tls::self_signed_acceptor()?;
    let binds = smtp::server::spawn_all(
        &cli.bind,
        &cli.ports,
        &cli.implicit_tls_ports,
        tx,
        counter,
        acceptor,
    )
    .await;

    // Hand the terminal over to ratatui (raw mode + alternate screen) and make
    // sure we always restore it, even if the app loop errors out.
    let mut terminal = ratatui::init();
    let mut app = app::App::new(rx, binds);
    let result = app.run(&mut terminal).await;
    ratatui::restore();

    result
}
