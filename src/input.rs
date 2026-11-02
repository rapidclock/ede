//! Normalize terminal events without performing application actions or I/O.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent};

use crate::command::{BINDINGS, Command};

/// Application-level input; paste payloads are intentionally discarded until editing exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Input {
    Command(Command),
    Pointer(MouseEvent),
    Paste,
    FocusLost,
    FocusGained,
    Resize(u16, u16),
}

/// Convert one terminal event. Unknown keys have no effect on the empty shell.
pub(crate) fn normalize(event: Event) -> Option<Input> {
    match event {
        Event::Key(key) => binding(key).map(Input::Command),
        Event::Mouse(mouse) => Some(Input::Pointer(mouse)),
        Event::Paste(_) => Some(Input::Paste),
        Event::FocusLost => Some(Input::FocusLost),
        Event::FocusGained => Some(Input::FocusGained),
        Event::Resize(width, height) => Some(Input::Resize(width, height)),
    }
}

/// Resolve exact modifiers, normalizing letter case and conventional BackTab encodings.
fn binding(mut key: KeyEvent) -> Option<Command> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && let KeyCode::Char(letter) = key.code
    {
        key.code = KeyCode::Char(letter.to_ascii_lowercase());
    }
    if key.code == KeyCode::BackTab {
        key.modifiers |= KeyModifiers::SHIFT;
    }
    BINDINGS.iter().find_map(|binding| {
        let matches = key.code == binding.code && key.modifiers == binding.modifiers;
        let repeatable = matches!(binding.command, Command::FocusNext | Command::FocusPrevious);
        (matches && (key.kind == KeyEventKind::Press || repeatable)).then_some(binding.command)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Modifiers, event kinds, and uppercase encodings cannot accidentally activate commands.
    #[test]
    fn shortcuts_are_exact_and_discrete() {
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(
            binding(KeyEvent::new(KeyCode::Char('S'), ctrl)),
            Some(Command::Save)
        );
        assert_eq!(
            binding(KeyEvent::new(KeyCode::Char('q'), ctrl | KeyModifiers::ALT)),
            None
        );
        assert_eq!(
            binding(KeyEvent::new(
                KeyCode::Char('q'),
                ctrl | KeyModifiers::SHIFT
            )),
            None
        );
        assert_eq!(
            binding(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            binding(KeyEvent::new_with_kind(
                KeyCode::Char('q'),
                ctrl,
                KeyEventKind::Repeat
            )),
            None
        );
        assert_eq!(
            binding(KeyEvent::new_with_kind(
                KeyCode::Tab,
                KeyModifiers::NONE,
                KeyEventKind::Repeat
            )),
            Some(Command::FocusNext)
        );
        assert_eq!(
            binding(KeyEvent::new_with_kind(
                KeyCode::Char('q'),
                ctrl,
                KeyEventKind::Release
            )),
            None
        );
        assert_eq!(
            binding(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE)),
            Some(Command::FocusPrevious)
        );
    }

    /// A pasted control character is data, never a quit shortcut.
    #[test]
    fn paste_cannot_invoke_bindings() {
        assert_eq!(
            normalize(Event::Paste("\u{11}\u{13}secret".into())),
            Some(Input::Paste)
        );
    }
}
