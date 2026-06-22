# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`mock-smtp` is a local, in-memory SMTP **sink** with a TUI inbox (Rust + tokio + ratatui). It receives mail on local ports and shows it in a list/detail view so a system under development can "send mail" somewhere observable. It never delivers, relays, persists, or does outbound SMTP — accepting a message means acknowledging it to the client and pushing it onto an in-memory queue that dies with the process.

## Commands

```bash
cargo run                       # launch the TUI on the default ports
cargo run -- -p 1025,2525       # custom ports (comma-separated or repeated -p)
cargo run -- -b 127.0.0.1       # bind a specific interface (default 0.0.0.0)
cargo build --release           # optimized binary at target/release/mock-smtp

cargo test                      # all tests
cargo test full_transaction     # one test by name substring
cargo clippy --all-targets      # lint (keep clean)
```

### Running / testing the TUI

`ratatui::init()` puts the terminal in raw mode + alternate screen, so the app **requires a TTY** — running it with stdout redirected (or headless) panics on init and kills the listeners. To smoke-test the live server, run it inside a pty and drive it with a real client:

```bash
script -qfec 'target/debug/mock-smtp -p 1026 -b 127.0.0.1' /dev/null &
python3 -c "import smtplib; smtplib.SMTP('127.0.0.1',1026).sendmail('a@x','b@y','Subject: hi\n\nbody')"
```

The protocol + parsing path is covered without a TTY by the integration-style test in `src/smtp/session.rs` (`full_transaction_yields_parsed_message`), which connects a real client to `session::handle` over a loopback socket — prefer adding to that when changing SMTP behavior.

## Architecture

Two halves connected by one channel:

- **SMTP listeners** (`src/smtp/`) — async, on the tokio runtime. `server::spawn_all` attempts to bind every requested port and spawns an accept loop per success; `session::handle` runs one permissive ESMTP state machine per connection. On `DATA` completion it builds a `ReceivedMessage` (parsing inline) and sends it over an `mpsc::UnboundedSender`.
- **TUI** (`src/app.rs`, `src/ui.rs`) — owns the receiver and the `Store`. `App::run` is a `tokio::select!` loop over crossterm's async `EventStream` (keys) and the message channel (new mail), redrawing after either.

```
main.rs ──spawn_all──▶ listeners (one task/port → one task/conn)
   │                        │  ReceivedMessage::parse (mail-parser)
   │                        ▼
   │                  mpsc::Unbounded
   ▼                        │
App::run (select!) ◀────────┘   + crossterm EventStream
   └─ Store (Vec, newest-first) ──▶ ui::draw
```

Key design points:

- **Threading model is single-owner, not shared-state.** The `Store` lives in the UI loop and is only mutated there; listeners never touch it. New mail crosses the thread boundary exclusively through the channel, so there are no locks. Don't introduce an `Arc<Mutex<Store>>` — keep mutations in `App`.
- **Message ids** come from one process-wide `Arc<AtomicU64>` shared by every connection (`main.rs` → `spawn_all` → each session). The id doubles as the SMTP `250 OK: queued as N` token.
- **Parsing happens once, at receive time** (`ReceivedMessage::parse`), never during draw — except HTML flattening (`rendered_view`), which depends on the live pane width and is recomputed per frame via `html2text`. The original bytes are kept in `raw` for the "Source"/"Headers" views, which slice the raw text rather than reconstructing from the parse tree.
- **Listeners are best-effort.** A failed bind (privileged ports 25/465/587 need elevated privileges) is non-fatal: it becomes a `BindStatus { ok: false, error }` shown in the status bar / empty-inbox hint. The app runs on whatever ports it got.
- **The SMTP server is intentionally permissive**, not RFC-strict. It speaks just enough to satisfy real clients: it advertises and **accepts any AUTH** (PLAIN/LOGIN, credentials read and discarded) so clients configured to authenticate still go through, and it refuses `STARTTLS` (`454`) — plaintext only, by design. There is no TLS support (no implicit-TLS on 465, no STARTTLS upgrade); a TLS-only sender won't connect.

### Module map

| File | Responsibility |
|------|----------------|
| `main.rs` | Wire CLI → channel → listeners → TUI; own terminal init/restore |
| `cli.rs` | clap args (`--ports`, `--bind`) |
| `smtp/server.rs` | Bind ports, `BindStatus`, accept loop |
| `smtp/session.rs` | Per-connection ESMTP state machine; DATA/dot-stuffing/AUTH; **tests live here** |
| `message.rs` | `ReceivedMessage` + `Envelope`; mail-parser MIME extraction; body views |
| `store.rs` | In-memory newest-first `Vec` queue + ops |
| `app.rs` | `App` state, `BodyView`, the select! event loop, key handling |
| `ui.rs` | All ratatui drawing — pure functions of `App`, no mutation |

## Conventions

- **`ui.rs` renders, it does not mutate.** All state changes go through `App` methods driven by `on_key`. Keep new keybindings there and reflect them in the status bar (`draw_status`) and help overlay (`draw_help`).
- **Crate versions are recent and API-churny** (ratatui 0.30, crossterm 0.29, mail-parser 0.11, html2text 0.17). When unsure of a signature, check the vendored source under `~/.cargo/registry/src/.../<crate>-<version>/` rather than guessing — several of these renamed methods between minor versions.
- **Edition 2024** — let-chains (`if let ... && cond`) are in use; keep clippy clean.
