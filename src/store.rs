//! The in-memory message queue. No disk persistence — quitting drops everything.

use crate::message::ReceivedMessage;

/// Newest-first ordered list of received messages.
#[derive(Default)]
pub struct Store {
    /// Index 0 is the most recently received message.
    messages: Vec<ReceivedMessage>,
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&ReceivedMessage> {
        self.messages.get(index)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ReceivedMessage> {
        self.messages.iter()
    }

    /// Insert a newly received message at the front (newest-first).
    pub fn push_front(&mut self, message: ReceivedMessage) {
        self.messages.insert(0, message);
    }

    /// Remove the message at `index`, if present.
    pub fn remove(&mut self, index: usize) {
        if index < self.messages.len() {
            self.messages.remove(index);
        }
    }

    /// Drop every message.
    pub fn clear(&mut self) {
        self.messages.clear();
    }
}
