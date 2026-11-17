//! Keyboard- and mouse-operable editing of one settings revision and its disk base.

use super::*;
use crate::settings::{Base, Chord, Revision, SaveState, Scalar, Update};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operation {
    Scalar(Scalar),
    Add,
    Replace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Confirmation {
    Close,
    Reload,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Page {
    List,
    Binding,
    Edit(Operation),
    Capture(Operation),
    Preview(Operation, Chord),
    Options,
    Confirm(Confirmation),
    Info,
    Writing,
}

/// A visible button on the active settings page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Button {
    PreviousRow,
    NextRow,
    Edit,
    Options,
    Close,
    PreviousKey,
    NextKey,
    Add,
    Replace,
    Remove,
    Defaults,
    Back,
    Capture,
    Accept,
    Discard,
    KeepEditing,
    Confirm,
    Save,
    Reload,
    Restore,
    Info,
    CancelSave,
}

impl Button {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::PreviousRow | Self::PreviousKey => "Prev",
            Self::NextRow | Self::NextKey => "Next",
            Self::Edit => "Edit",
            Self::Options => "Options",
            Self::Close => "Close",
            Self::Add => "Add",
            Self::Replace => "Replace",
            Self::Remove => "Remove",
            Self::Defaults => "Use defaults",
            Self::Back => "Back",
            Self::Capture => "Capture",
            Self::Accept => "Accept",
            Self::Discard => "Discard",
            Self::KeepEditing => "Keep Editing",
            Self::Confirm => "Discard draft",
            Self::Save => "Save Settings",
            Self::Reload => "Reload Settings",
            Self::Restore => "Restore defaults",
            Self::Info => "Config info",
            Self::CancelSave => "Cancel Save",
        }
    }
}

/// Draft and navigation persist independently of asynchronous document updates.
#[derive(Debug)]
pub(crate) struct Draft {
    base: Option<Base>,
    original: Revision,
    revision: Revision,
    page: Page,
    selected: usize,
    chord: usize,
    pub(crate) generation: u64,
    pub(crate) field: String,
    pub(crate) info_scroll: u16,
    invalid: Option<String>,
    feedback: Option<String>,
    blocked: Option<Chord>,
}

impl Draft {
    fn new(settings: &settings::Service) -> Self {
        Self {
            base: settings.base.clone(),
            original: settings.applied.clone(),
            revision: settings.applied.clone(),
            page: Page::List,
            selected: 0,
            chord: 0,
            generation: 0,
            field: String::new(),
            info_scroll: 0,
            invalid: None,
            feedback: None,
            blocked: None,
        }
    }

    fn scalar(&self) -> Option<Scalar> {
        Scalar::ALL.get(self.selected).copied()
    }

    fn index(&self) -> usize {
        self.selected.saturating_sub(Scalar::ALL.len())
    }

    fn changed(&self) -> bool {
        self.original != self.revision
    }

    fn row_label(&self) -> String {
        if let Some(scalar) = self.scalar() {
            format!("{} {}", scalar.label(), self.revision.values.get(scalar))
        } else {
            self.revision.map.rows[self.index()].identifier()
        }
    }

    fn detail(&self) -> String {
        if let Some(scalar) = self.scalar() {
            return format!(
                "Range 1–{} • default {}",
                scalar.maximum(),
                scalar.default()
            );
        }
        let row = &self.revision.map.rows[self.index()];
        let source = if row.override_keys.is_some() {
            "Override"
        } else {
            "Default"
        };
        if let Some(chord) = row.keys().get(self.chord) {
            let suffix = format!("{}/{} {chord}", self.chord + 1, row.keys().len());
            if source.len() + suffix.len() >= 34 {
                format!("{} {suffix}", &source[..1])
            } else {
                format!("{source} {suffix}")
            }
        } else {
            format!("{source} • Unbound")
        }
    }

