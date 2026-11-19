//! Modal routing for the file picker and the overflow tab list.

use super::*;

impl AppState {
    /// Open a details dialog without changing the active document or its viewport.
    pub(super) fn open_tab_details(&mut self, tab: Option<TabId>, from_tabs: bool) {
        self.modal = Some(Modal::TabDetails { tab, from_tabs });
        self.focus_details();
    }

    /// Open About, carrying the Settings control to restore for legacy bindings.
    pub(super) fn open_about(&mut self, settings_focus: Option<Control>) {
        self.modal = Some(Modal::About { settings_focus });
        self.focus_details();
    }

    /// Let a user read a full diagnostic without leaving the current Settings draft.
    pub(super) fn open_settings_diagnostic(&mut self, text: String, settings_focus: Control) {
        self.modal = Some(Modal::SettingsDiagnostic {
            text,
            settings_focus,
        });
        self.focus_details();
    }

    fn focus_details(&mut self) {
        self.focus = Some(Control::DetailsClose);
        self.help_scroll = 0;
        self.capture = None;
        self.dirty = true;
    }

    /// Restore the parent of a child dialog without consuming the workspace opener.
    pub(super) fn return_to_parent(&mut self) -> bool {
        (self.modal, self.focus) = match &self.modal {
            Some(Modal::TabDetails {
                tab,
                from_tabs: true,
            }) => (Some(Modal::Tabs(*tab)), Some(Control::TabListDetails)),
            Some(
                Modal::About {
                    settings_focus: Some(focus),
                }
                | Modal::SettingsDiagnostic {
                    settings_focus: focus,
                    ..
                },
            ) => (Some(Modal::Settings), Some(*focus)),
            _ => return false,
        };
        self.capture = None;
        self.dirty = true;
        true
    }

    /// Details can refresh only the named, non-streaming tab they describe.
    pub(crate) fn details_refresh_target(&self) -> Option<TabId> {
        let tab = match self.modal {
            Some(Modal::TabDetails { tab, .. }) => tab?,
            Some(Modal::Details(_)) => self.screen.tab?,
            _ => return None,
        };
        if self.screen.tab == Some(tab) {
            return (self.screen.path.is_some() && self.screen.stream.is_none()).then_some(tab);
        }
        self.screen.tabs.iter().find_map(|entry| {
            (entry.id == tab && entry.path_hint.is_some() && entry.stream.is_none()).then_some(tab)
        })
    }

    /// Display the running package version and the resolved configuration path safely.
    pub(crate) fn about(&self) -> String {
        let location = self.config_location.display_file().map_or_else(
            || "Configuration path unavailable".into(),
            |path| format!("Configuration file\n{path}"),
        );
        let suffix = if self.config_location.safe() {
            "\nSafe config uses bundled settings."
        } else {
            ""
        };
        format!("ede {}\n{location}{suffix}", env!("CARGO_PKG_VERSION"))
    }

    /// Apply a choice to the tab the picker was opened for, or keep it pending at tiny sizes.
    pub(super) fn syntax_picker_command(
        &mut self,
        command: Command,
        layout: &LayoutSnapshot,
    ) -> Option<Intent> {
        if layout.list_rows() == 0 {
            return None;
        }
        if matches!(command, Command::Activate | Command::Newline)
            && self.focus == Some(Control::PathCancel)
        {
            return self.command(Command::Cancel, layout);
        }
        let (row, end) = match &mut self.modal {
            Some(Modal::Language { row, .. }) => (row, Choice::options().count() - 1),
            Some(Modal::SyntaxTheme { config, row }) => (row, config.themes().count() - 1),
            _ => return None,
        };
        *row = match command {
            Command::FocusNext | Command::FocusPrevious => {
                let backwards = command == Command::FocusPrevious;
                if self.focus == Some(Control::PathCancel) {
                    if backwards { end } else { 0 }
                } else if (backwards && *row == 0) || (!backwards && *row == end) {
                    self.focus = Some(Control::PathCancel);
                    self.dirty = true;
                    return None;
                } else if backwards {
                    *row - 1
                } else {
                    *row + 1
                }
            }
            Command::Move(Movement::Up, _) => row.saturating_sub(1),
            Command::Move(Movement::Down, _) => (*row + 1).min(end),
            Command::Move(Movement::Home | Movement::Start, _) => 0,
            Command::Move(Movement::End | Movement::Finish, _) => end,
            Command::Move(Movement::PageUp, _) => row.saturating_sub(layout.list_rows() as usize),
            Command::Move(Movement::PageDown, _) => (*row + layout.list_rows() as usize).min(end),
            Command::Newline | Command::Activate | Command::SelectRow => {
                let row = match self.focus {
                    Some(Control::LanguageRow(clicked)) if command == Command::Activate => clicked,
                    _ => *row,
                };
                let action = match &self.modal {
                    Some(Modal::Language { tab, .. }) => {
                        Action::SetLanguage(*tab, Choice::options().nth(row)?)
                    }
                    Some(Modal::SyntaxTheme { config, .. }) => {
                        Action::SyntaxTheme(Arc::from(config.themes().nth(row)?))
                    }
                    _ => return None,
                };
                self.modal = None;
                self.focus = self.opener.take().or(Some(Control::Content));
                self.capture = None;
                self.dirty = true;
                return Some(Intent::Editor(action));
            }
            _ => return None,
        };
        self.focus = Some(Control::LanguageRow(*row));
        self.dirty = true;
        None
    }

