//! The received-message model and MIME parsing.
//!
//! A [`ReceivedMessage`] couples the SMTP *envelope* (who the sending client
//! said the mail was from/to, plus connection metadata) with the *parsed*
//! content of the DATA payload. Parsing happens once, at receive time, so the
//! UI thread never does heavy work while drawing.

use std::net::SocketAddr;

use chrono::{DateTime, Local};
use mail_parser::{MessageParser, MimeHeaders};

/// Connection-level metadata captured during the SMTP conversation, before the
/// message body is known.
pub struct Envelope {
    pub port: u16,
    pub peer: SocketAddr,
    /// Address from `MAIL FROM:<...>`.
    pub mail_from: String,
    /// Addresses from each `RCPT TO:<...>`.
    pub rcpt_to: Vec<String>,
}

/// A summary of one MIME attachment (we keep the metadata, not the bytes, since
/// this is only for display).
pub struct Attachment {
    pub name: String,
    pub content_type: String,
    pub size: usize,
}

/// A fully received and parsed message held in the in-memory queue.
pub struct ReceivedMessage {
    pub id: u64,
    pub received_at: DateTime<Local>,
    pub envelope: Envelope,

    /// The raw RFC 5322 bytes exactly as received (dot-unstuffed).
    pub raw: Vec<u8>,

    // --- Parsed headers (best-effort; empty when absent or unparseable) ---
    pub from: String,
    pub to: String,
    pub subject: String,
    pub date: String,

    // --- Parsed body parts ---
    pub text_body: Option<String>,
    pub html_body: Option<String>,
    pub attachments: Vec<Attachment>,
}

impl ReceivedMessage {
    /// Parse a freshly received DATA payload into a displayable message.
    pub fn parse(id: u64, envelope: Envelope, raw: Vec<u8>) -> Self {
        let parsed = MessageParser::default().parse(&raw);

        let (from, to, subject, date, text_body, html_body, attachments) = match &parsed {
            Some(msg) => (
                msg.from().map(fmt_address).unwrap_or_default(),
                msg.to().map(fmt_address).unwrap_or_default(),
                msg.subject().unwrap_or_default().to_string(),
                msg.date().map(|d| d.to_rfc3339()).unwrap_or_default(),
                msg.body_text(0).map(|c| c.into_owned()),
                msg.body_html(0).map(|c| c.into_owned()),
                msg.attachments()
                    .map(|part| Attachment {
                        name: part.attachment_name().unwrap_or("(unnamed)").to_string(),
                        content_type: part
                            .content_type()
                            .map(|ct| match ct.subtype() {
                                Some(sub) => format!("{}/{}", ct.ctype(), sub),
                                None => ct.ctype().to_string(),
                            })
                            .unwrap_or_else(|| "application/octet-stream".to_string()),
                        size: part.len(),
                    })
                    .collect(),
            ),
            None => Default::default(),
        };

        Self {
            id,
            received_at: Local::now(),
            envelope,
            raw,
            from,
            to,
            subject,
            date,
            text_body,
            html_body,
            attachments,
        }
    }

    /// A short subject for list rows (never empty).
    pub fn display_subject(&self) -> &str {
        if self.subject.is_empty() {
            "(no subject)"
        } else {
            &self.subject
        }
    }

    /// A best-effort "from" for list rows, falling back to the envelope sender.
    pub fn display_from(&self) -> &str {
        if self.from.is_empty() {
            if self.envelope.mail_from.is_empty() {
                "(unknown sender)"
            } else {
                &self.envelope.mail_from
            }
        } else {
            &self.from
        }
    }

    /// The raw header block (everything before the first blank line), decoded
    /// loosely as UTF-8 for display.
    pub fn header_block(&self) -> String {
        let text = String::from_utf8_lossy(&self.raw);
        match text.find("\r\n\r\n").or_else(|| text.find("\n\n")) {
            Some(end) => text[..end].to_string(),
            None => text.into_owned(),
        }
    }

    /// The full raw message decoded loosely as UTF-8 for the "source" view.
    pub fn raw_text(&self) -> String {
        String::from_utf8_lossy(&self.raw).into_owned()
    }

    /// The plain-text body, or a flattened-from-HTML rendering, or a hint.
    pub fn plain_view(&self) -> String {
        if let Some(text) = &self.text_body {
            return text.clone();
        }
        if self.html_body.is_some() {
            return "(no text/plain part — switch view to render the HTML)".to_string();
        }
        "(empty body)".to_string()
    }

    /// The body rendered for reading: HTML flattened to terminal text via
    /// `html2text` when present, otherwise the plain-text body.
    ///
    /// `width` is the wrap width in columns (the detail pane's inner width).
    pub fn rendered_view(&self, width: usize) -> String {
        if let Some(html) = &self.html_body {
            let width = width.max(20);
            return html2text::from_read(html.as_bytes(), width)
                .unwrap_or_else(|e| format!("(failed to render HTML: {e})"));
        }
        self.plain_view()
    }
}

/// Format a parsed address header as `Name <email>, ...`.
fn fmt_address(addr: &mail_parser::Address) -> String {
    addr.iter()
        .map(|a| match (a.name(), a.address()) {
            (Some(name), Some(email)) => format!("{name} <{email}>"),
            (None, Some(email)) => email.to_string(),
            (Some(name), None) => name.to_string(),
            (None, None) => String::new(),
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}
