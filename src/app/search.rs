//! Search window focus, literal fields, and paged occurrence navigation.
use super::*;
use crate::{
    editor::search::{
        Operation, SearchRequest,
        results::{PAGE_SIZE, PageRequest, Rows},
    },
    search::Query,
};
use unicode_segmentation::UnicodeSegmentation;

pub(crate) const SUBMIT: u8 = 17;
pub(crate) const OPTIONS: u8 = 18;
pub(crate) const SHOW_RESULTS: u8 = 19;
pub(crate) const FORM_UP: u8 = 20;
pub(crate) const FORM_DOWN: u8 = 21;
const EXPANDED_OPTIONS: [u8; 6] = [2, 3, 11, 12, 13, 4];

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
    pub expanded: bool,
    pub list_open: bool,
    pub result_top: u64,
    pub result_focus: Option<u64>,
    visible_results: u16,
    hidden: Option<u64>,
    requested: Option<(u64, u64)>,
    expected: Option<(u64, u64)>,
    cursor: [Option<usize>; 2],
}

impl Panel {
    /// Visible form rows before clipping to the available height.
    pub(crate) fn groups(&self, busy: bool) -> Vec<Vec<u8>> {
        let mut groups = vec![vec![0, SUBMIT]];
        if self.replace {
            groups.extend([vec![1], vec![7, 8]]);
        }
        groups.push(vec![OPTIONS]);
        if self.expanded {
            groups.extend([vec![2, 3], vec![11, 12, 13], vec![4]]);
        }
        groups.push(vec![5, 6, 14]);
        if busy {
            groups.push(vec![9]);
        }
        groups
    }

    pub(crate) fn controls(&self, busy: bool) -> Vec<u8> {
        let mut controls: Vec<_> = self.groups(busy).into_iter().flatten().collect();
        if self.replace {
            // Tab between the text fields without visiting the equivalent Find action.
            controls.swap(1, 2);
        }
        controls
    }

    pub(crate) fn current_rows<'a>(&self, rows: &'a Rows) -> Option<&'a Rows> {
        (self.hidden != Some(rows.generation)).then_some(rows)
    }

    pub(crate) fn result_offset(&self, rows: &Rows, capacity: u16) -> u64 {
        self.result_top
            .min(rows.count.saturating_sub(u64::from(capacity.max(1))))
    }

    pub(crate) fn field_cursor(&self, field: u8, text: &str) -> usize {
        text.floor_char_boundary(
            self.cursor[usize::from(field)]
                .unwrap_or(text.len())
                .min(text.len()),
        )
    }

    pub(super) fn move_field(&mut self, field: u8, movement: Movement) {
        let text = if field == 0 {
            &self.query.literal
        } else {
            &self.replacement
        };
        let offset = self.field_cursor(field, text);
        let next = match movement {
            Movement::Left => text[..offset]
                .grapheme_indices(true)
                .next_back()
                .map_or(0, |(start, _)| start),
            Movement::Right => offset + text[offset..].graphemes(true).next().map_or(0, str::len),
            Movement::Home | Movement::Start => 0,
            Movement::End | Movement::Finish => text.len(),
            _ => return,
        };
        self.set_field_cursor(field, next);
    }

    pub(super) fn set_field_cursor(&mut self, field: u8, offset: usize) {
        self.cursor[usize::from(field)] = Some(offset);
    }

    pub(crate) fn request(&self, operation: Operation) -> SearchRequest {
        SearchRequest {
            replacement_scope: None,
            navigation: None,
            query: self.query.clone(),
            replacement: self.replacement.clone(),
            all: self.all,
            operation,
            browse: self.open,
            expected: self.expected,
        }
    }
    pub(crate) fn label(&self, index: u8) -> String {
        match index {
            0 => format!("Find: {}", display(&self.query.literal)),
            1 => format!("Replace: {}", display(&self.replacement)),
            2 => format!(
                "{} Match case",
                if self.query.match_case { "[x]" } else { "[ ]" }
            ),
            3 => format!(
                "{} Match Whole Word",
                if self.query.whole_word { "[x]" } else { "[ ]" }
            ),
            4 => self.scope_label().into(),
            5 => "Previous".into(),
            6 => "Next".into(),
            7 => "Replace Next".into(),
            8 | 15 => "Replace All".into(),
            9 => "Cancel search".into(),
            10 => "×".into(),
            11 => "Tab".into(),
            12 => "LF".into(),
            13 => "CRLF".into(),
            14 => "Summary".into(),
            SUBMIT => "Find".into(),
            OPTIONS if self.expanded => "⌄ Options".into(),
            OPTIONS => format!(
                "› Options  Case {} · Match Whole Word {} · {}",
                if self.query.match_case { "on" } else { "off" },
                if self.query.whole_word { "on" } else { "off" },
                self.scope_label()
            ),
            SHOW_RESULTS if self.list_open => "Form".into(),
            SHOW_RESULTS => "Results".into(),
            FORM_UP => "↑".into(),
            FORM_DOWN => "↓".into(),
            _ => "Cancel".into(),
        }
    }

    pub(crate) fn scope_label(&self) -> &'static str {
        if self.all {
            "All open tabs"
        } else {
            "Current tab"
        }
    }
}
/// Escape actual controls distinctly from literal backslashes; never emit terminal bytes.
pub(crate) fn display(text: &str) -> String {
    let mut result = String::new();
    for ch in text.chars() {
        if ch == '\\' || ch.is_control() || crate::text::is_bidi_format(ch) {
            result.extend(ch.escape_default());
        } else {
            result.push(ch);
        }
    }
    result
}