    pub(crate) fn editing(&self) -> bool {
        matches!(self.page, Page::Edit(_))
    }

    pub(crate) fn capturing(&self) -> bool {
        matches!(self.page, Page::Capture(_))
    }

    pub(crate) fn info(&self) -> bool {
        self.page == Page::Info
    }

    pub(crate) fn buttons(
        &self,
        save_state: Option<SaveState>,
        writable: bool,
        can_save: bool,
    ) -> Vec<(u16, Vec<Button>)> {
        use Button as B;
        match self.page {
            Page::List => vec![
                (1, vec![B::PreviousRow, B::NextRow]),
                (3, vec![B::Edit, B::Options, B::Close]),
            ],
            Page::Binding => {
                if !writable {
                    return vec![(2, vec![B::PreviousKey, B::NextKey]), (3, vec![B::Back])];
                }
                let mut keys = vec![B::PreviousKey, B::NextKey, B::Add];
                let mut edits = vec![B::Defaults, B::Back];
                if !self.revision.map.rows[self.index()].keys().is_empty() {
                    keys.push(B::Replace);
                    edits.insert(0, B::Remove);
                }
                vec![(2, keys), (3, edits)]
            }
            Page::Edit(Operation::Scalar(_)) => vec![(3, vec![B::Accept, B::Back])],
            Page::Edit(_) => vec![(3, vec![B::Capture, B::Accept, B::Back])],
            Page::Capture(_) => vec![(3, vec![B::Back])],
            Page::Preview(..) => vec![(3, vec![B::Discard, B::Accept])],
            Page::Options if writable => vec![
                (
                    1,
                    if can_save {
                        vec![B::Save, B::Reload]
                    } else {
                        vec![B::Reload]
                    },
                ),
                (2, vec![B::Restore, B::Info]),
                (3, vec![B::Back, B::Close]),
            ],
            Page::Options => vec![(2, vec![B::Info]), (3, vec![B::Back, B::Close])],
            Page::Confirm(_) => vec![(3, vec![B::KeepEditing, B::Confirm])],
            Page::Info => vec![(3, vec![B::Back])],
            Page::Writing if save_state == Some(SaveState::Staging) => {
                vec![(3, vec![B::CancelSave])]
            }
            Page::Writing => vec![(3, vec![B::Close])],
        }
    }

