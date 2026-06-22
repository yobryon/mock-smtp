//! All ratatui rendering. Pure functions of [`App`] state — no mutation here.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap,
};

use crate::app::{App, BodyView};
use crate::message::ReceivedMessage;

const ACCENT: Color = Color::Cyan;

pub fn draw(frame: &mut Frame, app: &App) {
    let [body, status] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    let [list_area, detail_area] =
        Layout::horizontal([Constraint::Percentage(38), Constraint::Min(0)]).areas(body);

    draw_list(frame, app, list_area);
    draw_detail(frame, app, detail_area);
    draw_status(frame, app, status);

    if app.show_help {
        draw_help(frame, frame.area());
    }
}

fn draw_list(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            format!(" Inbox ({}) ", app.store.len()),
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));

    if app.store.is_empty() {
        let mut lines = vec![
            Line::raw(""),
            Line::from(" Waiting for mail…".dim()),
            Line::raw(""),
            Line::from(" Point a client at one of the".dim()),
            Line::from(" listening ports below.".dim()),
        ];
        // Explain any ports we couldn't claim (commonly privileged ports).
        let failed: Vec<_> = app.binds.iter().filter(|b| !b.ok).collect();
        if !failed.is_empty() {
            lines.push(Line::raw(""));
            lines.push(Line::from(" Skipped:".red()));
            for b in failed {
                let reason = b.error.as_deref().unwrap_or("unavailable");
                lines.push(Line::from(format!("  {} — {}", b.port, reason).dim()));
            }
        }
        frame.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
        return;
    }

    let items: Vec<ListItem> = app
        .store
        .iter()
        .map(|m| {
            let time = m.received_at.format("%H:%M:%S").to_string();
            let top = Line::from(vec![
                Span::styled(time, Style::new().fg(Color::DarkGray)),
                Span::raw("  "),
                Span::styled(
                    truncate(m.display_from(), 26),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
            ]);
            let bottom = Line::from(Span::raw(format!("   {}", truncate(m.display_subject(), 32))));
            ListItem::new(vec![top, bottom])
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::new()
                .bg(ACCENT)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▌");

    let mut state = ListState::default();
    state.select(Some(app.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_detail(frame: &mut Frame, app: &App, area: Rect) {
    let Some(message) = app.store.get(app.selected) else {
        let empty = Paragraph::new(Line::from(" No message selected".dim()))
            .block(Block::bordered().border_type(BorderType::Rounded));
        frame.render_widget(empty, area);
        return;
    };

    let [header_area, body_area] =
        Layout::vertical([Constraint::Length(8), Constraint::Min(0)]).areas(area);

    draw_headers(frame, message, header_area);
    draw_body(frame, app, message, body_area);
}

fn draw_headers(frame: &mut Frame, message: &ReceivedMessage, area: Rect) {
    // Prefer the parsed To header; fall back to the envelope recipients.
    let to = if message.to.is_empty() {
        message.envelope.rcpt_to.join(", ")
    } else {
        message.to.clone()
    };
    let mut lines = vec![
        field("From", message.display_from()),
        field("To", &to),
        field("Subject", message.display_subject()),
        field("Date", &message.date),
    ];

    // Envelope + connection provenance, dimmed.
    lines.push(Line::from(vec![
        Span::styled("Envelope ", Style::new().fg(Color::DarkGray)),
        Span::styled(
            format!(
                "{} → {}  ·  port {} from {}",
                blank_as(&message.envelope.mail_from, "?"),
                blank_as(&message.envelope.rcpt_to.join(", "), "?"),
                message.envelope.port,
                message.envelope.peer,
            ),
            Style::new().fg(Color::DarkGray),
        ),
    ]));

    if !message.attachments.is_empty() {
        let names: Vec<String> = message
            .attachments
            .iter()
            .map(|a| format!("{} ({}, {})", a.name, a.content_type, human_size(a.size)))
            .collect();
        lines.push(field("Attach", &names.join(", ")));
    }

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            format!(" Message #{} ", message.id),
            Style::new().fg(ACCENT),
        ));
    frame.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
}

fn draw_body(frame: &mut Frame, app: &App, message: &ReceivedMessage, area: Rect) {
    // Inner width drives HTML wrap; subtract the border columns.
    let width = area.width.saturating_sub(2) as usize;
    let content = match app.view {
        BodyView::Rendered => message.rendered_view(width),
        BodyView::Plain => message.plain_view(),
        BodyView::Source => message.raw_text(),
        BodyView::Headers => message.header_block(),
    };

    let title = Line::from(vec![
        Span::raw(" Body: "),
        Span::styled(app.view.label(), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::styled("  (Tab to switch) ", Style::new().fg(Color::DarkGray)),
    ]);

    let block = Block::bordered().border_type(BorderType::Rounded).title(title);
    let paragraph = Paragraph::new(content)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.body_scroll, 0));
    frame.render_widget(paragraph, area);
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![Span::styled(" Ports ", Style::new().fg(Color::Black).bg(ACCENT))];
    for bind in &app.binds {
        let style = if bind.ok {
            Style::new().fg(Color::Green)
        } else {
            Style::new().fg(Color::Red).add_modifier(Modifier::DIM)
        };
        let mark = if bind.ok { "●" } else { "○" };
        spans.push(Span::raw(" "));
        spans.push(Span::styled(format!("{mark}{}", bind.port), style));
    }
    spans.push(Span::styled(
        "   j/k move · Tab view · PgUp/PgDn scroll · d delete · X clear · ? help · q quit",
        Style::new().fg(Color::DarkGray),
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_help(frame: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from(" Keys ".bold().fg(ACCENT)),
        Line::raw(""),
        Line::raw("  j / ↓        next message"),
        Line::raw("  k / ↑        previous message"),
        Line::raw("  g / G        first / last"),
        Line::raw("  Tab          cycle body view (Rendered/Plain/Source/Headers)"),
        Line::raw("  Space/PgDn   scroll body down"),
        Line::raw("  PgUp         scroll body up"),
        Line::raw("  d            delete selected message"),
        Line::raw("  X            clear the whole queue"),
        Line::raw("  ? / q        toggle help / quit"),
        Line::raw(""),
        Line::from("  press any key to dismiss".dim()),
    ];
    let popup = centered_rect(64, 16, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(" Help ");
    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

// --- small helpers ---

fn field(name: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{name:<8} "), Style::new().fg(Color::DarkGray)),
        Span::raw(value.to_string()),
    ])
}

fn blank_as(value: &str, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

fn truncate(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}

fn human_size(bytes: usize) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// A centered rectangle `width`×`height` (in cells) clamped to `area`.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}
