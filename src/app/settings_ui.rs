//! Draft editing, navigation, and publication for the Settings pane.

use super::*;
use crate::settings::{Base, Chord, Revision, SaveState, Scalar, Update};
use unicode_width::UnicodeWidthStr;

mod shortcuts;
pub(crate) use shortcuts::group_label;
use shortcuts::{Category, command_label};

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
    Writing { exit: bool },
}

/// Settings categories share a draft while exposing disjoint editor rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Section {
    Recovery,
    Highlighting,
    Keyboard,
}

impl Section {
    pub(crate) const ALL: [Self; 3] = [Self::Recovery, Self::Highlighting, Self::Keyboard];

    pub(crate) const fn label(self, compact: bool) -> &'static str {
        match (self, compact) {
            (Self::Recovery, _) => "Recovery",
            (Self::Highlighting, true) => "Limits",
            (Self::Highlighting, false) => "Highlighting Limits",
            (Self::Keyboard, true) => "Keys",
            (Self::Keyboard, false) => "Keyboard Shortcuts",
        }
    }

    fn adjacent(self, delta: isize) -> Self {
        Self::ALL[(self as usize)
            .saturating_add_signed(delta)
            .min(Self::ALL.len() - 1)]
    }
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
    CancelSave,
    Diagnostic,
}

impl Button {
    /// Primary and confirmation controls align together at the right of the pane.
    pub(crate) fn align_right(self) -> bool {
        matches!(
            self,
            Self::Save
                | Self::Discard
                | Self::Exit
                | Self::SaveAndExit
                | Self::DiscardAndExit
                | Self::KeepEditing
                | Self::CancelSave
        )
    }

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
            Self::CancelSave => "Cancel Save",
            Self::Diagnostic => "Details",
        }
    }
}