    pub(crate) fn lines(
        &self,
        settings: &settings::Service,
        location: &settings::Location,
    ) -> Vec<(u16, String)> {
        let error = self
            .feedback
            .as_deref()
            .or(self.invalid.as_deref())
            .or(settings.diagnostic.as_deref());
        let state = match (location.safe(), settings.user_config) {
            (true, _) => "Safe config",
            (_, true) => "User config",
            _ => "Defaults",
        };
        match self.page {
            Page::List => vec![
                (
                    0,
                    format!(
                        "{state}{}  {}/{}",
                        if self.changed() { " • Draft" } else { "" },
                        self.selected + 1,
                        Scalar::ALL.len() + self.revision.map.rows.len()
                    ),
                ),
                (2, self.row_label()),
            ],
            Page::Binding => vec![(0, self.row_label()), (1, self.detail())],
            Page::Edit(op) => vec![
                (
                    0,
                    match op {
                        Operation::Scalar(scalar) => {
                            format!("{} • 1–{}", scalar.label(), scalar.maximum())
                        }
                        Operation::Add => "Type a chord to add".into(),
                        Operation::Replace => "Type the replacement chord".into(),
                    },
                ),
                (
                    2,
                    error
                        .unwrap_or(if matches!(op, Operation::Scalar(_)) {
                            ""
                        } else {
                            "For example Ctrl+S or F9"
                        })
                        .to_owned(),
                ),
            ],
            Page::Capture(_) => vec![
                (0, "Press a shortcut to preview".into()),
                (1, error.unwrap_or("Any supported chord").to_owned()),
                (2, "Back cancels capture".into()),
            ],
            Page::Preview(_, chord) => {
                let preview = format!("Captured {chord}");
                if preview.len() <= 34 {
                    vec![(0, preview), (1, "Accept adds it to the draft".into())]
                } else {
                    vec![
                        (0, "Captured shortcut".into()),
                        (1, chord.to_string()),
                        (2, "Accept adds it to the draft".into()),
                    ]
                }
            }
            Page::Options => vec![(
                0,
                if let Some(error) = error {
                    if error.len() > 33 {
                        "See Config info for the diagnostic".into()
                    } else {
                        error.to_owned()
                    }
                } else if location.user_directory().is_none() {
                    "Saving and reloading disabled".into()
                } else if settings.base.is_none() {
                    "Reload before saving".into()
                } else {
                    state.to_owned()
                },
            )],
            Page::Confirm(decision) => vec![(
                0,
                match decision {
                    Confirmation::Close => "Discard unsaved settings?".into(),
                    Confirmation::Reload => "Discard draft and reload settings?".into(),
                },
            )],
            Page::Info => vec![(
                0,
                format!(
                    "{state}\n{}\n{}",
                    location
                        .display_file()
                        .unwrap_or_else(|| "Configuration path unavailable".into()),
                    error.unwrap_or(if location.user_directory().is_none() {
                        "Saving and reloading are disabled."
                    } else if settings.base.is_none() {
                        "Reload a valid file before saving."
                    } else {
                        "No configuration errors."
                    })
                ),
            )],
            Page::Writing => vec![(
                0,
                match settings.save_state() {
                    Some(SaveState::Finishing) => "Finishing save".into(),
                    _ => "Saving settings…".into(),
                },
            )],
        }
    }

    fn reset(&mut self, service: &settings::Service) {
        let selected = self.selected;
        let generation = self.generation.wrapping_add(1);
        *self = Self::new(service);
        self.selected = selected;
        self.generation = generation;
    }

    fn validate(&mut self) {
        self.invalid = self.revision.map.revalidate().err();
        self.feedback = self.invalid.clone();
        self.generation = self.generation.wrapping_add(1);
    }

    fn adjacent(&mut self, delta: isize) {
        if self.page == Page::List {
            self.selected = self
                .selected
                .saturating_add_signed(delta)
                .min(Scalar::ALL.len() + self.revision.map.rows.len() - 1);
            self.chord = 0;
        } else if self.page == Page::Binding {
            self.chord = self.chord.saturating_add_signed(delta).min(
                self.revision.map.rows[self.index()]
                    .keys()
                    .len()
                    .saturating_sub(1),
            );
        } else if self.page == Page::Info {
            self.info_scroll = self
                .info_scroll
                .saturating_add_signed(delta.clamp(-8, 8) as i16);
        }
        self.generation = self.generation.wrapping_add(1);
    }

    fn bind(&mut self, operation: Operation, chord: Chord) -> Result<(), String> {
        let index = self.index();
        let row = &mut self.revision.map.rows[index];
        let keys = row
            .override_keys
            .get_or_insert_with(|| row.defaults.clone());
        match operation {
            Operation::Add if keys.len() < 8 => {
                self.chord = keys.len();
                keys.push(chord);
            }
            Operation::Add => return Err("At most eight shortcuts per command".into()),
            Operation::Replace => {
                *keys.get_mut(self.chord).ok_or("Select a shortcut first")? = chord;
            }
            Operation::Scalar(_) => unreachable!(),
        }
        self.validate();
        Ok(())
    }
}

impl AppState {
    pub(super) fn open_settings(&mut self) {
        if self.settings_ui.is_none() {
            self.settings_ui = Some(Box::new(Draft::new(&self.settings)));
        }
        self.opener = self.opener.or(self.focus);
        self.help = false;
        self.modal = Some(Modal::Settings);
        self.focus = Some(Control::SettingsButton(Button::Edit));
        self.capture = None;
        self.field_paste = None;
        self.field_generation = self.field_generation.wrapping_add(1);
        self.dirty = true;
    }

