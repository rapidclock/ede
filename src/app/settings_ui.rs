//! Draft editing, navigation, and publication for the Settings pane.

use super::*;
use crate::settings::{Base, Chord, Revision, SaveState, Scalar, Update};

const GENERAL_ROWS: usize = Scalar::ALL.len();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operation {
    Scalar(Scalar),
    Add,
    Replace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Confirmation {
    Exit,
    Discard,
    Reload,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Page {
    List,
    Binding,
    Edit(Operation),
    Capture(Operation),
    Preview(Operation, Chord),
    Confirm(Confirmation),
    Info,
    Writing { exit: bool },
}

/// Stable targets for pane-local actions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Button {
    PreviousKey,
    NextKey,
    Edit,
    Exit,
    Add,
    Replace,
    Remove,
    Defaults,
    Back,
    Capture,
    Accept,
    DiscardCapture,
    Discard,
    KeepEditing,
    Save,
    SaveAndExit,
    DiscardAndExit,
    Reload,
    Restore,
    Info,
    CancelSave,
}

impl Button {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::PreviousKey => "Prev",
            Self::NextKey => "Next",
            Self::Edit => "Edit",
            Self::Exit => "Exit",
            Self::Add => "Add",
            Self::Replace => "Replace",
            Self::Remove => "Remove",
            Self::Defaults => "Use defaults",
            Self::Back => "Back",
            Self::Capture => "Capture",
            Self::Accept => "Accept",
            Self::DiscardCapture | Self::Discard => "Discard",
            Self::KeepEditing => "Keep Editing",
            Self::Save => "Save",
            Self::SaveAndExit => "Save and Exit",
            Self::DiscardAndExit => "Discard and Exit",
            Self::Reload => "Reload",
            Self::Restore => "Restore Defaults",
            Self::Info => "Config",
            Self::CancelSave => "Cancel Save",
        }
    }
}

/// One working revision and its publication base, including unaccepted field text.
#[derive(Debug)]
pub(crate) struct Draft {
    base: Option<Base>,
    original: Revision,
    revision: Revision,
    page: Page,
    return_page: Page,
    selected: usize,
    chord: usize,
    pub(crate) generation: u64,
    pub(crate) field: String,
    pub(crate) info_scroll: u16,
    feedback: Option<String>,
    blocked: Option<Chord>,
    field_action: Option<Button>,
}

impl Draft {
    fn new(settings: &settings::Service) -> Self {
        Self {
            base: settings.base.clone(),
            original: settings.applied.clone(),
            revision: settings.applied.clone(),
            page: Page::List,
            return_page: Page::List,
            selected: 0,
            chord: 0,
            generation: 0,
            field: String::new(),
            info_scroll: 0,
            feedback: None,
            blocked: None,
            field_action: None,
        }
    }

    fn index(&self) -> usize {
        self.selected - GENERAL_ROWS
    }

    fn count(&self) -> usize {
        GENERAL_ROWS + self.revision.map.rows.len()
    }

    fn changed(&self) -> bool {
        self.original != self.revision
    }

    fn dirty(&self) -> bool {
        let page = if matches!(self.page, Page::Confirm(_)) {
            self.return_page
        } else {
            self.page
        };
        self.changed()
            || match page {
                Page::Edit(Operation::Scalar(scalar)) => {
                    self.field != self.revision.values.get(scalar).to_string()
                }
                Page::Edit(Operation::Add) | Page::Capture(Operation::Add) => {
                    !self.field.is_empty()
                }
                Page::Edit(Operation::Replace) | Page::Capture(Operation::Replace) => {
                    self.revision.map.rows[self.index()]
                        .keys()
                        .get(self.chord)
                        .is_some_and(|key| self.field != key.to_string())
                }
                Page::Preview(..) => true,
                _ => false,
            }
    }

    /// Label and effective contents of a logical row, including inherited bindings.
    pub(crate) fn row_label(&self, index: usize) -> String {
        if let Some(scalar) = Scalar::ALL.get(index).copied() {
            let source = if self.revision.values.override_value(scalar).is_some() {
                "override"
            } else {
                "default"
            };
            format!(
                "{} {} ({source})",
                scalar.label(),
                self.revision.values.get(scalar)
            )
        } else {
            let row = &self.revision.map.rows[index - GENERAL_ROWS];
            let keys = row
                .keys()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            let source = if row.override_keys.is_some() {
                "override"
            } else {
                "default"
            };
            let value = if keys.is_empty() {
                "Unbound".into()
            } else {
                keys.join(" / ")
            };
            format!("{} · {value} ({source})", row.identifier())
        }
    }

