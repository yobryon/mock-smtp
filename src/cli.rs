//! Command-line configuration.

use clap::Parser;

/// A mock SMTP server with a TUI inbox. Receives mail on local ports and shows
/// it in a navigable list/detail view. Nothing is ever delivered or persisted.
#[derive(Parser, Debug)]
#[command(name = "mock-smtp", version, about)]
pub struct Cli {
    /// Ports to listen on (comma-separated or repeated).
    ///
    /// Defaults cover the dev-friendly high ports plus the standard SMTP ports.
    /// Standard ports (25/465/587) require elevated privileges on Unix; any
    /// port that fails to bind is skipped and reported in the status bar.
    #[arg(
        short,
        long,
        value_delimiter = ',',
        default_values_t = [1025u16, 2525, 25, 587, 465]
    )]
    pub ports: Vec<u16>,

    /// Interface address to bind the listeners to.
    #[arg(short, long, default_value = "0.0.0.0")]
    pub bind: String,

    /// Ports that use implicit TLS (SMTPS — TLS from the first byte).
    ///
    /// Every other listening port starts plaintext and offers STARTTLS. All TLS
    /// is backed by a self-signed cert minted at startup, so clients must
    /// disable certificate verification.
    #[arg(long, value_delimiter = ',', default_values_t = [465u16])]
    pub implicit_tls_ports: Vec<u16>,
}