    fn close_settings(&mut self, preserve: bool) {
        if !preserve {
            self.settings_ui = None;
        }
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
                if !draft.changed()
                    && matches!(
                        draft.page,
                        Page::List | Page::Binding | Page::Options | Page::Info
                    ) =>
            {
                draft.reset(&self.settings);
            }
            Update::WriteApplied => {
                if matches!(self.modal, Some(Modal::Settings)) {
                    draft.reset(&self.settings);
                    draft.feedback = Some("Settings saved".into());
                } else {
                    self.settings_ui = None;
                }
            }
            Update::Diagnostic if draft.page == Page::Writing => {
                draft.page = Page::Options;
                draft.generation = draft.generation.wrapping_add(1);
                if matches!(self.modal, Some(Modal::Settings)) {
                    self.focus = Some(Control::SettingsButton(Button::Info));
                }
            }
            _ => {}
        }
    }

    fn switch_settings(&mut self, page: Page, focus: Control) {
        if let Some(draft) = self.settings_ui.as_mut() {
            draft.page = page;
            draft.generation = draft.generation.wrapping_add(1);
            if page == Page::Info {
                draft.info_scroll = 0;
            }
        }
        self.focus = Some(focus);
        self.capture = None;
        self.field_generation = self.field_generation.wrapping_add(1);
        self.field_paste = None;
        self.field_submit = None;
        self.field_error = None;
        self.dirty = true;
    }

    fn settings_page(&mut self, page: Page, button: Button) {
        self.switch_settings(page, Control::SettingsButton(button));
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
                // Below the layout floor a dedicated Cancel still exits capture.
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
                    self.switch_settings(Page::Edit(operation), Control::SettingsField);
                }
                return None;
            }
            if key.kind != KeyEventKind::Press {
                return None;
            }
            if let Some(chord) = Chord::delivered(key) {
                draft.blocked = Some(chord);
                self.settings_page(Page::Preview(operation, chord), Button::Discard);
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
        let draft = self.settings_ui.as_ref()?;
        if command == Command::Cancel {
            if self.cancel_receiving_paste() {
                return None;
            }
            self.settings_back(layout);
            return None;
        }
        layout.overlay?;
        let button = match command {
            Command::FocusNext | Command::FocusPrevious => {
                self.focus = layout.next(self.focus, command == Command::FocusPrevious);
                self.dirty = true;
                return None;
            }
            Command::Move(movement, _) => {
                let count = Scalar::ALL.len() + draft.revision.map.rows.len();
                let step = match movement {
                    Movement::Up | Movement::Left => -1,
                    Movement::Down | Movement::Right => 1,
                    Movement::PageUp => -8,
                    Movement::PageDown => 8,
                    Movement::Home | Movement::Start => -(count as isize),
                    Movement::End | Movement::Finish => count as isize,
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
                Some(Control::SettingsField) => Button::Accept,
                _ => return None,
            },
            Command::EditSetting | Command::SelectRow => Button::Edit,
            Command::AddShortcut => Button::Add,
            Command::ReplaceShortcut => Button::Replace,
            Command::RemoveShortcut => Button::Remove,
            Command::UseDefaults => Button::Defaults,
            Command::CaptureShortcut => Button::Capture,
            Command::AcceptSetting => Button::Accept,
            Command::DiscardCapture => Button::Discard,
            Command::KeepEditing => Button::KeepEditing,
            Command::DiscardDraft
                if self.focus == Some(Control::SettingsButton(Button::Confirm)) =>
            {
                Button::Confirm
            }
            Command::SettingsOptions => Button::Options,
            Command::SettingsInfo => Button::Info,
            Command::ReloadSettings => Button::Reload,
            Command::SaveSettings => Button::Save,
            Command::CancelSave => Button::CancelSave,
            Command::RestoreDefaults => Button::Restore,
            _ => return None,
        };
        self.settings_button(button, layout);
        None
    }

    pub(super) fn settings_adjacent(&mut self, delta: isize) {
        if let Some(draft) = self.settings_ui.as_deref_mut() {
            draft.adjacent(delta);
            self.capture = None;
            self.dirty = true;
        }
    }

    fn settings_back(&mut self, layout: &LayoutSnapshot) {
        let Some(draft) = self.settings_ui.as_deref() else {
            return;
        };
        match draft.page {
            Page::List | Page::Options if !draft.changed() => self.close_settings(false),
            Page::List | Page::Options if layout.overlay.is_some() => {
                self.settings_page(Page::Confirm(Confirmation::Close), Button::KeepEditing);
            }
            Page::List | Page::Options => {}
            Page::Binding => self.settings_page(Page::List, Button::Edit),
            Page::Confirm(_) if layout.overlay.is_some() => {
                self.settings_page(Page::List, Button::Edit)
            }
            Page::Confirm(_) => {}
            Page::Info => self.settings_page(Page::Options, Button::Info),
            Page::Edit(Operation::Scalar(_)) => self.settings_page(Page::List, Button::Edit),
            Page::Edit(_) => self.settings_page(Page::Binding, Button::Add),
            Page::Capture(operation) | Page::Preview(operation, _) => {
                self.switch_settings(Page::Edit(operation), Control::SettingsField);
            }
            Page::Writing => {
                if self.settings.cancel_save() {
                    self.settings_page(Page::Options, Button::Save);
                } else if self.settings.save_state() == Some(SaveState::Finishing) {
                    self.close_settings(true);
                }
            }
        }
    }

    pub(super) fn settings_button(&mut self, button: Button, layout: &LayoutSnapshot) {
        use Button as B;
        let Some(draft) = self.settings_ui.as_deref_mut() else {
            return;
        };
        let writable = self.config_location.user_directory().is_some();
        let page = draft.page;
        let visible = layout.contains(Control::SettingsButton(button));
        let direct = match button {
            B::Save | B::Reload | B::Restore => {
                matches!(page, Page::List | Page::Binding | Page::Options)
            }
            _ => false,
        };
        if !visible && !direct {
            return;
        }
        if !writable
            && matches!(
                button,
                B::Add
                    | B::Replace
                    | B::Remove
                    | B::Defaults
                    | B::Save
                    | B::Reload
                    | B::Restore
                    | B::Capture
            )
        {
            return;
        }
        match button {
            B::PreviousRow | B::PreviousKey => self.settings_adjacent(-1),
            B::NextRow | B::NextKey => self.settings_adjacent(1),
            B::Back if page == Page::Options => self.settings_page(Page::List, B::Edit),
            B::Close | B::Back => self.settings_back(layout),
            B::Edit => {
                if let Some(scalar) = draft.scalar() {
                    if writable {
                        draft.field = draft.revision.values.get(scalar).to_string();
                        self.switch_settings(
                            Page::Edit(Operation::Scalar(scalar)),
                            Control::SettingsField,
                        );
                    }
                } else {
                    self.settings_page(Page::Binding, B::PreviousKey);
                }
            }
            B::Options => {
                let focus = if self.settings.base.is_some() {
                    B::Save
                } else if writable {
                    B::Reload
                } else {
                    B::Info
                };
                self.settings_page(Page::Options, focus)
            }
            B::Info => self.settings_page(Page::Info, B::Back),
            B::Add | B::Replace => {
                let operation = if button == B::Add {
                    Operation::Add
                } else {
                    Operation::Replace
                };
                draft.field.clear();
                draft.feedback = None;
                self.switch_settings(Page::Edit(operation), Control::SettingsField);
            }
            B::Remove | B::Defaults => {
                let index = draft.index();
                let row = &mut draft.revision.map.rows[index];
                if button == B::Remove {
                    let keys = row
                        .override_keys
                        .get_or_insert_with(|| row.defaults.clone());
                    if draft.chord < keys.len() {
                        keys.remove(draft.chord);
                        draft.chord = draft.chord.min(keys.len().saturating_sub(1));
                    }
                } else {
                    row.override_keys = None;
                    draft.chord = 0;
                }
                draft.validate();
                self.dirty = true;
            }
            B::Capture => {
                if let Page::Edit(operation) = page {
                    draft.feedback = None;
                    self.settings_page(Page::Capture(operation), B::Back);
                }
            }
            B::Discard => self.settings_back(layout),
            B::KeepEditing => self.settings_page(Page::List, B::Edit),
            B::Accept => {
                if !matches!(page, Page::Edit(_) | Page::Preview(..)) {
                    return;
                }
                if let Some(pending) = self.field_paste.as_mut() {
                    pending.submit = true;
                    return;
                }
                let result = match page {
                    Page::Edit(Operation::Scalar(scalar)) => {
                        scalar.validate(draft.field.parse().ok()).map(|value| {
                            draft.revision.values.set(scalar, Some(value));
                            draft.feedback = None;
                        })
                    }
                    Page::Edit(operation) => draft
                        .field
                        .parse::<Chord>()
                        .map_err(str::to_owned)
                        .and_then(|chord| draft.bind(operation, chord)),
                    Page::Preview(operation, chord) => draft.bind(operation, chord),
                    _ => unreachable!(),
                };
                if let Err(error) = result {
                    draft.feedback = Some(error);
                    self.dirty = true;
                } else if matches!(page, Page::Edit(Operation::Scalar(_))) {
                    self.settings_page(Page::List, B::Edit);
                } else {
                    self.settings_page(Page::Binding, B::Add);
                }
            }
            B::Restore => {
                draft.revision = Revision::default();
                draft.chord = 0;
                draft.validate();
                self.settings_page(Page::List, B::Edit);
            }
            B::Reload if draft.changed() => {
                self.settings_page(Page::Confirm(Confirmation::Reload), B::KeepEditing)
            }
            B::Reload => {
                self.settings.reload();
                self.settings_page(Page::List, B::Edit);
            }
            B::Confirm => match page {
                Page::Confirm(Confirmation::Close) => self.close_settings(false),
                Page::Confirm(Confirmation::Reload) => {
                    draft.reset(&self.settings);
                    self.settings.reload();
                    self.settings_page(Page::List, B::Edit);
                }
                _ => {}
            },
            B::Save => {
                self.settings_saved = false;
                let save = if let Some(error) = &draft.invalid {
                    Err(error.clone())
                } else if let Some(base) = draft.base.clone() {
                    self.settings
                        .save(&self.config_location, base, draft.revision.clone())
                } else {
                    Err("Reload a valid settings file before saving".into())
                };
                if let Err(error) = save {
                    draft.feedback = Some(error);
                    self.settings_page(Page::Options, B::Info);
                } else {
                    draft.feedback = None;
                    self.settings_page(Page::Writing, B::CancelSave);
                }
            }
            B::CancelSave => {
                if self.settings.cancel_save() {
                    self.settings_page(Page::Options, B::Save);
                }
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
            + Scalar::ALL.len();
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
        draft.selected = Scalar::ALL.len();
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
        assert_eq!(app.focus, Some(Control::SettingsButton(Button::Discard)));
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
    fn back_button_returns_from_options_to_list() {
        let mut app = AppState::new();
        app.open_settings();
        app.settings_ui.as_mut().unwrap().page = Page::Options;
        let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 40, 8), &app, 1);
        app.settings_button(Button::Back, &layout);
        assert!(matches!(app.modal, Some(Modal::Settings)));
        assert_eq!(app.settings_ui.as_ref().unwrap().page, Page::List);
    }
}
