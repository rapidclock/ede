//! Non-modal search panel; fields contain literal UTF-8, never escape syntax.
use super::*;
use crate::{
    editor::search::{Operation, SearchRequest},
    search::Query,
};

/// Bounded session fields and the visible slice of keyboard/mouse controls.
#[derive(Default)]
pub(crate) struct Panel {
    pub open: bool,
    pub replace: bool,
    pub query: Query,
    pub replacement: String,
    pub all: bool,
    pub row: usize,
    pub field: u8,
}

impl Panel {
    pub(crate) fn controls(&self) -> Vec<u8> {
        (0..15)
            .filter(|i| self.replace || !matches!(i, 1 | 7 | 8))
            .collect()
    }
    pub(crate) fn request(&self, operation: Operation) -> SearchRequest {
        SearchRequest {
            navigation: None,
            query: self.query.clone(),
            replacement: self.replacement.clone(),
            all: self.all,
            operation,
        }
    }
    pub(crate) fn label(&self, index: u8) -> String {
        match index {
            0 => format!("Find: {}", display(&self.query.literal)),
            1 => format!("Replace: {}", display(&self.replacement)),
            2 => format!("Match case: {}", self.query.match_case),
            3 => format!("Whole word: {}", self.query.whole_word),
            4 => format!(
                "Scope: {}",
                if self.all {
                    "All open"
                } else {
                    "Current document"
                }
            ),
            5 => "Previous match (Shift+F3)".into(),
            6 => "Next match (F3)".into(),
            7 => "Replace next".into(),
            8 => "Replace all…".into(),
            9 => "Cancel search".into(),
            10 => "Close search".into(),
            11 => "Insert Tab".into(),
            12 => "Insert LF".into(),
            13 => "Insert CRLF".into(),
            14 => "Search summary".into(),
            15 => "Replace all".into(),
            _ => "Cancel".into(),
        }
    }
}
/// Escape actual controls distinctly from literal backslashes; never emit terminal bytes.
pub(crate) fn display(text: &str) -> String {
    let mut result = String::new();
    for ch in text.chars() {
        if ch == '\\' || ch.is_control() {
            result.extend(ch.escape_default());
        } else {
            result.push(ch);
        }
    }
    result
}

