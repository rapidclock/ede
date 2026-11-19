//! User-facing shortcut scopes, categories, and command labels.

use super::{Command, Context, Movement};

/// Labels describe which surface receives the keys.
pub(crate) fn group_label(context: Context) -> &'static str {
    match context {
        Context::Global => "App commands",
        Context::Editor => "Editing text",
        Context::Controls => "Focused buttons",
        Context::Search => "Find & Replace",
        Context::Picker => "File picker",
        Context::Dialog => "Dialogs",
        Context::Settings => "While in Settings",
    }
}

/// Contexts keep their dispatch identity even when grouped by task for display.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum Category {
    Files,
    Tabs,
    Find,
    Highlighting,
    Streams,
    Editing,
    Navigation,
    Selection,
    ButtonActions,
    ButtonNavigation,
    SearchOptions,
    SearchInput,
    PanelActions,
    Other,
}

impl Category {
    pub(super) fn for_command(context: Context, command: Command) -> Self {
        use Command as C;
        match context {
            Context::Global => match command {
                C::New | C::Open | C::Save | C::SaveAs | C::RetryRefresh | C::Recovery => {
                    Self::Files
                }
                C::Tabs | C::PreviousTab | C::NextTab | C::CloseTab | C::TabDetails => Self::Tabs,
                C::Find
                | C::Replace
                | C::FindNext
                | C::FindPrevious
                | C::ReplaceNext
                | C::ReplaceAll
                | C::CancelSearch => Self::Find,
                C::Language | C::SyntaxTheme | C::ReloadHighlighting | C::RetryHighlighting => {
                    Self::Highlighting
                }
                C::FollowFile | C::ToggleFollow | C::RetryFinish | C::ExportRetained => {
                    Self::Streams
                }
                _ => Self::Other,
            },
            Context::Editor => match command {
                C::Move(_, true) | C::SelectAll => Self::Selection,
                C::Move(_, false) | C::FocusSearch | C::FocusNext | C::FocusPrevious => {
                    Self::Navigation
                }
                _ => Self::Editing,
            },
            Context::Picker | Context::Dialog | Context::Settings => match command {
                C::Move(..)
                | C::FocusNext
                | C::FocusPrevious
                | C::SelectRow
                | C::Activate
                | C::Cancel => Self::Navigation,
                _ => Self::Other,
            },
            Context::Controls => match command {
                C::FocusNext | C::FocusPrevious => Self::ButtonNavigation,
                _ => Self::ButtonActions,
            },
            Context::Search => match command {
                C::ToggleMatchCase | C::ToggleWholeWord | C::ToggleSearchScope => {
                    Self::SearchOptions
                }
                C::FieldBackspace | C::InsertQueryTab | C::InsertQueryLf | C::InsertQueryCrlf => {
                    Self::SearchInput
                }
                _ => Self::PanelActions,
            },
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Files => "Files",
            Self::Tabs => "Tabs",
            Self::Find => "Find & Replace",
            Self::Highlighting => "Highlighting",
            Self::Streams => "Streams",
            Self::Editing => "Editing",
            Self::Navigation => "Navigation",
            Self::Selection => "Selection",
            Self::ButtonActions => "Button actions",
            Self::ButtonNavigation => "Button navigation",
            Self::SearchOptions => "Search options",
            Self::SearchInput => "Search input",
            Self::PanelActions => "Panel actions",
            Self::Other => "Other commands",
        }
    }
}

/// Stable config IDs remain available in the binding editor and diagnostics.
pub(super) fn command_label(context: Context, command: Command) -> &'static str {
    use Command as C;
    match command {
        C::New => "New scratch",
        C::Open => "Open file",
        C::Save => "Save file",
        C::Settings => "Open Settings",
        C::Activate if context == Context::Controls => "Activate button",
        C::FocusNext if context == Context::Controls => "Next button",
        C::FocusPrevious if context == Context::Controls => "Previous button",
        C::Paste => "Paste",
        C::Newline => "Insert line break",
        C::Tab => "Insert tab",
        C::Move(Movement::Home, false) if context != Context::Editor => "First item",
        C::Move(Movement::End, false) if context != Context::Editor => "Last item",
        C::Move(Movement::Start, false) if context != Context::Editor => "Start of list",
        C::Move(Movement::Finish, false) if context != Context::Editor => "End of list",
        C::Move(movement, select) => movement_label(movement, select),
        _ => command.label(),
    }
}

fn movement_label(movement: Movement, select: bool) -> &'static str {
    match (movement, select) {
        (Movement::Left, false) => "Move left",
        (Movement::Right, false) => "Move right",
        (Movement::Up, false) => "Move up",
        (Movement::Down, false) => "Move down",
        (Movement::Home, false) => "Start of line",
        (Movement::End, false) => "End of line",
        (Movement::Start, false) => "Start of document",
        (Movement::Finish, false) => "End of document",
        (Movement::PageUp, false) => "Page up",
        (Movement::PageDown, false) => "Page down",
        (Movement::Left, true) => "Select left",
        (Movement::Right, true) => "Select right",
        (Movement::Up, true) => "Select up",
        (Movement::Down, true) => "Select down",
        (Movement::Home, true) => "Select to line start",
        (Movement::End, true) => "Select to line end",
        (Movement::Start, true) => "Select doc start",
        (Movement::Finish, true) => "Select doc end",
        (Movement::PageUp, true) => "Select one page up",
        (Movement::PageDown, true) => "Select one page down",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppState;
    use std::collections::HashSet;
    use unicode_width::UnicodeWidthChar;

    /// A clipped command can still be distinguished from its neighbors at 40 columns.
    #[test]
    fn compact_command_labels_are_distinct_within_each_group() {
        let app = AppState::new();
        let mut seen = HashSet::new();
        for row in &app.settings.applied.map.rows {
            let mut remaining = 18;
            let label = command_label(row.context, row.command);
            let visible: String = label
                .chars()
                .take_while(|ch| {
                    let width = ch.width().unwrap_or(0);
                    if width > remaining {
                        return false;
                    }
                    remaining -= width;
                    true
                })
                .collect();
            assert!(
                seen.insert((row.context, visible)),
                "indistinct command label {label:?} in {:?}",
                row.context
            );
        }
    }
}
