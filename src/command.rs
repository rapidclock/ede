//! Named commands and the single default keyboard/help binding table.
use crossterm::event::{KeyCode, KeyModifiers};

/// Movement units shared by keyboard and background text navigation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Movement {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Start,
    Finish,
    PageUp,
    PageDown,
}
/// Actions independent of their invoking device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Command {
    Find,
    Replace,
    FindNext,
    FindPrevious,
    ReplaceNext,
    ReplaceAll,
    CancelSearch,
    Quit,
    Help,
    Menu,
    Cancel,
    FocusNext,
    FocusPrevious,
    PreviousControl,
    Activate,
    New,
    Open,
    FollowFile,
    ToggleFollow,
    RetryFinish,
    ExportRetained,
    Tabs,
    PreviousTab,
    NextTab,
    CloseTab,
    TabDetails,
    Language,
    SyntaxTheme,
    ReloadHighlighting,
    RetryHighlighting,
    Recovery,
    RetryRefresh,
    Save,
    SaveAs,
    Copy,
    Cut,
    Paste,
    Undo,
    Redo,
    SelectAll,
    GoTo,
    Newline,
    Tab,
    Backspace,
    Delete,
    Move(Movement, bool),
    Suspend,
    MouseCapture,
}
/// Exact default binding and human-readable shortcut.
pub(crate) struct Binding {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
    pub command: Command,
    pub shortcut: &'static str,
}
impl Binding {
    /// Build static metadata without allocation.
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
use Command as C;
use KeyCode as K;
use KeyModifiers as M;
/// Portable defaults plus explicit delivered Super/Cmd aliases. No blanket modifier substitution.
pub(crate) const BINDINGS: &[Binding] = &[
    Binding::new(K::Char('f'), M::CONTROL, C::Find, "Ctrl+F"),
    Binding::new(K::Char('f'), M::SUPER, C::Find, "Cmd+F"),
    Binding::new(K::Char('h'), M::CONTROL, C::Replace, "Ctrl+H"),
    Binding::new(
        K::Char('h'),
        M::SUPER.union(M::SHIFT),
        C::Replace,
        "Cmd+Shift+H",
    ),
    Binding::new(K::F(3), M::NONE, C::FindNext, "F3"),
    Binding::new(K::F(3), M::SHIFT, C::FindPrevious, "Shift+F3"),
    Binding::new(K::Char('q'), M::CONTROL, C::Quit, "Ctrl+Q"),
    Binding::new(K::Char('n'), M::CONTROL, C::New, "Ctrl+N"),
    Binding::new(K::Char('o'), M::CONTROL, C::Open, "Ctrl+O"),
    Binding::new(K::Char('w'), M::CONTROL, C::CloseTab, "Ctrl+W"),
    Binding::new(K::Char('s'), M::CONTROL, C::Save, "Ctrl+S"),
    Binding::new(K::Char('a'), M::CONTROL, C::SelectAll, "Ctrl+A"),
    Binding::new(K::Char('c'), M::CONTROL, C::Copy, "Ctrl+C"),
    Binding::new(K::Char('x'), M::CONTROL, C::Cut, "Ctrl+X"),
    Binding::new(K::Char('v'), M::CONTROL, C::Paste, "Ctrl+V"),
    Binding::new(K::Char('z'), M::CONTROL, C::Undo, "Ctrl+Z"),
    Binding::new(K::Char('y'), M::CONTROL, C::Redo, "Ctrl+Y"),
    Binding::new(K::Char('q'), M::SUPER, C::Quit, "Cmd+Q"),
    Binding::new(K::Char('n'), M::SUPER, C::New, "Cmd+N"),
    Binding::new(K::Char('o'), M::SUPER, C::Open, "Cmd+O"),
    Binding::new(K::Char('w'), M::SUPER, C::CloseTab, "Cmd+W"),
    Binding::new(K::Char('s'), M::SUPER, C::Save, "Cmd+S"),
    Binding::new(
        K::Char('s'),
        M::SUPER.union(M::SHIFT),
        C::SaveAs,
        "Cmd+Shift+S",
    ),
    Binding::new(K::Char('a'), M::SUPER, C::SelectAll, "Cmd+A"),
    Binding::new(K::Char('c'), M::SUPER, C::Copy, "Cmd+C"),
    Binding::new(K::Char('x'), M::SUPER, C::Cut, "Cmd+X"),
    Binding::new(K::Char('v'), M::SUPER, C::Paste, "Cmd+V"),
    Binding::new(K::Char('z'), M::SUPER, C::Undo, "Cmd+Z"),
    Binding::new(
        K::Char('z'),
        M::SUPER.union(M::SHIFT),
        C::Redo,
        "Cmd+Shift+Z",
    ),
    Binding::new(K::Char('g'), M::CONTROL, C::GoTo, "Ctrl+G"),
    Binding::new(K::F(1), M::NONE, C::Help, "F1"),
    Binding::new(K::F(2), M::NONE, C::Menu, "F2"),
    Binding::new(K::F(4), M::NONE, C::SaveAs, "F4"),
    Binding::new(K::F(6), M::NONE, C::FocusNext, "F6"),
    Binding::new(K::F(6), M::SHIFT, C::FocusPrevious, "Shift+F6"),
    Binding::new(K::F(7), M::NONE, C::PreviousTab, "F7"),
    Binding::new(K::F(8), M::NONE, C::NextTab, "F8"),
    Binding::new(K::Esc, M::NONE, C::Cancel, "Escape"),
    Binding::new(K::Tab, M::NONE, C::Tab, "Tab"),
    Binding::new(K::BackTab, M::SHIFT, C::PreviousControl, "Shift+Tab"),
    Binding::new(K::Enter, M::NONE, C::Newline, "Enter"),
    Binding::new(K::Backspace, M::NONE, C::Backspace, "Backspace"),
    Binding::new(K::Delete, M::NONE, C::Delete, "Delete"),
];
impl Command {
    /// Human-readable name shared by keyboard help and command discovery.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Find => "Find",
            Self::Replace => "Replace",
            Self::FindNext => "Find next",
            Self::FindPrevious => "Find previous",
            Self::ReplaceNext => "Replace next",
            Self::ReplaceAll => "Replace all",
            Self::CancelSearch => "Cancel search",
            Self::Quit => "Quit",
            Self::Help => "Help",
            Self::Menu => "Actions",
            Self::Cancel => "Cancel",
            Self::FocusNext => "Next control",
            Self::FocusPrevious => "Previous control",
            Self::PreviousControl => "Previous field/control",
            Self::Activate => "Activate",
            Self::New => "New",
            Self::Open => "Open",
            Self::FollowFile => "Follow File…",
            Self::ToggleFollow => "Pause View / Resume Follow",
            Self::RetryFinish => "Retry Finish",
            Self::ExportRetained => "Export Retained Copy…",
            Self::Tabs => "Tabs",
            Self::PreviousTab => "Previous tab",
            Self::NextTab => "Next tab",
            Self::CloseTab => "Close tab",
            Self::TabDetails => "Tab details",
            Self::Language => "Language…",
            Self::SyntaxTheme => "Syntax Theme…",
            Self::ReloadHighlighting => "Reload Highlighting",
            Self::RetryHighlighting => "Retry Highlighting",
            Self::Recovery => "Recovery",
            Self::RetryRefresh => "Retry refresh",
            Self::Save => "Save",
            Self::SaveAs => "Save As",
            Self::Copy => "Copy",
            Self::Cut => "Cut",
            Self::Paste => "Internal paste",
            Self::SelectAll => "Select all",
            Self::GoTo => "Go to line:cell",
            Self::Undo => "Undo",
            Self::Redo => "Redo",
            Self::Newline => "Newline / activate",
            Self::Tab => "Tab / next control",
            Self::Backspace => "Delete backward",
            Self::Delete => "Delete forward",
            Self::Move(..) => "Move caret",
            Self::Suspend => "Suspend",
            Self::MouseCapture => "Toggle mouse capture",
        }
    }
    /// Only local, repeatable interactions accept enhanced-key repeat events.
    pub fn repeatable(self) -> bool {
        matches!(
            self,
            Self::Move(..)
                | Self::Backspace
                | Self::Delete
                | Self::Newline
                | Self::Tab
                | Self::FocusNext
                | Self::FocusPrevious
        )
    }
}
