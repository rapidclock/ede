//! Category and command navigation for the Actions pane.

use super::{Command, Movement};

/// Categories are ordered for both keyboard navigation and rendering.
pub(crate) const GROUPS: &[(&str, &[Command])] = &[
    (
        "File",
        &[
            Command::New,
            Command::Open,
            Command::Save,
            Command::SaveAs,
            Command::Recovery,
            Command::RetryRefresh,
            Command::Quit,
        ],
    ),
    (
        "Edit",
        &[
            Command::Undo,
            Command::Redo,
            Command::Cut,
            Command::Copy,
            Command::Paste,
            Command::SelectAll,
            Command::GoTo,
        ],
    ),
    ("Search", &[Command::Find, Command::Replace]),
    (
        "Tabs",
        &[Command::Tabs, Command::CloseTab, Command::TabDetails],
    ),
    (
        "Highlighting",
        &[
            Command::Language,
            Command::SyntaxTheme,
            Command::ReloadHighlighting,
            Command::RetryHighlighting,
        ],
    ),
    (
        "Streams",
        &[
            Command::FollowFile,
            Command::ToggleFollow,
            Command::RetryFinish,
            Command::ExportRetained,
        ],
    ),
    (
        "Settings",
        &[
            Command::Settings,
            Command::ReloadSettings,
            Command::MouseCapture,
        ],
    ),
    ("Help", &[Command::Help, Command::About, Command::Suspend]),
];

/// Selection and focus are independent so the current category stays visible.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Menu {
    pub category: usize,
    pub row: usize,
    pub commands_focused: bool,
}

impl Menu {
    pub(crate) fn commands(self) -> &'static [Command] {
        GROUPS[self.category].1
    }

    /// Select a new category and start at its first command.
    pub(crate) fn select(&mut self, category: usize) {
        if self.category != category {
            self.category = category;
            self.row = 0;
        }
    }

    /// Move and focus within the requested column, keeping selections in bounds.
    pub(crate) fn move_in(&mut self, commands: bool, movement: Movement, rows: u16) {
        self.commands_focused = commands;
        let (selected, count) = if commands {
            (self.row, self.commands().len())
        } else {
            (self.category, GROUPS.len())
        };
        let next = match movement {
            Movement::Up => selected.saturating_sub(1),
            Movement::Down => selected + 1,
            Movement::PageUp => selected.saturating_sub(usize::from(rows)),
            Movement::PageDown => selected + usize::from(rows),
            Movement::Home | Movement::Start => 0,
            Movement::End | Movement::Finish => count - 1,
            Movement::Left | Movement::Right => selected,
        }
        .min(count - 1);
        if commands {
            self.row = next;
        } else {
            self.select(next);
        }
    }

    /// Encode the selected targets in the layout signature for pointer capture.
    pub(crate) fn layout_key(self) -> u64 {
        ((self.category as u64) << 32) | self.row as u64
    }
}
