# mock-smtp

A local, in-memory **mock SMTP server** with a clean terminal inbox. Point any
system that "sends mail" at it during development and watch what it actually
sends — headers, bodies, HTML, attachments — without delivering anything
anywhere.

Nothing is relayed, delivered, or written to disk. The queue lives in memory and
disappears when you quit.

## Run

```bash
cargo run
```

By default it listens on `1025, 2525, 25, 587, 465` (binding to `0.0.0.0`).
Privileged ports (25/465/587) need elevated privileges; any port it can't claim
is skipped and shown in the status bar. Configure with:

```bash
cargo run -- --ports 1025,2525 --bind 127.0.0.1
```

Then send mail to one of the live ports — e.g. set your app's SMTP host to
`localhost:1025`.

## Keys

| Key | Action |
|-----|--------|
| `j` / `k`, `↓` / `↑` | move through the inbox |
| `g` / `G` | jump to first / last |
| `Tab` | cycle body view: Rendered · Plain · Source · Headers |
| `Space` / `PgDn`, `PgUp` | scroll the body |
| `d` | delete the selected message |
| `X` | clear the whole queue |
| `?` | help · `q` / `Esc` quit |

## What it speaks

A permissive ESMTP conversation: `EHLO`/`HELO`, `MAIL`/`RCPT`/`DATA`,
`RSET`/`NOOP`/`QUIT`, and `AUTH` (accepted unconditionally so authenticating
clients still work). **Plaintext only** — there is no TLS/STARTTLS, so a
TLS-only sender won't connect.

Received messages are MIME-parsed: common headers, the text body, an
HTML-rendered-to-text view, and an attachment summary, alongside the raw source.