    pub(super) fn tab_change(&mut self, action: Action) -> Option<Intent> {
        self.capture = None;
        self.opener = None;
        self.focus = Some(Control::Content);
        self.dirty = true;
        Some(Intent::Editor(action))
    }

    pub(super) fn paste_target(&self) -> PasteTarget {
        if self.search_field_focused() {
            return PasteTarget::SearchField(self.field_generation, self.search.field);
        }
        if self.field_open()
            && (matches!(
                self.focus,
                Some(Control::PathField | Control::SettingsField)
            ) || self.search_field_focused())
        {
            PasteTarget::Field(self.field_generation)
        } else if self.editor_focused() && self.ready() && self.guard_request.is_none() {
            if self.screen.stream.is_some() {
                PasteTarget::ReadOnly
            } else {
                PasteTarget::Tab(self.screen.tab)
            }
        } else {
            PasteTarget::Ignore
        }
    }

    pub(super) fn request_picker(
        &mut self,
        path: PathBuf,
        browse: Option<Browse>,
    ) -> Option<Intent> {
        let Some(Modal::Open(picker)) = &mut self.modal else {
            return None;
        };
        self.picker_generation = self.picker_generation.wrapping_add(1);
        picker.generation = self.picker_generation;
        self.capture = None;
        self.dirty = true;
        if browse.is_none()
            && self
                .screen
                .reserved_paths
                .iter()
                .any(|reserved| reserved.as_os_str() == path.as_os_str())
        {
            picker.loading = false;
            picker.requested_file = None;
            self.field_message(&format!(
                "Path busy with Save As — {}",
                crate::text::safe(&path.to_string_lossy())
            ));
            return Some(Intent::CancelPicker);
        }
        picker.loading = true;
        picker.requested_file = browse.is_none().then(|| path.clone());
        self.field_error = None;
        Some(Intent::Picker(picker::Request {
            generation: picker.generation,
            path,
            browse,
        }))
    }

    /// Only the current dialog request can install a page or open a tab.
    pub fn picker_reply(&mut self, reply: picker::Reply) -> Option<Intent> {
        let Some(Modal::Open(picker)) = &mut self.modal else {
            return None;
        };
        if picker.generation != reply.generation {
            return None;
        }
        picker.loading = false;
        self.capture = None;
        self.dirty = true;
        if let Some(path) = picker.requested_file.take()
            && self
                .screen
                .reserved_paths
                .iter()
                .any(|reserved| reserved.as_os_str() == path.as_os_str())
        {
            self.field_message(&format!(
                "Path busy with Save As — {}",
                crate::text::safe(&path.to_string_lossy())
            ));
            return None;
        }
        match reply.result {
            Ok(picker::Outcome::Page(page)) => {
                picker.accept(page);
                self.field_error = None;
            }
            Ok(picker::Outcome::File(path)) => {
                let follow = picker.follow;
                self.modal = None;
                self.field_paste = None;
                self.field_submit = None;
                return self.tab_change(if follow {
                    Action::Follow(path)
                } else {
                    Action::Open(path)
                });
            }
            Err(error) => self.field_message(&error),
        }
        None
    }

