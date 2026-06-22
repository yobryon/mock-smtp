//! Application state and the merged event loop.
//!
//! The loop selects over two sources: terminal input (via crossterm's async
//! [`EventStream`]) and newly received mail (via the channel the SMTP listeners
//! push onto). A redraw happens after either fires, so the inbox updates live.

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::message::ReceivedMessage;
use crate::smtp::server::BindStatus;
use crate::store::Store;
use crate::ui;

/// Which rendering of the selected message's body the detail pane shows.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BodyView {
    /// HTML flattened to readable text (or plain text when there's no HTML).
    Rendered,
    /// The raw text/plain part.
    Plain,
    /// The full raw RFC 5322 source.
    Source,
    /// Just the header block.
    Headers,
}

impl BodyView {
    pub fn label(self) -> &'static str {
        match self {
            BodyView::Rendered => "Rendered",
            BodyView::Plain => "Plain",
            BodyView::Source => "Source",
            BodyView::Headers => "Headers",
        }
    }

    /// Cycle to the next view (Tab).
    fn next(self) -> Self {
        match self {
            BodyView::Rendered => BodyView::Plain,
            BodyView::Plain => BodyView::Source,
            BodyView::Source => BodyView::Headers,
            BodyView::Headers => BodyView::Rendered,
        }
    }
}

pub struct App {
    rx: UnboundedReceiver<ReceivedMessage>,
    pub store: Store,
    pub binds: Vec<BindStatus>,
    pub selected: usize,
    pub body_scroll: u16,
    pub view: BodyView,
    pub show_help: bool,
    should_quit: bool,
}

impl App {
    pub fn new(rx: UnboundedReceiver<ReceivedMessage>, binds: Vec<BindStatus>) -> Self {
        Self {
            rx,
            store: Store::new(),
            binds,
            selected: 0,
            body_scroll: 0,
            view: BodyView::Rendered,
            show_help: false,
            should_quit: false,
        }
    }

    /// Run the draw/event loop until the user quits.
    pub async fn run(&mut self, terminal: &mut DefaultTerminal) -> anyhow::Result<()> {
        let mut events = EventStream::new();

        while !self.should_quit {
            terminal.draw(|frame| ui::draw(frame, self))?;

            tokio::select! {
                Some(message) = self.rx.recv() => self.on_new_message(message),
                maybe = events.next() => match maybe {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        self.on_key(key);
                    }
                    Some(Ok(_)) => {}        // resize, mouse, focus — redraw covers it
                    Some(Err(_)) | None => break, // terminal closed
                },
            }
        }

        Ok(())
    }

    /// Insert newly received mail, keeping the selection anchored on whatever
    /// the user was reading (unless they were already at the top, in which case
    /// they follow the newest arrival).
    fn on_new_message(&mut self, message: ReceivedMessage) {
        self.store.push_front(message);
        if self.selected != 0 {
            self.selected += 1;
        } else {
            // Pinned to the top: stay on the new newest, reset scroll.
            self.body_scroll = 0;
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        // Any key dismisses the help overlay first.
        if self.show_help {
            self.show_help = false;
            return;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('c') if ctrl => self.should_quit = true,

            KeyCode::Char('j') | KeyCode::Down => self.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.select_prev(),
            KeyCode::Char('g') | KeyCode::Home => self.select_to(0),
            KeyCode::Char('G') | KeyCode::End => self.select_to(usize::MAX),

            KeyCode::PageDown => self.body_scroll = self.body_scroll.saturating_add(10),
            KeyCode::PageUp => self.body_scroll = self.body_scroll.saturating_sub(10),
            KeyCode::Char(' ') => self.body_scroll = self.body_scroll.saturating_add(10),

            KeyCode::Tab => {
                self.view = self.view.next();
                self.body_scroll = 0;
            }

            KeyCode::Char('d') => self.delete_selected(),
            KeyCode::Char('X') => self.clear_all(),

            KeyCode::Char('?') => self.show_help = true,

            _ => {}
        }
    }

    fn select_next(&mut self) {
        if self.selected + 1 < self.store.len() {
            self.selected += 1;
            self.body_scroll = 0;
        }
    }

    fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.body_scroll = 0;
        }
    }

    fn select_to(&mut self, index: usize) {
        let last = self.store.len().saturating_sub(1);
        self.selected = index.min(last);
        self.body_scroll = 0;
    }

    fn delete_selected(&mut self) {
        if self.store.is_empty() {
            return;
        }
        self.store.remove(self.selected);
        let last = self.store.len().saturating_sub(1);
        self.selected = self.selected.min(last);
        self.body_scroll = 0;
    }

    fn clear_all(&mut self) {
        self.store.clear();
        self.selected = 0;
        self.body_scroll = 0;
    }
}