/// Category labels occupy a display row without becoming editable targets.
pub(crate) enum ListEntry {
    Heading(&'static str),
    Row(usize),
}

/// One working revision and its publication base, including unaccepted field text.
#[derive(Debug)]
pub(crate) struct Draft {
    base: Option<Base>,
    original: Revision,
    revision: Revision,
    shortcut_order: Vec<usize>,
    page: Page,
    return_page: Page,
    selected: usize,
    chord: usize,
    pub(crate) generation: u64,
    pub(crate) field: String,
    feedback: Option<String>,
    blocked: Option<Chord>,
    field_action: Option<Button>,
}

impl Draft {
    fn new(settings: &settings::Service) -> Self {
        let rows = &settings.applied.map.rows;
        let mut shortcut_order: Vec<_> = (GENERAL_ROWS..GENERAL_ROWS + rows.len()).collect();
        shortcut_order.sort_by_key(|index| {
            let row = &rows[index - GENERAL_ROWS];
            (
                row.context as usize,
                Category::for_command(row.context, row.command),
                row.command.id(),
            )
        });
        Self {
            base: settings.base.clone(),
            original: settings.applied.clone(),
            revision: settings.applied.clone(),
            shortcut_order,
            page: Page::List,
            return_page: Page::List,
            selected: 0,
            chord: 0,
            generation: 0,
            field: String::new(),
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

    /// Describe a scalar row with its current value and source.
    pub(crate) fn row_label(&self, index: usize) -> String {
        let scalar = Scalar::ALL[index];
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
    }

    /// Format a shortcut row for the two display columns.
    pub(crate) fn shortcut_columns(&self, index: usize, width: u16) -> (&'static str, String) {
        let row = &self.revision.map.rows[index - GENERAL_ROWS];
        let keys = row.keys();
        let mut shortcut = if keys.is_empty() {
            "Unbound".to_owned()
        } else {
            keys.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" / ")
        };
        if keys.len() > 1 && shortcut.width() > usize::from(width) {
            shortcut = format!("{} +{}", keys[0], keys.len() - 1);
        }
        let sourced = format!(
            "{shortcut} · {}",
            if row.override_keys.is_some() {
                "Custom"
            } else {
                "Default"
            }
        );
        if sourced.width() <= usize::from(width) {
            shortcut = sourced;
        }
        (command_label(row.context, row.command), shortcut)
    }

    /// Expose the exact config key when a binding is open.
    pub(crate) fn binding_id(&self) -> Option<String> {
        if self.section() == Section::Keyboard
            && matches!(
                self.page,
                Page::Binding | Page::Edit(_) | Page::Capture(_) | Page::Preview(..)
            )
        {
            Some(self.revision.map.rows[self.index()].identifier())
        } else {
            None
        }
    }

    /// The displayed group and its visible focus exist only in the shortcut list.
    pub(crate) fn shortcut_group(&self) -> Option<Context> {
        (self.page == Page::List && self.section() == Section::Keyboard)
            .then(|| self.revision.map.rows[self.index()].context)
    }

    fn group_rows(&self, context: Context) -> &[usize] {
        let compare = |index: &usize| self.revision.map.rows[index - GENERAL_ROWS].context as usize;
        let id = context as usize;
        let start = self
            .shortcut_order
            .partition_point(|index| compare(index) < id);
        let end = self
            .shortcut_order
            .partition_point(|index| compare(index) <= id);
        &self.shortcut_order[start..end]
    }

    fn row_start(&self) -> usize {
        self.shortcut_group().map_or_else(
            || self.section_rows(self.section()).start,
            |context| self.group_rows(context)[0],
        )
    }

    fn section_focus(&self) -> Control {
        self.shortcut_group().map_or(
            Control::SettingsSection(self.section()),
            Control::SettingsGroup,
        )
    }

    /// Resolve the category of the selected logical row.
    pub(crate) fn section(&self) -> Section {
        match self.selected {
            0 => Section::Recovery,
            1..GENERAL_ROWS => Section::Highlighting,
            _ => Section::Keyboard,
        }
    }

    fn section_rows(&self, section: Section) -> std::ops::Range<usize> {
        match section {
            Section::Recovery => 0..1,
            Section::Highlighting => 1..GENERAL_ROWS,
            Section::Keyboard => GENERAL_ROWS..self.count(),
        }
    }

    pub(crate) fn show_sections(&self) -> bool {
        self.page == Page::List || self.inline_scalar().is_some()
    }

    pub(crate) fn show_utilities(&self) -> bool {
        self.page == Page::List
    }

    pub(crate) fn title(
        &self,
        settings: &settings::Service,
        location: &settings::Location,
        width: u16,
    ) -> String {
        let dirty = if self.dirty() { " *" } else { "" };
        if let Some(id) = self.binding_id() {
            let title = format!(" Binding · {id}{dirty} ");
            return if title.width() <= usize::from(width - 2) {
                title
            } else {
                format!(" {id}{dirty} ")
            };
        }
        let state = if location.safe() {
            "Safe config"
        } else if settings.user_config {
            "User config"
        } else {
            "Defaults"
        };
        let context = self
            .section()
            .label(width < crate::ui::SETTINGS_SPLIT_WIDTH);
        format!(" Settings · {context} · {state}{dirty} ")
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

    /// Keep the selected command visible, including categories when the pane has room.
    pub(crate) fn entries(&self, capacity: u16) -> Vec<ListEntry> {
        let capacity = usize::from(capacity);
        if capacity == 0 || !self.show_sections() {
            return Vec::new();
        }
        let mut entries = Vec::new();
        if let Some(context) = self.shortcut_group() {
            let mut last = None;
            for &index in self.group_rows(context) {
                let row = &self.revision.map.rows[index - GENERAL_ROWS];
                let heading = Category::for_command(context, row.command).label();
                if capacity > 1 && Some(heading) != last {
                    entries.push(ListEntry::Heading(heading));
                    last = Some(heading);
                }
                entries.push(ListEntry::Row(index));
            }
        } else {
            entries.extend(self.section_rows(self.section()).map(ListEntry::Row));
        }
        let selected = entries
            .iter()
            .position(|entry| matches!(entry, ListEntry::Row(row) if *row == self.selected))
            .unwrap_or(0);
        let start = selected
            .saturating_sub(capacity / 2)
            .min(entries.len().saturating_sub(capacity));
        entries.into_iter().skip(start).take(capacity).collect()
    }

    /// Row numbers are relative to the pane interior.
    pub(crate) fn buttons(
        &self,
        height: u16,
        save_state: Option<SaveState>,
        writable: bool,
        compact: bool,
        show_utilities: bool,
    ) -> Vec<(u16, Vec<Button>)> {
        use Button as B;
        let footer = height - 1;
        match self.page {
            Page::Confirm(Confirmation::Exit) => {
                return if compact {
                    vec![
                        (footer - 1, vec![B::SaveAndExit, B::DiscardAndExit]),
                        (footer, vec![B::KeepEditing]),
                    ]
                } else {
                    vec![(
                        footer,
                        vec![B::SaveAndExit, B::DiscardAndExit, B::KeepEditing],
                    )]
                };
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
            Page::List if writable && show_utilities => {
                let row = if compact { footer - 1 } else { footer };
                result.push((row, vec![B::Reload, B::Restore]));
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
            _ => {}
        }
        result
    }

    /// Confirmation and write status reserve the pane for their own actions.
    pub(crate) fn show_diagnostic(&self) -> bool {
        !matches!(self.page, Page::Confirm(_) | Page::Writing { .. })
    }

    pub(crate) fn lines(&self, settings: &settings::Service) -> Vec<(u16, String)> {
        match self.page {
            Page::List => Vec::new(),
            Page::Binding => {
                let row = &self.revision.map.rows[self.index()];
                let value = row.keys().get(self.chord).map_or_else(
                    || "Unbound".into(),
                    |chord| format!("{}/{} {chord}", self.chord + 1, row.keys().len()),
                );
                let source = if row.override_keys.is_some() {
                    "Custom"
                } else {
                    "Default"
                };
                vec![
                    (0, command_label(row.context, row.command).into()),
                    (1, format!("{value} ({source})")),
                ]
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
                (0, {
                    let row = &self.revision.map.rows[self.index()];
                    command_label(row.context, row.command).into()
                }),
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
                if let Some(context) = self.shortcut_group() {
                    let rows = self.group_rows(context);
                    let index = rows
                        .iter()
                        .position(|row| *row == self.selected)
                        .unwrap_or(0);
                    self.selected = rows[index.saturating_add_signed(delta).min(rows.len() - 1)];
                } else {
                    let rows = self.section_rows(self.section());
                    self.selected = self
                        .selected
                        .saturating_add_signed(delta)
                        .clamp(rows.start, rows.end - 1);
                }
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
    /// Inline feedback takes precedence over a service diagnostic.
    pub(crate) fn settings_feedback(&self) -> Option<&str> {
        self.field_error
            .as_deref()
            .or_else(|| self.settings_ui.as_ref()?.feedback(&self.settings))
    }

    pub(super) fn open_settings(&mut self) {
        self.settings_saved = false;
        let draft = self
            .settings_ui
            .get_or_insert_with(|| Box::new(Draft::new(&self.settings)));
        self.opener = self.focus;
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
                if !draft.dirty() && matches!(draft.page, Page::List | Page::Binding) =>
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
            Command::Tab | Command::FocusNext | Command::FocusPrevious => {
                self.focus = layout.next_setting(
                    self.focus,
                    command == Command::FocusPrevious,
                    draft.section(),
                    draft.shortcut_group(),
                    draft.focus(),
                );
                self.dirty = true;
                return None;
            }
            Command::Move(movement, _) => {
                let compact = area.width < crate::ui::SETTINGS_SPLIT_WIDTH;
                if let Some(Control::SettingsGroup(_)) = self.focus {
                    let step = match movement {
                        Movement::Up if !compact => -1,
                        Movement::Down if !compact => 1,
                        Movement::Left if compact => -1,
                        Movement::Right if compact => 1,
                        Movement::Home | Movement::Start => -(Context::ALL.len() as isize),
                        Movement::End | Movement::Finish => Context::ALL.len() as isize,
                        Movement::Left | Movement::Up => {
                            self.focus = Some(Control::SettingsSection(Section::Keyboard));
                            self.dirty = true;
                            return None;
                        }
                        Movement::Right | Movement::Down => {
                            self.focus = Some(draft.focus());
                            self.dirty = true;
                            return None;
                        }
                        _ => return None,
                    };
                    self.settings_group_adjacent(step);
                    return None;
                }
                if let Some(Control::SettingsSection(section)) = self.focus {
                    if section == Section::Keyboard && !compact && movement == Movement::Down {
                        self.focus = Some(draft.section_focus());
                        self.dirty = true;
                        return None;
                    }
                    let delta = match movement {
                        Movement::Up if !compact => -1,
                        Movement::Down if !compact => 1,
                        Movement::Left if compact => -1,
                        Movement::Right if compact => 1,
                        Movement::Home | Movement::Start => -3,
                        Movement::End | Movement::Finish => 3,
                        Movement::Right | Movement::Down => {
                            self.focus = Some(
                                draft
                                    .shortcut_group()
                                    .map_or(draft.focus(), Control::SettingsGroup),
                            );
                            self.dirty = true;
                            return None;
                        }
                        _ => return None,
                    };
                    self.settings_section(section.adjacent(delta), layout);
                    return None;
                }
                if draft.page == Page::List
                    && matches!(self.focus, Some(Control::SettingsRow(_)))
                    && (movement == Movement::Left && !compact
                        || movement == Movement::Up
                            && compact
                            && draft.selected == draft.row_start())
                {
                    self.focus = Some(draft.section_focus());
                    self.dirty = true;
                    return None;
                }
                let page = layout.list_rows().max(1) as isize;
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
                Some(Control::SettingsSection(section))
                    if layout.contains(Control::SettingsSection(section)) =>
                {
                    self.focus = Some(
                        draft
                            .shortcut_group()
                            .map_or(draft.focus(), Control::SettingsGroup),
                    );
                    self.dirty = true;
                    return None;
                }
                Some(Control::SettingsGroup(group))
                    if layout.contains(Control::SettingsGroup(group)) =>
                {
                    self.focus = Some(draft.focus());
                    self.dirty = true;
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
            Command::SettingsOptions | Command::SettingsInfo => {
                if matches!(draft.page, Page::List | Page::Binding) {
                    self.open_about(self.focus.or(Some(draft.focus())));
                }
                return None;
            }
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

    /// Select a shortcut group without changing any bindings in the working draft.
    pub(super) fn settings_group(&mut self, group: Context) {
        let Some(draft) = self.settings_ui.as_deref_mut() else {
            return;
        };
        if let Some(active) = draft.shortcut_group() {
            if active != group {
                draft.selected = draft.group_rows(group)[0];
                draft.chord = 0;
                draft.generation = draft.generation.wrapping_add(1);
            }
            self.focus = Some(Control::SettingsGroup(group));
            self.capture = None;
            self.dirty = true;
        }
    }

    /// Move through scopes in the same order for arrow keys, wheels, and the compact buttons.
    pub(super) fn settings_group_adjacent(&mut self, step: isize) {
        if let Some(group) = self
            .settings_ui
            .as_ref()
            .and_then(|draft| draft.shortcut_group())
        {
            let index = (group as usize)
                .saturating_add_signed(step)
                .min(Context::ALL.len() - 1);
            self.settings_group(Context::ALL[index]);
        }
    }

    /// Change sections after validating any pending inline scalar edit.
    pub(super) fn settings_section(&mut self, section: Section, layout: &LayoutSnapshot) {
        let Some(draft) = self.settings_ui.as_deref_mut() else {
            return;
        };
        if !layout.contains(Control::SettingsSection(section)) || !draft.show_sections() {
            return;
        }
        if draft.inline_scalar().is_some() {
            if self.field_paste.is_some() {
                self.focus = Some(Control::SettingsField);
                self.dirty = true;
                return;
            }
            if let Err(error) = draft.accept_field() {
                draft.feedback = Some(error);
                self.focus = Some(Control::SettingsField);
                self.dirty = true;
                return;
            }
            draft.feedback = None;
        }
        if section != draft.section() {
            draft.selected = if section == Section::Keyboard {
                draft.group_rows(Context::Global)[0]
            } else {
                draft.section_rows(section).start
            };
            draft.chord = 0;
        }
        self.switch_settings(Page::List);
        self.focus = Some(Control::SettingsSection(section));
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
            Page::Binding | Page::Edit(Operation::Scalar(_)) => self.switch_settings(Page::List),
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
            B::Reload | B::Restore => matches!(page, Page::List | Page::Binding),
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
            B::Diagnostic if self.field_paste.is_none() => {
                let focus = self.focus.unwrap_or(Control::SettingsButton(B::Diagnostic));
                if let Some(text) = self.settings_feedback() {
                    self.open_settings_diagnostic(text.to_owned(), focus);
                }
            }
            B::Diagnostic => {
                self.focus = Some(draft.focus());
                self.dirty = true;
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
    fn legacy_config_shortcut_opens_about_and_returns_to_the_draft() {
        let mut app = AppState::new();
        app.open_settings();
        let draft = app.settings_ui.as_mut().unwrap();
        draft.revision.values.set(Scalar::Checkpoint, Some(17));
        let original_focus = app.focus;
        let area = Rect::new(0, 0, 40, 8);
        let layout = LayoutSnapshot::for_app(area, &app, 1);
        app.settings_command(Command::SettingsInfo, &layout);
        assert!(matches!(app.modal, Some(Modal::About { .. })));
        let layout = LayoutSnapshot::for_app(area, &app, 2);
        app.command(Command::Cancel, &layout);
        assert!(matches!(app.modal, Some(Modal::Settings)));
        assert_eq!(app.focus, original_focus);
        let draft = app.settings_ui.as_ref().unwrap();
        assert_eq!(draft.page, Page::List);
        assert_eq!(draft.revision.values.get(Scalar::Checkpoint), 17);
    }

    /// Opening a diagnostic waits for an admitted field paste to settle.
    #[test]
    fn diagnostics_preserve_pending_settings_field_paste() {
        let mut app = AppState::new();
        app.open_settings();
        app.settings.diagnostic = Some("Long configuration diagnostic ".repeat(4));
        app.switch_settings(Page::Edit(Operation::Scalar(Scalar::Checkpoint)));
        app.field_paste = Some(super::super::FieldPaste {
            generation: 10,
            search_field: None,
            edits: Vec::new(),
            bytes: 0,
            submit: false,
        });
        let area = Rect::new(0, 0, 40, 8);
        let layout = LayoutSnapshot::for_app(area, &app, 1);
        app.focus = Some(Control::SettingsButton(Button::Diagnostic));
        app.settings_button(Button::Diagnostic, &layout);
        assert!(matches!(app.modal, Some(Modal::Settings)));
        assert_eq!(app.focus, Some(Control::SettingsField));
        app.event(super::super::Event::FieldPaste(10, Ok("17".into())));
        assert_eq!(app.settings_ui.as_ref().unwrap().field, "17");
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