    pub(super) fn picker_command(
        &mut self,
        command: Command,
        layout: &LayoutSnapshot,
    ) -> Option<Intent> {
        layout.overlay?;
        let Some(Modal::Open(picker)) = &mut self.modal else {
            return None;
        };
        let target = match command {
            Command::PickerParent => Some(Control::PickerUp),
            Command::PickerPrevious => Some(Control::PickerPrevious),
            Command::PickerNext => Some(Control::PickerNext),
            Command::SelectRow => Some(Control::PickerList),
            _ => None,
        };
        if target.is_some_and(|target| !layout.contains(target)) {
            return None;
        }
        match command {
            Command::Backspace if self.focus == Some(Control::PathField) => {
                self.edit_field(FieldEdit::Backspace)
            }
            Command::Tab | Command::FocusNext | Command::FocusPrevious => {
                self.focus = layout.next(self.focus, command == Command::FocusPrevious);
                self.dirty = true;
            }
            Command::Move(movement, _) => {
                let rows = layout.list_rows() as isize;
                let delta = match movement {
                    Movement::Up => -1,
                    Movement::Down => 1,
                    Movement::PageUp => -rows,
                    Movement::PageDown => rows,
                    _ => return None,
                };
                let at_boundary = if delta < 0 {
                    picker.highlight == 0
                } else {
                    picker.highlight + 1 >= picker.count()
                };
                if at_boundary
                    && matches!(movement, Movement::PageUp | Movement::PageDown)
                    && let Some(browse) = picker.page_request(delta < 0)
                {
                    let directory = picker.directory.clone();
                    return self.request_picker(directory, Some(browse));
                }
                picker.scroll(delta);
                self.focus = Some(Control::PickerList);
                self.dirty = true;
            }
            Command::Activate
            | Command::Newline
            | Command::PickerParent
            | Command::PickerPrevious
            | Command::PickerNext
            | Command::SelectRow => match target.or(self.focus) {
                Some(Control::PathCancel) => return self.command(Command::Cancel, layout),
                Some(Control::PathField) => {
                    if let Some(pending) = &mut self.field_paste {
                        pending.submit = true;
                        return None;
                    }
                    if picker.field.is_empty() {
                        self.field_message("Enter a file or directory path");
                        return None;
                    }
                    let path = picker.field_path();
                    return self.request_picker(path, None);
                }
                Some(Control::PickerUp) => {
                    let path = picker.parent()?;
                    return self.request_picker(path, Some(Browse::First));
                }
                Some(Control::PickerPrevious | Control::PickerNext) => {
                    let browse = picker
                        .page_request(target.or(self.focus) == Some(Control::PickerPrevious))?;
                    let directory = picker.directory.clone();
                    return self.request_picker(directory, Some(browse));
                }
                Some(Control::PickerList | Control::PickerOpen) => {
                    let path = picker.directory.join(&picker.row(picker.highlight)?.name);
                    return self.request_picker(path, None);
                }
                _ => {}
            },
            _ => {}
        }
        None
    }

