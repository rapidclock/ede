//! Device normalization retains text, selection modifiers, and delivered Cmd/Super.
use crate::{
    command::{BINDINGS, Command, Movement},
    document::Cancellation,
    spool::Spool,
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent};
use std::sync::Arc;

/// Owned application input. Paste payloads are private disk files, never giant strings.
#[derive(Clone, Debug)]
pub(crate) enum Input {
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
/// Normalize already decoded small events. Terminal paste must use streaming ingress.
pub(crate) fn normalize(event: Event) -> Option<Input> {
    match event {
        Event::Key(key) => {
            if let Some(command) = binding(key) {
                return Some(Input::Command(command));
            }
            if key.kind != KeyEventKind::Release
                && (key.modifiers - KeyModifiers::SHIFT).is_empty()
                && let KeyCode::Char(ch) = key.code
                && !ch.is_control()
            {
                return Some(Input::Text(ch.to_string()));
            }
            None
        }
        Event::Mouse(mouse) => Some(Input::Pointer(mouse)),
        Event::Paste(_) => Some(Input::PasteRejected(
            "Non-streaming paste source refused; use terminal ingress".into(),
        )),
        Event::FocusLost => Some(Input::FocusLost),
        Event::FocusGained => Some(Input::FocusGained),
        Event::Resize(w, h) => Some(Input::Resize(w, h)),
    }
}
/// Resolve exact modifiers and explicit navigation combinations.
fn binding(mut key: KeyEvent) -> Option<Command> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
        && let KeyCode::Char(letter) = key.code
    {
        key.code = KeyCode::Char(letter.to_ascii_lowercase());
    }
    if key.code == KeyCode::BackTab {
        key.modifiers |= KeyModifiers::SHIFT;
    }
    let base = key.modifiers - KeyModifiers::SHIFT;
    let extend = key.modifiers.contains(KeyModifiers::SHIFT);
    let movement = match (key.code, base) {
        (KeyCode::Left, KeyModifiers::NONE) => Some(Movement::Left),
        (KeyCode::Right, KeyModifiers::NONE) => Some(Movement::Right),
        (KeyCode::Up, KeyModifiers::NONE) => Some(Movement::Up),
        (KeyCode::Down, KeyModifiers::NONE) => Some(Movement::Down),
        (KeyCode::Home, KeyModifiers::NONE) => Some(Movement::Home),
        (KeyCode::End, KeyModifiers::NONE) => Some(Movement::End),
        (KeyCode::PageUp, KeyModifiers::NONE) => Some(Movement::PageUp),
        (KeyCode::PageDown, KeyModifiers::NONE) => Some(Movement::PageDown),
        (KeyCode::Home, KeyModifiers::CONTROL) => Some(Movement::Start),
        (KeyCode::End, KeyModifiers::CONTROL) => Some(Movement::Finish),
        _ => None,
    };
    if let Some(movement) = movement {
        return Some(Command::Move(movement, extend));
    }
    BINDINGS.iter().find_map(|binding| {
        (key.code == binding.code
            && key.modifiers == binding.modifiers
            && (key.kind == KeyEventKind::Press || binding.command.repeatable()))
        .then_some(binding.command)
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    /// Cmd aliases resolve explicitly, preserving Shift and rejecting unrelated modifiers/releases.
    #[test]
    fn exact_shortcuts_and_text() {
        for binding in BINDINGS {
            assert_eq!(
                super::binding(KeyEvent::new(binding.code, binding.modifiers)),
                Some(binding.command)
            );
            assert_eq!(
                super::binding(KeyEvent::new_with_kind(
                    binding.code,
                    binding.modifiers,
                    KeyEventKind::Release
                )),
                None
            );
        }
        assert_eq!(
            binding(KeyEvent::new(
                KeyCode::Char('S'),
                KeyModifiers::SUPER | KeyModifiers::SHIFT
            )),
            Some(Command::SaveAs)
        );
        assert_eq!(
            binding(KeyEvent::new(
                KeyCode::Char('q'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            )),
            None
        );
        assert_eq!(
            binding(KeyEvent::new_with_kind(
                KeyCode::Char('q'),
                KeyModifiers::CONTROL,
                KeyEventKind::Repeat
            )),
            None
        );
        assert!(
            matches!(normalize(Event::Key(KeyEvent::new(KeyCode::Char('界'), KeyModifiers::NONE))), Some(Input::Text(text)) if text == "界")
        );
    }
}