    fn section(&self) -> &'static str {
        match self.selected {
            0 => "Recovery",
            1..GENERAL_ROWS => "Highlighting limits",
            _ => "Keyboard shortcuts",
        }
    }

    pub(crate) fn title(
        &self,
        settings: &settings::Service,
        location: &settings::Location,
    ) -> String {
        let state = if location.safe() {
            "Safe config"
        } else if settings.user_config {
            "User config"
        } else {
            "Defaults"
        };
        let context = if self.page == Page::Info {
            "Config"
        } else {
            self.section()
        };
        format!(
            " Settings · {context} · {state}{} ",
            if self.dirty() { " *" } else { "" }
        )
    }

    pub(crate) fn feedback<'a>(&'a self, settings: &'a settings::Service) -> Option<&'a str> {
        self.feedback.as_deref().or(settings.diagnostic.as_deref())
    }

    pub(crate) fn editing(&self) -> bool {
        matches!(self.page, Page::Edit(_))
    }

    /// A scalar's input occupies the value column of its actual list row.
    pub(crate) fn inline_scalar(&self) -> Option<(usize, Scalar)> {
        match self.page {
            Page::Edit(Operation::Scalar(scalar)) => Some((self.selected, scalar)),
            _ => None,
        }
    }

    pub(crate) fn capturing(&self) -> bool {
        matches!(self.page, Page::Capture(_))
    }

    pub(crate) fn info(&self) -> bool {
        self.page == Page::Info
    }

    /// Keep the selected logical row in the viewport independent of terminal size.
    pub(crate) fn rows(&self, capacity: u16) -> std::ops::Range<usize> {
        if self.page != Page::List && self.inline_scalar().is_none() {
            return 0..0;
        }
        let capacity = usize::from(capacity);
        let start = self
            .selected
            .saturating_sub(capacity / 2)
            .min(self.count().saturating_sub(capacity));
        start..(start + capacity).min(self.count())
    }

    /// Row numbers are relative to the pane interior.
    pub(crate) fn buttons(
        &self,
        height: u16,
        save_state: Option<SaveState>,
        writable: bool,
    ) -> Vec<(u16, Vec<Button>)> {
        use Button as B;
        let footer = height - 1;
        match self.page {
            Page::Confirm(Confirmation::Exit) => {
                return vec![
                    (2, vec![B::SaveAndExit, B::DiscardAndExit]),
                    (footer, vec![B::KeepEditing]),
                ];
            }
            Page::Confirm(_) => return vec![(footer, vec![B::Discard, B::KeepEditing])],
            Page::Writing { .. } => {
                return if save_state == Some(SaveState::Staging) {
                    vec![(footer, vec![B::CancelSave])]
                } else {
                    Vec::new()
                };
            }
            _ => {}
        }
        let mut result = vec![(footer, vec![B::Save, B::Discard, B::Exit])];
        match self.page {
            Page::List => {
                let mut header = vec![B::Info];
                if writable {
                    header.extend([B::Reload, B::Restore]);
                }
                result.insert(0, (0, header));
            }
            Page::Binding if writable => {
                let mut keys = vec![B::PreviousKey, B::NextKey, B::Add];
                let mut edits = vec![B::Defaults, B::Back];
                if !self.revision.map.rows[self.index()].keys().is_empty() {
                    keys.push(B::Replace);
                    edits.insert(0, B::Remove);
                }
                result.extend([(2, keys), (3, edits)]);
            }
            Page::Binding => result.push((3, vec![B::PreviousKey, B::NextKey, B::Back])),
            Page::Edit(Operation::Scalar(_)) => {}
            Page::Edit(_) => result.push((3, vec![B::Capture, B::Accept, B::Back])),
            Page::Capture(_) => result.push((3, vec![B::Back])),
            Page::Preview(..) => result.push((3, vec![B::DiscardCapture, B::Accept])),
            Page::Info => result.push((0, vec![B::Back])),
            _ => {}
        }
        result
    }

    pub(crate) fn lines(
        &self,
        settings: &settings::Service,
        location: &settings::Location,
    ) -> Vec<(u16, String)> {
        match self.page {
            Page::List => Vec::new(),
            Page::Binding => {
                let row = &self.revision.map.rows[self.index()];
                let value = row.keys().get(self.chord).map_or_else(
                    || "Unbound".into(),
                    |chord| format!("{}/{} {chord}", self.chord + 1, row.keys().len()),
                );
                let source = if row.override_keys.is_some() {
                    "override"
                } else {
                    "default"
                };
                vec![(0, row.identifier()), (1, format!("{value} ({source})"))]
            }
            Page::Edit(Operation::Scalar(scalar)) => vec![(
                0,
                format!(
                    "Range 1–{} · default {}",
                    scalar.maximum(),
                    scalar.default()
                ),
            )],
            Page::Edit(op) => vec![
                (0, self.revision.map.rows[self.index()].identifier()),
                (
                    1,
                    match op {
                        Operation::Scalar(_) => unreachable!(),
                        Operation::Add => "Type a shortcut to add".into(),
                        Operation::Replace => "Type the replacement shortcut".into(),
                    },
                ),
            ],
            Page::Capture(_) => vec![(1, "Press a shortcut to preview".into())],
            Page::Preview(_, chord) => vec![
                (0, "Captured shortcut".into()),
                (1, chord.to_string()),
                (2, "Accept adds it to the draft".into()),
            ],
            Page::Confirm(decision) => vec![(
                0,
                match decision {
                    Confirmation::Exit => "Exit with unsaved settings?",
                    Confirmation::Discard => "Discard unsaved settings?",
                    Confirmation::Reload => "Discard draft and reload settings?",
                }
                .into(),
            )],
            Page::Info => vec![(
                1,
                format!(
                    "{}\n{}",
                    location
                        .display_file()
                        .unwrap_or_else(|| "Configuration path unavailable".into()),
                    self.feedback(settings)
                        .unwrap_or(if location.user_directory().is_none() {
                            "Saving and reloading are disabled."
                        } else if settings.base.is_none() {
                            "Reload a valid file before saving."
                        } else {
                            "No configuration errors."
                        })
                ),
            )],
            Page::Writing { .. } => vec![(
                0,
                match settings.save_state() {
                    Some(SaveState::Finishing) => "Finishing save",
                    Some(SaveState::Cancelled) => "Cancelling save…",
                    _ => "Saving settings…",
                }
                .into(),
            )],
        }
    }

    fn reset(&mut self, service: &settings::Service) {
        let selected = self.selected;
        let generation = self.generation.wrapping_add(1);
        *self = Self::new(service);
        self.selected = selected.min(self.count() - 1);
        self.generation = generation;
    }

    fn adjacent(&mut self, delta: isize) {
        match self.page {
            Page::List => {
                self.selected = self
                    .selected
                    .saturating_add_signed(delta)
                    .min(self.count() - 1);
                self.chord = 0;
            }
            Page::Binding => {
                self.chord = self.chord.saturating_add_signed(delta).min(
                    self.revision.map.rows[self.index()]
                        .keys()
                        .len()
                        .saturating_sub(1),
                );
            }
            Page::Info => {
                self.info_scroll = self
                    .info_scroll
                    .saturating_add_signed(delta.clamp(-16, 16) as i16);
            }
            _ => {}
        }
        self.generation = self.generation.wrapping_add(1);
    }

    fn update_keys(&mut self, op: Option<(Operation, Chord)>) -> Result<(), String> {
        let index = self.index();
        let row = &mut self.revision.map.rows[index];
        let mut keys = row.keys().to_vec();
        match op {
            Some((Operation::Add, chord)) if keys.len() < 8 => {
                self.chord = keys.len();
                keys.push(chord);
            }
            Some((Operation::Add, _)) => return Err("At most eight shortcuts per command".into()),
            Some((Operation::Replace, chord)) => {
                *keys.get_mut(self.chord).ok_or("Select a shortcut first")? = chord;
            }
            Some((Operation::Scalar(_), _)) => unreachable!(),
            None if self.chord < keys.len() => {
                keys.remove(self.chord);
                self.chord = self.chord.min(keys.len().saturating_sub(1));
            }
            None => return Ok(()),
        }
        row.override_keys = Some(keys);
        self.revalidate();
        Ok(())
    }

    fn revalidate(&mut self) {
        self.feedback = self.revision.map.revalidate().err();
        self.generation = self.generation.wrapping_add(1);
    }

    fn accept_field(&mut self) -> Result<Page, String> {
        match self.page {
            Page::Edit(Operation::Scalar(scalar)) => {
                let value = scalar
                    .validate(self.field.parse().ok())
                    .map_err(|_| format!("Enter a whole number from 1 to {}", scalar.maximum()))?;
                if value != self.revision.values.get(scalar) {
                    self.revision.values.set(scalar, Some(value));
                }
                Ok(Page::List)
            }
            Page::Edit(operation) => {
                let chord = self.field.parse::<Chord>().map_err(str::to_owned)?;
                self.update_keys(Some((operation, chord)))?;
                Ok(Page::Binding)
            }
            Page::Preview(operation, chord) => {
                self.update_keys(Some((operation, chord)))?;
                Ok(Page::Binding)
            }
            _ => Ok(self.page),
        }
    }

    fn focus(&self) -> Control {
        use Button as B;
        match self.page {
            Page::List => Control::SettingsRow(self.selected),
            Page::Edit(_) => Control::SettingsField,
            Page::Confirm(_) => Control::SettingsButton(B::KeepEditing),
            Page::Preview(..) => Control::SettingsButton(B::DiscardCapture),
            Page::Binding => Control::SettingsButton(B::PreviousKey),
            Page::Writing { .. } => Control::SettingsButton(B::CancelSave),
            _ => Control::SettingsButton(B::Back),
        }
    }
}