impl AppState {
    /// Retire the window before returning focus to the editor.
    pub(crate) fn close_search(&mut self) {
        self.revoke_native_clipboard();
        self.search.open = false;
        self.field_generation = self.field_generation.wrapping_add(1);
        self.field_paste = None;
        self.field_submit = None;
        self.search_submit_field = None;
        self.focus = Some(Control::Content);
        self.capture = None;
        self.dirty = true;
    }

    pub(super) fn search_field_changed(&mut self) -> Intent {
        if self.search.field == 1 {
            Intent::Editor(Action::InvalidateReplacement)
        } else {
            self.invalidate_search_rows()
        }
    }

    pub(super) fn invalidate_search_rows(&mut self) -> Intent {
        self.search.hidden = Some(self.screen.search.rows.generation);
        self.search.result_top = 0;
        self.search.result_focus = None;
        self.search.requested = None;
        self.search.expected = None;
        Intent::Editor(Action::InvalidateSearch)
    }

    pub(super) fn sync_search_selection(&mut self, rows: &Rows) {
        if self.search.expected.is_some_and(|(generation, index)| {
            generation == rows.generation
                && (rows.current == Some(index)
                    || index
                        .checked_sub(rows.start)
                        .and_then(|local| rows.page.get(local as usize))
                        .is_some_and(|row| row.change.is_some()))
        }) {
            self.search.expected = None;
        }
        if self.screen.search.rows.current != rows.current
            && let Some(row) = rows.current
        {
            self.search.result_focus = Some(row);
            self.search.result_top = row.saturating_sub(u64::from(self.search.visible_results > 1));
        }
    }

    /// Keep the selection visible when its viewport shrinks, even across a modal.
    pub(super) fn search_resize_results(&mut self, capacity: u16) {
        if capacity == 0 {
            return;
        }
        let previous = std::mem::replace(&mut self.search.visible_results, capacity);
        if previous > capacity
            && let Some(row) = self.search.result_focus
            && let Some(rows) = self.search.current_rows(&self.screen.search.rows)
        {
            let prior_top = self.search.result_offset(rows, previous);
            let top = self.search.result_offset(rows, capacity);
            let adjusted = top.clamp(row.saturating_sub(u64::from(capacity - 1)), row);
            if (prior_top..prior_top + u64::from(previous)).contains(&row) && adjusted != top {
                self.search.result_top = adjusted;
                self.dirty = true;
            }
        }
    }

    pub(super) fn search_fetch(&mut self, layout: &LayoutSnapshot) -> Option<Intent> {
        let capacity = layout.search_result_rows();
        let rows = self.search.current_rows(&self.screen.search.rows)?;
        if capacity == 0 || rows.count == 0 {
            return None;
        }
        let start = self.search.result_offset(rows, capacity);
        let end = (start + u64::from(capacity)).min(rows.count);
        if start >= rows.start && end <= rows.start + rows.page.len() as u64
            || self.search.requested == Some((rows.generation, start))
        {
            return None;
        }
        let generation = rows.generation;
        self.search.requested = Some((generation, start));
        Some(Intent::Editor(Action::SearchPage(PageRequest {
            generation,
            start,
            jump: None,
            navigation: None,
        })))
    }

