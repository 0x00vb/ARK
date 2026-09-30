//! Steering: messages a human sends while a turn is still running. A front-end holds a
//! [`Steering`] handle and pushes text into it; the harness takes whatever is waiting at its
//! next step boundary and hands it to the model as a user message, then keeps working.
//!
//! Delivery is never mid-call. A message pushed while the model is thinking waits for that
//! call to finish. If the turn ends before another step starts, the message was not
//! delivered: [`Steering::take_undelivered`] returns it so the front-end can send it as the
//! next turn instead.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub struct Steering {
    queue: Arc<Mutex<VecDeque<String>>>,
}

impl Steering {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a message for the running turn. Blank text is ignored.
    pub fn push(&self, text: impl Into<String>) {
        let text = text.into();
        if !text.trim().is_empty() {
            self.queue.lock().unwrap().push_back(text);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.queue.lock().unwrap().is_empty()
    }

    /// Everything waiting, oldest first. The harness calls this at a step boundary.
    pub(crate) fn drain(&self) -> Vec<String> {
        self.queue.lock().unwrap().drain(..).collect()
    }

    /// Messages that arrived too late for the turn. Call after the turn ends.
    pub fn take_undelivered(&self) -> Vec<String> {
        self.drain()
    }
}

/// Drain an optional handle.
pub(crate) fn drain(s: &Option<Steering>) -> Vec<String> {
    s.as_ref().map(|s| s.drain()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_drain_in_order_and_skip_blank() {
        let s = Steering::new();
        s.push("first");
        s.push("   ");
        s.push("second");
        assert_eq!(s.drain(), vec!["first", "second"]);
        assert!(s.is_empty());
        assert!(s.take_undelivered().is_empty());
    }
}