impl AppState {
    pub(super) fn open_settings(&mut self) {
        self.settings_saved = false;
        let draft = self
            .settings_ui
            .get_or_insert_with(|| Box::new(Draft::new(&self.settings)));
        self.opener = self.opener.or(self.focus);
        self.help = false;
        self.modal = Some(Modal::Settings);
        self.focus = Some(draft.focus());
        self.capture = None;
        self.field_paste = None;
        self.field_generation = self.field_generation.wrapping_add(1);
        self.dirty = true;
    }

    fn close_settings(&mut self) {
        self.settings_ui = None;
        self.modal = None;
        self.focus = self.opener.take().or(Some(Control::Content));
        self.capture = None;
        self.field_paste = None;
        self.field_generation = self.field_generation.wrapping_add(1);
        self.dirty = true;
        self.show_open_error();
    }

    pub(super) fn settings_updated(&mut self, update: Update) {
        self.settings_saved = update == Update::WriteApplied;
        let Some(draft) = self.settings_ui.as_mut() else {
            return;
        };
        match update {
            Update::ReadApplied
                if !draft.dirty()
                    && matches!(draft.page, Page::List | Page::Binding | Page::Info) =>
            {
                let page = draft.page;
                draft.reset(&self.settings);
                self.switch_settings(page);
            }
            Update::WriteApplied if draft.page == (Page::Writing { exit: true }) => {
                self.close_settings();
            }
            Update::WriteApplied => {
                draft.reset(&self.settings);
                self.switch_settings(Page::List);
            }
            Update::Cancelled if matches!(draft.page, Page::Writing { .. }) => {
                draft.feedback = Some("Settings save cancelled".into());
                self.switch_settings(Page::List);
            }
            Update::Diagnostic if matches!(draft.page, Page::Writing { .. }) => {
                self.switch_settings(Page::List);
            }
            _ => {}
        }
    }

