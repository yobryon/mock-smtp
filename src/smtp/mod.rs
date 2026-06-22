//! The SMTP receiver: one async listener per port, one task per connection.
//!
//! Listeners push fully parsed [`crate::message::ReceivedMessage`]s onto an
//! unbounded channel that the TUI drains. The server never delivers, relays, or
//! persists anything — it only acknowledges so that a real sending client
//! believes its mail was accepted.

pub mod server;
pub mod session;
