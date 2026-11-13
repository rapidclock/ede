//! Bounded background-to-UI messages. Producers never draw or mutate application state.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

/// Maximum retained messages; input and signal handling never use this queue.
const CAPACITY: usize = 64;

/// A small status message; control characters and unbounded payloads are excluded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusMessage {
    /// Severity controls presentation, accompanied by a text label for warnings/errors.
    pub severity: Severity,
    text: String,
}

/// Severity of a status notification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Severity {
    /// Ordinary informational feedback.
    Info,
    /// An operation could not proceed as expected.
    Warning,
    /// An operation failed.
    Error,
}

impl StatusMessage {
    /// Retain at most 256 printable characters; callers must not supply document or paste content.
    pub fn new(severity: Severity, text: &str) -> Self {
        Self {
            severity,
            text: crate::text::safe(&text.chars().take(256).collect::<String>())
                .chars()
                .take(256)
                .collect(),
        }
    }

    /// The bounded, single-line notification.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// One pending notification, optionally replacing progress for the same operation.
struct Pending {
    progress: Option<u64>,
    message: StatusMessage,
}

/// Thread-safe shared queue, held briefly only while adding/removing small messages.
type Queue = Arc<Mutex<VecDeque<Pending>>>;

/// Cloneable producer. Full-queue errors require the producer to retry or cancel its work.
#[derive(Clone)]
pub struct StatusSender(Queue);

/// Consumer owned by the UI thread; no blocking receive is exposed.
pub struct StatusInbox(Queue);

/// Construct a fixed-capacity mailbox without spawning a worker.
pub fn mailbox() -> (StatusSender, StatusInbox) {
    let queue = Arc::new(Mutex::new(VecDeque::with_capacity(CAPACITY)));
    (StatusSender(queue.clone()), StatusInbox(queue))
}

impl StatusSender {
    /// Publish completion/error feedback. On saturation, return ownership for explicit retry.
    pub fn try_send(&self, message: StatusMessage) -> Result<(), StatusMessage> {
        self.publish(None, message)
    }

    /// Coalesce progress for an operation. On saturation, the caller may skip this progress update.
    pub fn try_progress(
        &self,
        operation: u64,
        message: StatusMessage,
    ) -> Result<(), StatusMessage> {
        self.publish(Some(operation), message)
    }

    /// Replace matching progress or append while preserving the fixed memory bound.
    fn publish(&self, progress: Option<u64>, message: StatusMessage) -> Result<(), StatusMessage> {
        let mut queue = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        if progress.is_some()
            && let Some(pending) = queue
                .iter_mut()
                .find(|pending| pending.progress == progress)
        {
            pending.message = message;
            return Ok(());
        }
        if queue.len() == CAPACITY {
            return Err(message);
        }
        queue.push_back(Pending { progress, message });
        Ok(())
    }
}

impl StatusInbox {
    /// Take one available message, immediately returning `None` when empty.
    pub(crate) fn pop(&self) -> Option<StatusMessage> {
        self.0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .pop_front()
            .map(|pending| pending.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Saturation is explicit, progress coalesces, and ordinary notifications stay ordered.
    #[test]
    fn bounded_delivery_and_coalescing() {
        let (sender, inbox) = mailbox();
        for value in 0..100 {
            sender
                .try_progress(1, StatusMessage::new(Severity::Info, &value.to_string()))
                .unwrap();
        }
        for _ in 1..CAPACITY {
            sender
                .try_send(StatusMessage::new(Severity::Info, "complete"))
                .unwrap();
        }
        let error = StatusMessage::new(Severity::Error, "retry me");
        assert_eq!(sender.try_send(error.clone()), Err(error.clone()));
        assert_eq!(inbox.pop().unwrap().text(), "99");
        sender.try_send(error).unwrap();
        for _ in 1..CAPACITY {
            assert_eq!(inbox.pop().unwrap().text(), "complete");
        }
        assert_eq!(inbox.pop().unwrap().text(), "retry me");
        assert!(inbox.pop().is_none());
    }

    /// Untrusted notifications cannot inject terminal escapes or grow without bound.
    #[test]
    fn text_is_bounded_and_printable() {
        let message = StatusMessage::new(Severity::Warning, &format!("\x1b\n{}", "é".repeat(500)));
        assert_eq!(message.text().chars().count(), 256);
        assert!(!message.text().chars().any(char::is_control));
    }
}