    fn switch_settings(&mut self, page: Page) {
        if let Some(draft) = self.settings_ui.as_mut() {
            draft.page = page;
            draft.generation = draft.generation.wrapping_add(1);
            draft.field_action = None;
            if page == Page::Info {
                draft.info_scroll = 0;
            }
            self.focus = Some(draft.focus());
        }
        self.capture = None;
        self.field_generation = self.field_generation.wrapping_add(1);
        self.field_paste = None;
        self.field_submit = None;
        self.field_error = None;
        self.dirty = true;
    }

    pub(super) fn settings_key(
        &mut self,
        key: KeyEvent,
        command: Option<Command>,
        layout: &LayoutSnapshot,
    ) -> Option<Intent> {
        let draft = self.settings_ui.as_deref_mut()?;
        let raw = Chord::delivered(key);
        if draft.blocked.is_some() {
            if draft.blocked == raw {
                if key.kind == KeyEventKind::Release {
                    draft.blocked = None;
                }
                if layout.overlay.is_some() || command != Some(Command::Cancel) {
                    return None;
                }
            } else if key.kind == KeyEventKind::Press {
                draft.blocked = None;
            }
        }
        if let Page::Capture(operation) = draft.page {
            if layout.overlay.is_none() {
                if command == Some(Command::Cancel) && key.kind == KeyEventKind::Press {
                    self.switch_settings(Page::Edit(operation));
                }
                return None;
            }
            if key.kind != KeyEventKind::Press {
                return None;
            }
            if let Some(chord) = raw {
                draft.blocked = Some(chord);
                self.switch_settings(Page::Preview(operation, chord));
            } else {
                draft.feedback = Some("Unsupported chord. Try typing it instead.".into());
                self.dirty = true;
            }
            return None;
        }
        if let Some(command) = command {
            let old_page = draft.page;
            self.settings_command(command, layout);
            if let Some(draft) = self.settings_ui.as_deref_mut()
                && matches!(draft.page, Page::Capture(_))
                && draft.page != old_page
            {
                draft.blocked = raw;
            }
        } else if self.focus == Some(Control::SettingsField)
            && draft.editing()
            && layout.overlay.is_some()
            && let Some(ch) = crate::input::text(key)
        {
            self.edit_field(FieldEdit::Text(ch.to_string()));
        }
        None
    }