    /// Allow a passive page request to be retried if the editor queue rejected it.
    pub(crate) fn search_page_rejected(&mut self, request: PageRequest) {
        if request.jump.is_none()
            && self.search.requested == Some((request.generation, request.start))
        {
            self.search.requested = None;
        }
    }

    pub(super) fn search_wheel(&mut self, up: bool, layout: &LayoutSnapshot, over_list: bool) {
        if over_list {
            let rows = &self.screen.search.rows;
            let capacity = layout.search_result_rows();
            let step = i64::from(capacity.min(3));
            self.search.result_top = self
                .search
                .result_offset(rows, capacity)
                .saturating_add_signed(if up { -step } else { step })
                .min(rows.count.saturating_sub(u64::from(capacity)));
        } else {
            self.search_form_step(!up);
        }
        self.dirty = true;
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
        if let Some(row) = self
            .search
            .groups(self.screen.search.busy)
            .iter()
            .position(|group| group.contains(&index))
        {
            self.search.row = row;
            self.search.list_open = false;
        }
        self.focus = Some(Control::Search(index));
        self.dirty = true;
    }

    fn search_form_step(&mut self, down: bool) {
        let groups = self.search.groups(self.screen.search.busy);
        let row = self
            .search
            .row
            .saturating_add_signed(if down { 1 } else { -1 })
            .min(groups.len() - 1);
        self.search_focus(groups[row][0]);
    }

    fn list_focus(&mut self) {
        self.search.list_open = true;
        let rows = &self.screen.search.rows;
        let selected = rows.current.unwrap_or(0);
        let visible = self.search.visible_results.max(1);
        let top = self.search.result_offset(rows, visible);
        if selected < top || selected >= top + u64::from(visible) {
            self.search.result_top = selected;
        }
        self.search.result_focus = Some(selected);
        self.focus = Some(Control::SearchList);
        self.dirty = true;
    }

    fn search_select(&mut self, row: u64, layout: &LayoutSnapshot) -> Option<Intent> {
        let rows = self.search.current_rows(&self.screen.search.rows)?;
        if row >= rows.count {
            return None;
        }
        let capacity = u64::from(layout.search_result_rows().max(1));
        let top = self.search.result_offset(rows, capacity as u16);
        self.search.result_top = row.clamp(top.min(row), top.max(row.saturating_sub(capacity - 1)));
        self.search.result_top = self
            .search
            .result_top
            .min(rows.count.saturating_sub(capacity));
        self.search.result_focus = Some(row);
        self.search.list_open = true;
        self.focus = Some(Control::SearchList);
        self.dirty = true;
        let generation = rows.generation;
        let start = self.search.result_top.min(row);
        self.search.requested = Some((generation, start));
        self.search.expected = Some((generation, row));
        Some(Intent::Editor(Action::SearchPage(PageRequest {
            generation,
            start: start.max(row.saturating_sub(PAGE_SIZE as u64 - 1)),
            jump: Some(row),
            navigation: None,
        })))
    }

