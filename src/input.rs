//! Decoded input retains key identity until the current UI surface dispatches it.

use crate::{command::Command, document::Cancellation, spool::Spool};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent};
use std::sync::Arc;

/// Owned application input. Paste payloads are private disk files, never giant strings.
#[derive(Clone, Debug)]
pub(crate) enum Input {
    Key(KeyEvent),
    Command(Command),
    Text(String),
    Pointer(MouseEvent),
    Paste(Arc<Spool>),
    PasteStarted(Cancellation),
    Notice(String),
    PasteRejected(String),
    KeyboardSupported,
    FocusLost,
    FocusGained,
    Resize(u16, u16),
}

/// Unbound printable presses and repeats fall through to ordinary text input.
pub(crate) fn text(key: KeyEvent) -> Option<char> {
    if key.kind != KeyEventKind::Release
        && (key.modifiers - KeyModifiers::SHIFT).is_empty()
        && let KeyCode::Char(ch) = key.code
        && !ch.is_control()
    {
        Some(ch)
    } else {
        None
    }
}