    pub(super) fn settings_command(
        &mut self,
        command: Command,
        layout: &LayoutSnapshot,
    ) -> Option<Intent> {
        use Button as B;
        let draft = self.settings_ui.as_ref()?;
        if command == Command::Cancel {
            if layout.overlay.is_some() && !self.cancel_receiving_paste() {
                self.settings_back(layout);
            }
            return None;
        }
        let area = layout.overlay?;
        let button = match command {
            Command::FocusNext | Command::FocusPrevious => {
                let backwards = command == Command::FocusPrevious;
                self.focus = match self.focus {
                    Some(Control::SettingsRow(_)) if !backwards => {
                        Some(Control::SettingsButton(B::Save))
                    }
                    Some(Control::SettingsRow(_)) if backwards => {
                        Some(Control::SettingsButton(B::Restore))
                            .filter(|button| layout.contains(*button))
                            .or(Some(Control::SettingsButton(B::Info)))
                    }
                    Some(Control::SettingsField) if draft.inline_scalar().is_some() => {
                        Some(Control::SettingsButton(if backwards {
                            B::Exit
                        } else {
                            B::Save
                        }))
                    }
                    _ => match layout.next(self.focus, backwards) {
                        Some(Control::SettingsRow(_)) if draft.inline_scalar().is_some() => {
                            Some(Control::SettingsField)
                        }
                        Some(Control::SettingsRow(_)) => Some(Control::SettingsRow(draft.selected)),
                        next => next,
                    },
                };
                self.dirty = true;
                return None;
            }
            Command::Move(movement, _) => {
                let page = (area.height - 4) as isize;
                let step = match movement {
                    Movement::Up | Movement::Left => -1,
                    Movement::Down | Movement::Right => 1,
                    Movement::PageUp => -page,
                    Movement::PageDown => page,
                    Movement::Home | Movement::Start => -(draft.count() as isize),
                    Movement::End | Movement::Finish => draft.count() as isize,
                };
                self.settings_adjacent(step);
                return None;
            }
            Command::FieldBackspace if self.focus == Some(Control::SettingsField) => {
                self.edit_field(FieldEdit::Backspace);
                return None;
            }
            Command::Activate | Command::Newline => match self.focus {
                Some(Control::SettingsButton(button))
                    if layout.contains(Control::SettingsButton(button)) =>
                {
                    button
                }
                Some(Control::SettingsRow(row)) if layout.contains(Control::SettingsRow(row)) => {
                    self.settings_row(row, layout);
                    return None;
                }
                Some(Control::SettingsField) => B::Accept,
                _ => return None,
            },
            Command::EditSetting | Command::SelectRow => B::Edit,
            Command::AddShortcut => B::Add,
            Command::ReplaceShortcut => B::Replace,
            Command::RemoveShortcut => B::Remove,
            Command::UseDefaults => B::Defaults,
            Command::CaptureShortcut => B::Capture,
            Command::AcceptSetting => B::Accept,
            Command::DiscardCapture => B::DiscardCapture,
            Command::KeepEditing => B::KeepEditing,
            Command::DiscardDraft => match (draft.page, self.focus) {
                (
                    Page::Confirm(Confirmation::Exit),
                    Some(Control::SettingsButton(B::DiscardAndExit)),
                ) => B::DiscardAndExit,
                (Page::Confirm(_), Some(Control::SettingsButton(B::Discard))) => B::Discard,
                (Page::Confirm(_), _) => return None,
                _ => B::Discard,
            },
            Command::SettingsOptions | Command::SettingsInfo => B::Info,
            Command::ReloadSettings => B::Reload,
            Command::SaveSettings => B::Save,
            Command::CancelSave => B::CancelSave,
            Command::RestoreDefaults => B::Restore,
            _ => return None,
        };
        self.settings_button(button, layout);
        None
    }

    pub(super) fn settings_adjacent(&mut self, delta: isize) {
        if let Some(draft) = self.settings_ui.as_deref_mut() {
            draft.adjacent(delta);
            if draft.page == Page::List {
                self.focus = Some(draft.focus());
            }
            self.capture = None;
            self.dirty = true;
        }
    }