    pub(super) fn tabs_command(
        &mut self,
        command: Command,
        layout: &LayoutSnapshot,
    ) -> Option<Intent> {
        layout.overlay?;
        let Some(Modal::Tabs(highlight)) = &mut self.modal else {
            return None;
        };
        let requested = match command {
            Command::SelectRow => Some(Control::TabList),
            Command::CloseSelectedTab => Some(Control::TabListClose),
            _ => None,
        };
        if requested.is_some_and(|target| {
            !layout.contains(target)
                || command == Command::CloseSelectedTab && self.focus != Some(target)
        }) {
            return None;
        }
        match command {
            Command::Tab | Command::FocusNext | Command::FocusPrevious => {
                self.focus = layout.next(self.focus, command == Command::FocusPrevious);
                self.dirty = true;
            }
            Command::Move(movement, _) => {
                let rows = layout.list_rows() as isize;
                let delta = match movement {
                    Movement::Up => -1,
                    Movement::Down => 1,
                    Movement::PageUp => -rows,
                    Movement::PageDown => rows,
                    _ => return None,
                };
                let index = self
                    .screen
                    .tabs
                    .iter()
                    .position(|t| Some(t.id) == *highlight)
                    .unwrap_or(0);
                let next = index
                    .saturating_add_signed(delta)
                    .min(self.screen.tabs.len().saturating_sub(1));
                *highlight = self.screen.tabs.get(next).map(|t| t.id);
                self.focus = Some(Control::TabList);
                self.dirty = true;
            }
            Command::Activate
            | Command::Newline
            | Command::SelectRow
            | Command::CloseSelectedTab => {
                let target = requested.or(self.focus)?;
                if target == Control::PathCancel {
                    return self.command(Command::Cancel, layout);
                }
                let id = highlight.filter(|id| self.screen.tabs.iter().any(|tab| tab.id == *id))?;
                if !layout.contains(target) {
                    return None;
                }
                let action = match target {
                    Control::TabList | Control::TabListSwitch => Action::Switch(id),
                    Control::TabListClose => Action::Guard(After::Close(id)),
                    Control::TabListDetails => {
                        self.open_tab_details(Some(id), true);
                        return None;
                    }
                    _ => return None,
                };
                self.modal = None;
                if let Action::Guard(After::Close(id)) = action {
                    self.guard_return = Some(Modal::Tabs(Some(id)));
                    self.capture = None;
                    self.dirty = true;
                    return Some(Intent::Editor(action));
                }
                return self.tab_change(action);
            }
            _ => {}
        }
        None
    }

    pub(super) fn dialog_wheel(&mut self, up: bool, layout: &LayoutSnapshot) -> bool {
        if matches!(self.modal, Some(Modal::Recovery(_))) {
            for _ in 0..3 {
                self.recovery_command(
                    Command::Move(if up { Movement::Up } else { Movement::Down }, false),
                    layout,
                );
            }
            return true;
        }
        if let Some(Modal::Open(picker)) = &mut self.modal {
            picker.scroll(if up { -3 } else { 3 });
            self.dirty = true;
            return true;
        }
        if matches!(self.modal, Some(Modal::Tabs(_))) {
            for _ in 0..3 {
                self.tabs_command(
                    Command::Move(if up { Movement::Up } else { Movement::Down }, false),
                    layout,
                );
            }
            return true;
        }
        false
    }

    pub(super) fn dialog_click(&mut self, control: Control) -> bool {
        match (&mut self.modal, control) {
            (Some(Modal::Recovery(dialog)), Control::RecoveryRow(row)) => {
                if dialog.loading || row >= dialog.page.rows.len() {
                    return true;
                }
                dialog.highlight = row;
                dialog.revision = dialog.revision.wrapping_add(1);
                self.focus = Some(Control::RecoveryList);
                self.dirty = true;
            }
            (Some(Modal::Tabs(selected)), Control::TabListRow(tab)) => {
                *selected = Some(tab);
                self.focus = Some(Control::TabList);
                self.dirty = true;
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A busy Save As destination revokes any older picker reply and keeps field input.
    #[test]
    fn reserved_picker_destination_revokes_earlier_reply() {
        let mut app = AppState::new();
        let path = PathBuf::from("/tmp/pending-save");
        app.screen.reserved_paths = vec![Arc::from(path.as_path())].into();
        let mut dialog = picker::Dialog::new(PathBuf::from("/tmp"));
        dialog.field = "pending-save".into();
        app.modal = Some(Modal::Open(Box::new(dialog)));
        app.focus = Some(Control::PathField);
        assert!(matches!(
            app.request_picker("/tmp".into(), Some(Browse::First)),
            Some(Intent::Picker(_))
        ));
        assert!(matches!(
            app.request_picker(path, None),
            Some(Intent::CancelPicker)
        ));
        assert!(
            app.picker_reply(picker::Reply {
                generation: 1,
                result: Ok(picker::Outcome::File("/tmp/old".into())),
            })
            .is_none()
        );
        assert!(matches!(&app.modal, Some(Modal::Open(dialog)) if dialog.field == "pending-save"));
        assert!(
            app.field_error
                .as_deref()
                .unwrap()
                .contains("busy with Save As")
        );
    }
}