    fn search_move(
        &mut self,
        control: Control,
        movement: Movement,
        layout: &LayoutSnapshot,
    ) -> Option<Intent> {
        if matches!(control, Control::SearchList | Control::SearchResult(_)) {
            let count = self.screen.search.rows.count;
            let row = self
                .search
                .result_focus
                .or(self.screen.search.rows.current)
                .unwrap_or(0);
            if movement == Movement::Left
                || movement == Movement::Down && row >= count.saturating_sub(1)
            {
                self.search_focus(0);
                return None;
            }
            if count == 0 {
                return None;
            }
            let step = i64::from(layout.search_result_rows().max(1));
            let next = match movement {
                Movement::Up => row.saturating_sub(1),
                Movement::Down => row.saturating_add(1),
                Movement::PageUp => row.saturating_add_signed(-step),
                Movement::PageDown => row.saturating_add_signed(step),
                Movement::Home | Movement::Start => 0,
                Movement::End | Movement::Finish => count - 1,
                _ => return None,
            }
            .min(count - 1);
            return self.search_select(next, layout);
        }
        let Control::Search(index) = control else {
            return None;
        };
        let groups = self.search.groups(self.screen.search.busy);
        let Some(row) = groups.iter().position(|group| group.contains(&index)) else {
            self.search_focus(0);
            return None;
        };
        match movement {
            Movement::Down | Movement::Up => {
                let down = movement == Movement::Down;
                let step = if down { 1 } else { -1 };
                if let Some(next) = EXPANDED_OPTIONS
                    .iter()
                    .position(|&option| index == option)
                    .and_then(|option| option.checked_add_signed(step))
                    .and_then(|option| EXPANDED_OPTIONS.get(option))
                {
                    self.search_focus(*next);
                } else if let Some(target) =
                    row.checked_add_signed(step).and_then(|row| groups.get(row))
                {
                    self.search_focus(target[0]);
                } else if !down
                    && self
                        .search
                        .current_rows(&self.screen.search.rows)
                        .is_some_and(|rows| rows.count != 0)
                {
                    self.list_focus();
                }
            }
            Movement::Left | Movement::Right if index == OPTIONS => {
                self.search.expanded = movement == Movement::Right;
                self.dirty = true;
            }
            Movement::Left
            | Movement::Right
            | Movement::Home
            | Movement::End
            | Movement::Start
            | Movement::Finish
                if index <= 1 =>
            {
                if self.field_paste.is_none() {
                    self.search.move_field(index, movement);
                    self.dirty = true;
                }
            }
            Movement::Left | Movement::Right => {
                let group = &groups[row];
                let cell = group.iter().position(|&item| item == index).unwrap_or(0);
                let next = cell
                    .saturating_add_signed(if movement == Movement::Right { 1 } else { -1 })
                    .min(group.len() - 1);
                self.search_focus(group[next]);
            }
            _ => {}
        }
        None
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
                Command::Move(movement @ (Movement::Left | Movement::Right), _) => {
                    self.focus = Some(Control::Search(if movement == Movement::Left {
                        15
                    } else {
                        16
                    }));
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
            if !self.search.open {
                self.search.expanded = false;
                self.search.result_top = 0;
                self.search.result_focus = None;
                self.search.visible_results = 0;
                self.search.requested = None;
                self.search.expected = None;
            }
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
                if self.search.open {
                    return Some(None);
                }
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
            if operation == Operation::ReplaceNext
                && let Some((generation, row)) = self.search.expected
                && (self.screen.search.rows.generation != generation
                    || self.screen.search.rows.current != Some(row))
            {
                self.field_message(if self.screen.search.rows.generation == generation {
                    "Wait for the selected match to be revealed"
                } else {
                    "Results changed; select a match again"
                });
                return Some(None);
            }
            if matches!(operation, Operation::Next | Operation::Previous) {
                self.search.expected = None;
            }
            self.field_error = None;
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
            return Some(Some(Intent::DismissSearch));
        }
        if command == Command::FocusEditor
            && matches!(
                self.focus,
                Some(Control::Search(_) | Control::SearchList | Control::SearchResult(_))
            )
        {
            self.focus = Some(Control::Content);
            self.dirty = true;
            return Some(None);
        }
        if command == Command::FocusSearch && self.focus == Some(Control::Content) {
            self.search_focus(self.search.field);
            return Some(None);
        }
        let Some(focus @ (Control::Search(_) | Control::SearchList | Control::SearchResult(_))) =
            self.focus
        else {
            return None;
        };
        let direct = match command {
            Command::ToggleMatchCase => Some(2),
            Command::ToggleWholeWord => Some(3),
            Command::ToggleSearchScope => Some(4),
            Command::CloseSearch => Some(10),
            Command::InsertQueryTab => Some(11),
            Command::InsertQueryLf => Some(12),
            Command::InsertQueryCrlf => Some(13),
            Command::SearchDetails => Some(14),
            _ => None,
        };
        let focus = direct.map_or(focus, Control::Search);
        if let Command::Move(movement, _) = command {
            return Some(self.search_move(focus, movement, layout));
        }
        if !matches!(focus, Control::Search(_)) {
            return match command {
                Command::Activate | Command::Newline => {
                    let row = match focus {
                        Control::SearchResult(row) => Some(row),
                        _ => self.search.result_focus,
                    };
                    Some(row.and_then(|row| self.search_select(row, layout)))
                }
                Command::Tab | Command::PreviousControl => {
                    let controls = self.search.controls(self.screen.search.busy);
                    self.search_focus(if command == Command::Tab {
                        0
                    } else {
                        *controls.last().unwrap_or(&0)
                    });
                    Some(None)
                }
                _ => None,
            };
        }
        let Control::Search(index) = focus else {
            unreachable!()
        };
        if matches!(command, Command::Tab | Command::PreviousControl) {
            if self.field_paste.is_some() {
                self.field_message("Wait for field paste before changing controls");
                return Some(None);
            }
            let controls = self.search.controls(self.screen.search.busy);
            let current = controls.iter().position(|i| *i == index).unwrap_or(0);
            if command == Command::Tab
                && current + 1 == controls.len()
                && self
                    .search
                    .current_rows(&self.screen.search.rows)
                    .is_some_and(|rows| rows.count != 0)
            {
                self.list_focus();
                return Some(None);
            }
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
        if direct.is_none() && !matches!(command, Command::Activate | Command::Newline) {
            return None;
        }
        if layout.area.width < 40 || layout.area.height < 8 {
            return Some(None);
        }
        if let Some(pending) = &mut self.field_paste {
            if direct.is_none() {
                pending.submit = true;
                self.search_submit_field = Some(self.search.field);
            }
            return Some(None);
        }
        let index = direct.unwrap_or(index);
        if direct.is_none() && !layout.contains(Control::Search(index)) && !matches!(index, 0 | 1) {
            return Some(None);
        }
        if self.search.query.literal.is_empty() && matches!(index, 0 | 1 | 5..=8 | SUBMIT) {
            return Some(None);
        }
        Some(match index {
            0 | SUBMIT => self.search_command(Command::FindNext, layout).flatten(),
            1 | 7 => self.search_command(Command::ReplaceNext, layout).flatten(),
            2 => {
                self.search.query.match_case = !self.search.query.match_case;
                self.dirty = true;
                Some(self.invalidate_search_rows())
            }
            3 => {
                self.search.query.whole_word = !self.search.query.whole_word;
                self.dirty = true;
                Some(self.invalidate_search_rows())
            }
            4 => {
                self.search.all = !self.search.all;
                self.dirty = true;
                Some(self.invalidate_search_rows())
            }
            5 => self.search_command(Command::FindPrevious, layout).flatten(),
            6 => self.search_command(Command::FindNext, layout).flatten(),
            8 => self.search_command(Command::ReplaceAll, layout).flatten(),
            9 => Some(Intent::Editor(Action::CancelSearch)),
            10 => Some(Intent::DismissSearch),
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
                self.opener = self.focus;
                self.modal = Some(Modal::Details(
                    self.screen.search.summary(&self.screen.tabs),
                ));
                self.focus = Some(Control::DetailsClose);
                self.help_scroll = 0;
                self.dirty = true;
                Some(Intent::Editor(Action::RevokeSearch))
            }
            OPTIONS => {
                self.search.expanded = !self.search.expanded;
                self.search_focus(OPTIONS);
                None
            }
            SHOW_RESULTS => {
                if self.search.list_open {
                    let groups = self.search.groups(self.screen.search.busy);
                    self.search_focus(groups[self.search.row.min(groups.len() - 1)][0]);
                } else {
                    self.list_focus();
                }
                None
            }
            FORM_UP | FORM_DOWN => {
                self.search_form_step(index == FORM_DOWN);
                None
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
        app.command(Command::FocusEditor, &layout(&app));
        assert_eq!(app.focus, Some(Control::Content));
        app.command(Command::FocusSearch, &layout(&app));
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
        app.search.expanded = true;
        for index in app.search.controls(false) {
            assert_eq!(app.focus, Some(Control::Search(index)));
            let geometry = layout(&app);
            assert_eq!(geometry.content_height(), 6);
            assert_eq!(geometry.search_bounds().unwrap().height, 3);
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