    pub(super) fn settings_row(&mut self, row: usize, layout: &LayoutSnapshot) {
        if let Some(draft) = self.settings_ui.as_deref_mut()
            && layout.contains(Control::SettingsRow(row))
        {
            if let Some((selected, _)) = draft.inline_scalar() {
                if selected == row || self.field_paste.is_some() {
                    self.focus = Some(Control::SettingsField);
                    return;
                }
                if let Err(error) = draft.accept_field() {
                    draft.feedback = Some(error);
                    self.focus = Some(Control::SettingsField);
                    self.dirty = true;
                    return;
                }
                draft.page = Page::List;
            }
            if draft.page != Page::List {
                return;
            }
            draft.selected = row;
            draft.chord = 0;
            self.settings_button(Button::Edit, layout);
        }
    }

    fn settings_back(&mut self, layout: &LayoutSnapshot) {
        let Some(draft) = self.settings_ui.as_deref() else {
            return;
        };
        match draft.page {
            Page::List if !draft.dirty() => self.close_settings(),
            Page::List => self.settings_button(Button::Exit, layout),
            Page::Binding | Page::Info | Page::Edit(Operation::Scalar(_)) => {
                self.switch_settings(Page::List)
            }
            Page::Edit(_) => self.switch_settings(Page::Binding),
            Page::Capture(operation) | Page::Preview(operation, _) => {
                self.switch_settings(Page::Edit(operation))
            }
            Page::Confirm(_) => self.switch_settings(draft.return_page),
            Page::Writing { .. } => {
                self.settings.cancel_save();
            }
        }
    }

    pub(super) fn finish_settings_field(&mut self, layout: &LayoutSnapshot) {
        let button = self
            .settings_ui
            .as_mut()
            .and_then(|draft| draft.field_action.take())
            .unwrap_or(Button::Accept);
        self.settings_button(button, layout);
    }

    pub(super) fn settings_button(&mut self, button: Button, layout: &LayoutSnapshot) {
        use Button as B;
        let Some(draft) = self.settings_ui.as_deref_mut() else {
            return;
        };
        let writable = self.config_location.user_directory().is_some();
        let page = draft.page;
        let return_page = draft.return_page;
        let direct = match button {
            B::Edit => page == Page::List,
            B::Accept => draft.inline_scalar().is_some(),
            B::Info | B::Reload | B::Restore => matches!(page, Page::List | Page::Binding),
            _ => false,
        };
        if layout.overlay.is_none() || !layout.contains(Control::SettingsButton(button)) && !direct
        {
            return;
        }
        if !writable
            && matches!(
                button,
                B::Add | B::Replace | B::Remove | B::Defaults | B::Reload | B::Restore | B::Capture
            )
        {
            return;
        }
        if matches!(button, B::Accept | B::Save | B::Exit)
            && let Some(pending) = &mut self.field_paste
        {
            pending.submit = true;
            draft.field_action = Some(button);
            return;
        }
        match button {
            B::PreviousKey => self.settings_adjacent(-1),
            B::NextKey => self.settings_adjacent(1),
            B::Back | B::DiscardCapture => self.settings_back(layout),
            B::Edit => {
                if let Some(scalar) = Scalar::ALL.get(draft.selected).copied() {
                    if writable {
                        draft.field = draft.revision.values.get(scalar).to_string();
                        draft.feedback = None;
                        self.switch_settings(Page::Edit(Operation::Scalar(scalar)));
                    } else {
                        draft.generation = draft.generation.wrapping_add(1);
                        self.focus = Some(draft.focus());
                        self.dirty = true;
                    }
                } else {
                    self.switch_settings(Page::Binding);
                }
            }
            B::Info => self.switch_settings(Page::Info),
            B::Add | B::Replace => {
                let operation = if button == B::Add {
                    draft.field.clear();
                    Operation::Add
                } else {
                    draft.field =
                        draft.revision.map.rows[draft.index()].keys()[draft.chord].to_string();
                    Operation::Replace
                };
                draft.feedback = None;
                self.switch_settings(Page::Edit(operation));
            }
            B::Remove => {
                let _ = draft.update_keys(None);
                self.focus = Some(draft.focus());
                self.dirty = true;
            }
            B::Defaults => {
                let index = draft.index();
                draft.revision.map.rows[index].override_keys = None;
                draft.chord = 0;
                draft.revalidate();
                self.focus = Some(draft.focus());
                self.dirty = true;
            }
            B::Capture => {
                if let Page::Edit(operation) = page {
                    draft.feedback = None;
                    self.switch_settings(Page::Capture(operation));
                }
            }
            B::Accept => match draft.accept_field() {
                Ok(next) => self.switch_settings(next),
                Err(error) => {
                    draft.feedback = Some(error);
                    self.focus = Some(draft.focus());
                    self.dirty = true;
                }
            },
            B::Restore => {
                draft.revision = Revision::default();
                draft.chord = 0;
                draft.revalidate();
                self.switch_settings(Page::List);
            }
            B::Exit if !draft.dirty() => self.close_settings(),
            B::Discard if !draft.dirty() && !matches!(page, Page::Confirm(_)) => {
                draft.reset(&self.settings);
                self.switch_settings(Page::List);
            }
            B::Reload if !draft.dirty() => {
                self.settings.reload();
                self.switch_settings(Page::List);
            }
            B::KeepEditing => self.switch_settings(return_page),
            B::DiscardAndExit => self.close_settings(),
            B::Discard if matches!(page, Page::Confirm(_)) => {
                if page == Page::Confirm(Confirmation::Reload) {
                    self.settings.reload();
                }
                draft.reset(&self.settings);
                self.switch_settings(Page::List);
            }
            B::Exit | B::Discard | B::Reload => {
                draft.return_page = page;
                let confirmation = match button {
                    B::Exit => Confirmation::Exit,
                    B::Discard => Confirmation::Discard,
                    _ => Confirmation::Reload,
                };
                self.switch_settings(Page::Confirm(confirmation));
            }
            B::Save | B::SaveAndExit => self.save_settings(button == B::SaveAndExit),
            B::CancelSave => {
                self.settings.cancel_save();
            }
        }
    }