impl AppState {
    pub(super) fn search_field_changed(&self) -> Intent {
        Intent::Editor(if self.search.field == 1 {
            Action::InvalidateReplacement
        } else {
            Action::InvalidateSearch
        })
    }
    pub(super) fn search_field_focused(&self) -> bool {
        self.search.open
            && self.modal.is_none()
            && !self.help
            && matches!(self.focus, Some(Control::Search(0 | 1)))
    }
    pub(super) fn search_focus(&mut self, index: u8) {
        if index <= 1 {
            self.search.field = index;
        }
        let controls = self.search.controls();
        if let Some(row) = controls.iter().position(|i| *i == index) {
            if row < self.search.row {
                self.search.row = row;
            }
            if row >= self.search.row + 2 {
                self.search.row = row - 1;
            }
        }
        self.focus = Some(Control::Search(index));
        self.dirty = true;
    }
    /// Some(None) means handled without a runtime effect; None permits normal routing.
    pub(super) fn search_command(
        &mut self,
        command: Command,
        layout: &LayoutSnapshot,
    ) -> Option<Option<Intent>> {
        if let Some(Modal::ReplaceAll(_)) = &self.modal {
            return Some(match command {
                Command::Cancel => {
                    self.modal = None;
                    self.focus = Some(Control::Search(8));
                    self.dirty = true;
                    None
                }
                Command::FocusNext
                | Command::FocusPrevious
                | Command::PreviousControl
                | Command::Tab => {
                    self.focus = layout.next(
                        self.focus,
                        matches!(command, Command::FocusPrevious | Command::PreviousControl),
                    );
                    self.dirty = true;
                    None
                }
                Command::Move(movement, _) => {
                    self.help_scroll = if matches!(movement, Movement::Up | Movement::PageUp) {
                        self.help_scroll.saturating_sub(1)
                    } else {
                        self.help_scroll.saturating_add(1)
                    };
                    self.dirty = true;
                    None
                }
                Command::Activate | Command::Newline
                    if layout.area.width >= 40 && layout.area.height >= 8 =>
                {
                    if self.focus == Some(Control::Search(15)) {
                        let Some(Modal::ReplaceAll(request)) = self.modal.take() else {
                            unreachable!()
                        };
                        self.focus = Some(Control::Search(8));
                        self.dirty = true;
                        Some(Intent::Editor(Action::Search(request)))
                    } else {
                        self.modal = None;
                        self.focus = Some(Control::Search(8));
                        self.dirty = true;
                        None
                    }
                }
                _ => None,
            });
        }
        if self.modal.is_some() || self.help || self.guard_request.is_some() {
            return None;
        }
        if matches!(command, Command::Find | Command::Replace) {
            self.search.open = true;
            self.search.replace = command == Command::Replace;
            self.search.row = 0;
            self.field_generation = self.field_generation.wrapping_add(1);
            self.field_error = None;
            self.search_focus(0);
            return Some(Some(Intent::Editor(Action::RevokeSearch)));
        }
        if matches!(
            command,
            Command::FindNext
                | Command::FindPrevious
                | Command::ReplaceNext
                | Command::ReplaceAll
                | Command::CancelSearch
        ) {
            if command == Command::CancelSearch {
                return Some(Some(Intent::Editor(Action::CancelSearch)));
            }
            if self.search.query.literal.is_empty() {
                return self.search_command(Command::Find, layout);
            }
            if matches!(command, Command::ReplaceNext | Command::ReplaceAll) && !self.search.replace
            {
                return self.search_command(Command::Replace, layout);
            }
            let operation = match command {
                Command::FindPrevious => Operation::Previous,
                Command::ReplaceNext => Operation::ReplaceNext,
                Command::ReplaceAll => Operation::ReplaceAll,
                _ => Operation::Next,
            };
            let request = self.search.request(operation);
            if matches!(operation, Operation::ReplaceNext | Operation::ReplaceAll)
                && !self.search.all
                && !self.screen.can_replace
            {
                self.field_message("Replacement unavailable for read-only retained history");
                return Some(None);
            }
            if operation == Operation::ReplaceAll {
                self.modal = Some(Modal::ReplaceAll(request));
                self.focus = Some(Control::Search(16));
                self.dirty = true;
                return Some(Some(Intent::Editor(Action::RevokeSearch)));
            }
            return Some(Some(Intent::Editor(Action::Search(request))));
        }
        if !self.search.open {
            return None;
        }
        if command == Command::Cancel {
            if self.screen.search.busy {
                return Some(Some(Intent::Editor(Action::CancelSearch)));
            }
            self.search.open = false;
            self.field_generation = self.field_generation.wrapping_add(1);
            self.field_paste = None;
            self.field_submit = None;
            self.search_submit_field = None;
            self.focus = Some(Control::Content);
            self.dirty = true;
            return Some(Some(Intent::Editor(Action::InvalidateSearch)));
        }
        if matches!(command, Command::FocusNext | Command::FocusPrevious) {
            if matches!(self.focus, Some(Control::Search(_))) {
                self.focus = Some(Control::Content);
                self.dirty = true;
            } else {
                self.search_focus(self.search.field);
            }
            return Some(None);
        }
        let Some(Control::Search(index)) = self.focus else {
            return None;
        };
        if matches!(command, Command::Tab | Command::PreviousControl) {
            if self.field_paste.is_some() {
                self.field_message("Wait for field paste before changing controls");
                return Some(None);
            }
            let controls = self.search.controls();
            let current = controls.iter().position(|i| *i == index).unwrap_or(0);
            self.search_focus(
                controls[(current
                    + if command == Command::PreviousControl {
                        controls.len() - 1
                    } else {
                        1
                    })
                    % controls.len()],
            );
            return Some(None);
        }
        if command == Command::Backspace && index <= 1 {
            self.edit_field(FieldEdit::Backspace);
            return Some(Some(self.search_field_changed()));
        }
        if !matches!(command, Command::Activate | Command::Newline) {
            return Some(None);
        }
        if layout.area.width < 40 || layout.area.height < 8 {
            return Some(None);
        }
        if let Some(pending) = &mut self.field_paste {
            pending.submit = true;
            self.search_submit_field = Some(self.search.field);
            return Some(None);
        }
        Some(match index {
            0 => self.search_command(Command::FindNext, layout).flatten(),
            1 | 7 => self.search_command(Command::ReplaceNext, layout).flatten(),
            2 => {
                self.search.query.match_case = !self.search.query.match_case;
                self.dirty = true;
                Some(Intent::Editor(Action::InvalidateSearch))
            }
            3 => {
                self.search.query.whole_word = !self.search.query.whole_word;
                self.dirty = true;
                Some(Intent::Editor(Action::InvalidateSearch))
            }
            4 => {
                self.search.all = !self.search.all;
                self.dirty = true;
                Some(Intent::Editor(Action::InvalidateSearch))
            }
            5 => self.search_command(Command::FindPrevious, layout).flatten(),
            6 => self.search_command(Command::FindNext, layout).flatten(),
            8 => self.search_command(Command::ReplaceAll, layout).flatten(),
            9 => Some(Intent::Editor(Action::CancelSearch)),
            10 => {
                self.search.open = false;
                self.focus = Some(Control::Content);
                self.dirty = true;
                Some(Intent::Editor(Action::InvalidateSearch))
            }
            11..=13 => {
                let field = self.search.field;
                self.search_focus(field);
                self.edit_field(FieldEdit::Text(
                    match index {
                        11 => "\t",
                        12 => "\n",
                        _ => "\r\n",
                    }
                    .into(),
                ));
                Some(self.search_field_changed())
            }
            14 => {
                self.modal = Some(Modal::Details(
                    self.screen.search.summary(&self.screen.tabs),
                ));
                self.focus = Some(Control::DetailsClose);
                self.dirty = true;
                Some(Intent::Editor(Action::RevokeSearch))
            }
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    fn layout(app: &AppState) -> LayoutSnapshot {
        LayoutSnapshot::for_app(Rect::new(0, 0, 40, 8), app, 1)
    }

    #[test]
    fn field_paste_remains_bound_to_original_search_field() {
        let mut app = AppState::new();
        let mut geometry = layout(&app);
        app.command(Command::Replace, &geometry);
        geometry = layout(&app);
        app.input(Input::PasteStarted(Cancellation::default()), &geometry);
        app.command(Command::Tab, &geometry);
        app.input(Input::Text("replacement".into()), &geometry);
        let mut spool = crate::spool::Spool::new().unwrap();
        spool.append(b"query").unwrap();
        app.input(Input::Paste(Arc::new(spool)), &geometry);
        app.event(Event::FieldPaste(app.field_generation, Ok("query".into())));
        assert_eq!(app.search.query.literal, "query");
        assert_eq!(app.search.replacement, "replacement");
    }

    #[test]
    fn reverse_field_focus_stays_inside_panel_and_f6_returns_to_editor() {
        let mut app = AppState::new();
        app.command(Command::Replace, &layout(&app));
        app.command(Command::Tab, &layout(&app));
        assert_eq!(app.focus, Some(Control::Search(1)));
        app.command(Command::PreviousControl, &layout(&app));
        assert_eq!(app.focus, Some(Control::Search(0)));
        app.command(Command::FocusNext, &layout(&app));
        assert_eq!(app.focus, Some(Control::Content));
        app.command(Command::FocusPrevious, &layout(&app));
        assert_eq!(app.focus, Some(Control::Search(0)));
    }

    #[test]
    fn oversize_and_invalid_search_pastes_preserve_previous_field() {
        let mut app = AppState::new();
        app.command(Command::Find, &layout(&app));
        app.input(Input::Text("keep".into()), &layout(&app));
        app.input(Input::Text("x".repeat(4096)), &layout(&app));
        assert_eq!(app.search.query.literal, "keep");
        app.input(Input::Text("\0".into()), &layout(&app));
        assert_eq!(app.search.query.literal, "keep");
        assert!(app.field_error.is_some());
    }

    #[test]
    fn every_control_is_keyboard_and_pointer_reachable_at_minimum_size() {
        let mut app = AppState::new();
        app.command(Command::Replace, &layout(&app));
        for index in app.search.controls() {
            assert_eq!(app.focus, Some(Control::Search(index)));
            let geometry = layout(&app);
            assert_eq!(geometry.content_height(), 3);
            let point = (0..8)
                .flat_map(|y| (0..40).map(move |x| (x, y)))
                .find(|(x, y)| geometry.hit(*x, *y) == Some(Control::Search(index)));
            assert!(
                point.is_some(),
                "control {index} has no visible pointer target"
            );
            app.command(Command::Tab, &geometry);
        }
        assert_eq!(app.focus, Some(Control::Search(0)));
        app.search.query.literal = "keep".into();
        let small = LayoutSnapshot::for_app(Rect::new(0, 0, 39, 7), &app, 2);
        app.command(Command::ReplaceAll, &small);
        assert_eq!(app.search.query.literal, "keep");
        app.focus = Some(Control::Search(15));
        assert!(app.command(Command::Activate, &small).is_none());
        assert!(matches!(app.modal, Some(Modal::ReplaceAll(_))));
        assert!(matches!(
            app.command(Command::Activate, &layout(&app)),
            Some(Intent::Editor(Action::Search(_)))
        ));
    }

    #[test]
    fn opening_path_clears_delayed_search_field_submission() {
        let mut app = AppState::new();
        app.search_submit_field = Some(1);
        app.path(PathKind::Save);
        assert!(app.search_submit_field.is_none());
    }
}
