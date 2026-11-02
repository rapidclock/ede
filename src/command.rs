//! Stable command identities and their shared keyboard/help metadata.

use crossterm::event::{KeyCode, KeyModifiers};

/// An action independent of the keyboard or pointer that invokes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Command {
    Quit,
    Help,
    Cancel,
    FocusNext,
    FocusPrevious,
    Activate,
    Save,
    Copy,
    Suspend,
}

/// Default binding; user-configurable bindings will build on this table later.
pub(crate) struct Binding {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
    pub command: Command,
    pub shortcut: &'static str,
}

/// Initial conventional terminal bindings, without enhanced keyboard protocols.
pub(crate) const BINDINGS: &[Binding] = &[
    Binding::new(
        KeyCode::Char('q'),
        KeyModifiers::CONTROL,
        Command::Quit,
        "Ctrl+Q",
    ),
    Binding::new(KeyCode::F(1), KeyModifiers::NONE, Command::Help, "F1"),
    Binding::new(KeyCode::Esc, KeyModifiers::NONE, Command::Cancel, "Escape"),
    Binding::new(KeyCode::Tab, KeyModifiers::NONE, Command::FocusNext, "Tab"),
    Binding::new(
        KeyCode::BackTab,
        KeyModifiers::SHIFT,
        Command::FocusPrevious,
        "Shift+Tab",
    ),
    Binding::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
        Command::Activate,
        "Enter",
    ),
    Binding::new(
        KeyCode::Char('s'),
        KeyModifiers::CONTROL,
        Command::Save,
        "Ctrl+S",
    ),
    Binding::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
        Command::Copy,
        "Ctrl+C",
    ),
];

impl Binding {
    /// Construct static binding metadata without runtime allocation.
    const fn new(
        code: KeyCode,
        modifiers: KeyModifiers,
        command: Command,
        shortcut: &'static str,
    ) -> Self {
        Self {
            code,
            modifiers,
            command,
            shortcut,
        }
    }
}

impl Command {
    /// Stable identifier intended for future configuration and command discovery.
    pub(crate) const fn id(self) -> &'static str {
        match self {
            Self::Quit => "app.quit",
            Self::Help => "app.help",
            Self::Cancel => "ui.cancel",
            Self::FocusNext => "focus.next",
            Self::FocusPrevious => "focus.previous",
            Self::Activate => "ui.activate",
            Self::Save => "document.save",
            Self::Copy => "selection.copy",
            Self::Suspend => "app.suspend",
        }
    }
}