    /// Include typed edits in one draft, while a captured chord still requires explicit acceptance.
    fn save_settings(&mut self, exit: bool) {
        let Some(draft) = self.settings_ui.as_deref_mut() else {
            return;
        };
        let page = if exit { draft.return_page } else { draft.page };
        draft.page = page;
        self.settings_saved = false;
        let mut error_page = page;
        let result = (|| {
            if self.config_location.user_directory().is_none() {
                return Err("Settings are read-only in this session".into());
            }
            match page {
                Page::Preview(..) | Page::Capture(_) => {
                    return Err("Accept or discard the captured shortcut first".into());
                }
                Page::Edit(_) => error_page = draft.accept_field()?,
                _ => {}
            }
            draft.revision.map.revalidate()?;
            let base = draft
                .base
                .clone()
                .ok_or("Reload a valid settings file before saving")?;
            self.settings
                .save(&self.config_location, base, draft.revision.clone())
        })();
        match result {
            Ok(()) => {
                draft.feedback = None;
                self.switch_settings(Page::Writing { exit });
            }
            Err(error) => {
                draft.feedback = Some(error);
                self.switch_settings(error_page);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    #[test]
    fn capture_suppresses_normalized_case_deliveries() {
        let mut app = AppState::new();
        app.settings
            .applied
            .map
            .set_override(
                Context::Settings,
                "accept_setting",
                vec!["Ctrl+A".parse().unwrap()],
            )
            .unwrap();
        app.settings.applied.map.revalidate().unwrap();
        app.open_settings();
        let draft = app.settings_ui.as_mut().unwrap();
        draft.selected = draft
            .revision
            .map
            .rows
            .iter()
            .position(|row| row.command == Command::Settings)
            .unwrap()
            + GENERAL_ROWS;
        draft.page = Page::Capture(Operation::Add);
        let area = Rect::new(0, 0, 40, 8);
        let layout = LayoutSnapshot::for_app(area, &app, 1);
        app.input(
            Input::Key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::CONTROL)),
            &layout,
        );
        assert!(matches!(
            app.settings_ui.as_ref().unwrap().page,
            Page::Preview(..)
        ));
        let layout = LayoutSnapshot::for_app(area, &app, 2);
        app.input(
            Input::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            &layout,
        );
        assert!(
            matches!(app.settings_ui.as_ref().unwrap().page, Page::Preview(..)),
            "the same captured chord must remain blocked"
        );
    }

    #[test]
    fn capture_treats_escape_as_data_and_suppresses_held_keys_until_release() {
        let mut app = AppState::new();
        app.open_settings();
        let draft = app.settings_ui.as_mut().unwrap();
        draft.selected = GENERAL_ROWS;
        draft.page = Page::Capture(Operation::Add);
        let area = Rect::new(0, 0, 40, 8);
        let escape = |kind| {
            Input::Key(KeyEvent::new_with_kind(
                KeyCode::Esc,
                KeyModifiers::NONE,
                kind,
            ))
        };
        let layout = LayoutSnapshot::for_app(area, &app, 1);
        app.input(escape(KeyEventKind::Repeat), &layout);
        app.input(escape(KeyEventKind::Release), &layout);
        assert!(app.settings_ui.as_ref().unwrap().capturing());
        app.input(escape(KeyEventKind::Press), &layout);

        let layout = LayoutSnapshot::for_app(area, &app, 2);
        app.input(escape(KeyEventKind::Press), &layout);
        app.input(escape(KeyEventKind::Repeat), &layout);
        let draft = app.settings_ui.as_deref().unwrap();
        assert_eq!(
            draft.page,
            Page::Preview(Operation::Add, "Escape".parse().unwrap())
        );
        assert_eq!(
            app.focus,
            Some(Control::SettingsButton(Button::DiscardCapture))
        );
        assert!(!draft.changed());
        app.input(escape(KeyEventKind::Release), &layout);
        app.input(escape(KeyEventKind::Press), &layout);
        let draft = app.settings_ui.as_deref().unwrap();
        assert!(draft.editing());
        assert!(!draft.changed());
    }

    #[test]
    fn reload_does_not_discard_in_progress_field() {
        let mut app = AppState::new();
        app.open_settings();
        let draft = app.settings_ui.as_mut().unwrap();
        draft.page = Page::Edit(Operation::Scalar(Scalar::Checkpoint));
        draft.field = "37".into();
        app.settings_updated(Update::ReadApplied);
        let draft = app.settings_ui.as_ref().unwrap();
        assert_eq!(draft.field, "37");
        assert!(draft.editing());
    }

    #[test]
    fn back_button_returns_from_config_to_list() {
        let mut app = AppState::new();
        app.open_settings();
        app.settings_ui.as_mut().unwrap().page = Page::Info;
        let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 40, 8), &app, 1);
        app.settings_button(Button::Back, &layout);
        assert!(matches!(app.modal, Some(Modal::Settings)));
        assert_eq!(app.settings_ui.as_ref().unwrap().page, Page::List);
    }

    /// Repeated legacy shortcuts cannot accept a discard while Keep Editing owns focus.
    #[test]
    fn remapped_discard_requires_focused_confirmation() {
        let mut app = AppState::new();
        app.settings
            .applied
            .map
            .set_override(
                Context::Settings,
                "discard_draft",
                vec!["F9".parse().unwrap()],
            )
            .unwrap();
        app.settings.applied.map.revalidate().unwrap();
        app.open_settings();
        app.settings_ui
            .as_mut()
            .unwrap()
            .revision
            .values
            .set(Scalar::Checkpoint, Some(31));
        let area = Rect::new(0, 0, 40, 8);
        let key = || Input::Key(KeyEvent::new(KeyCode::F(9), KeyModifiers::NONE));
        let layout = LayoutSnapshot::for_app(area, &app, 1);
        app.input(key(), &layout);
        assert_eq!(
            app.focus,
            Some(Control::SettingsButton(Button::KeepEditing))
        );
        let layout = LayoutSnapshot::for_app(area, &app, 2);
        app.input(key(), &layout);
        assert_eq!(
            app.settings_ui.as_ref().unwrap().page,
            Page::Confirm(Confirmation::Discard)
        );
        assert_eq!(
            app.settings_ui
                .as_ref()
                .unwrap()
                .revision
                .values
                .get(Scalar::Checkpoint),
            31
        );
        app.input(Input::Command(Command::FocusPrevious), &layout);
        app.input(key(), &layout);
        assert_eq!(app.settings_ui.as_ref().unwrap().page, Page::List);
        assert_eq!(
            app.settings_ui
                .as_ref()
                .unwrap()
                .revision
                .values
                .get(Scalar::Checkpoint),
            5
        );
    }

    /// Exit protects typed input while a later capture is awaiting a key.
    #[test]
    fn capture_protects_pending_typed_shortcut() {
        let mut app = AppState::new();
        app.open_settings();
        let draft = app.settings_ui.as_mut().unwrap();
        draft.selected = GENERAL_ROWS;
        draft.page = Page::Capture(Operation::Add);
        draft.field = "F10".into();
        let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 40, 8), &app, 1);
        app.settings_button(Button::Exit, &layout);
        let draft = app.settings_ui.as_ref().unwrap();
        assert_eq!(draft.page, Page::Confirm(Confirmation::Exit));
        assert_eq!(draft.field, "F10");
    }
}
