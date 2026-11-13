//! Terminal-independent interaction state, modal routing, and named runtime intents.
use crate::{
    command::{Command, Movement},
    document::{Cancellation, JobId},
    editor::{Action, After, Event, PointerGesture, SaveResult, Screen},
    input::Input,
    status::{Severity, StatusMessage},
    ui::{Control, LayoutSnapshot},
};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use std::{path::PathBuf, sync::Arc, time::Duration};

/// Effects performed by the runtime, never by drawing or hit-testing.
#[derive(Clone, Debug)]
pub(crate) enum Intent {
    Quit,
    Suspend,
    MouseCapture(bool),
    Editor(Action),
    CancelWork,
}
/// Direct-path dialog intent; paths remain separate from safe display labels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PathKind {
    Open,
    Save,
    GoTo,
}
/// Only one overlay owns keyboard and pointer interaction at a time.
#[derive(Clone, Debug)]
pub(crate) enum Modal {
    Menu(usize),
    Path(PathKind, String),
    Confirm(After, Option<JobId>),
    Details(String),
    SaveError(String),
}
/// Captured pointer target and its geometry; text capture never activates crossed controls.
#[derive(Clone, Copy)]
struct Capture {
    control: Control,
    generation: u64,
    press: u64,
    x: u16,
    y: u16,
}
/// Bounded field keystrokes that arrived after a paste awaiting its worker read.
enum FieldEdit {
    Text(String),
    Backspace,
}
/// Preserve ordering without doing clipboard file I/O on the terminal thread.
struct FieldPaste {
    generation: u64,
    edits: Vec<FieldEdit>,
    bytes: usize,
    submit: bool,
}
/// Shell state contains only bounded prepared data; document bytes remain with storage.
pub(crate) struct AppState {
    pub help: bool,
    pub help_scroll: u16,
    pub modal: Option<Modal>,
    pub focus: Option<Control>,
    pub status: StatusMessage,
    pub dirty: bool,
    pub screen: Screen,
    pub mouse_capture: bool,
    pub pending: bool,
    opener: Option<Control>,
    capture: Option<Capture>,
    pointer_press: u64,
    paste: Option<Cancellation>,
    pending_after: Option<After>,
    save_dialog: Option<u64>,
    guard_request: Option<u64>,
    last_drag: Duration,
    field_generation: u64,
    field_paste: Option<FieldPaste>,
    field_submit: Option<u64>,
    pub field_error: Option<String>,
}
/// Discoverable menu actions; every item shares keyboard command dispatch.
pub(crate) const MENU: &[(Command, &str)] = &[
    (Command::New, "New                 Ctrl+N / Cmd+N"),
    (Command::Open, "Open                Ctrl+O / Cmd+O"),
    (Command::Save, "Save                Ctrl+S / Cmd+S"),
    (Command::SaveAs, "Save As             F4 / Cmd+Shift+S"),
    (Command::Undo, "Undo                Ctrl+Z / Cmd+Z"),
    (Command::Redo, "Redo                Ctrl+Y / Cmd+Shift+Z"),
    (Command::Cut, "Cut                 Ctrl+X / Cmd+X"),
    (Command::Copy, "Copy                Ctrl+C / Cmd+C"),
    (Command::Paste, "Internal paste      Ctrl+V / Cmd+V"),
    (Command::SelectAll, "Select all          Ctrl+A / Cmd+A"),
    (Command::GoTo, "Go to line:cell      Ctrl+G"),
    (Command::MouseCapture, "Toggle mouse capture"),
    (Command::Help, "Help                F1"),
    (Command::Suspend, "Suspend"),
    (Command::Quit, "Quit                Ctrl+Q / Cmd+Q"),
];
impl AppState {
    /// Begin with text focus and a visible asynchronous preparation state.
    pub fn new() -> Self {
        Self {
            help: false,
            help_scroll: 0,
            modal: None,
            focus: Some(Control::Content),
            status: StatusMessage::new(Severity::Info, "Preparing document"),
            dirty: true,
            screen: Screen::default(),
            mouse_capture: true,
            pending: false,
            opener: None,
            capture: None,
            pointer_press: 0,
            paste: None,
            pending_after: None,
            save_dialog: None,
            guard_request: None,
            last_drag: Duration::ZERO,
            field_generation: 0,
            field_paste: None,
            field_submit: None,
            field_error: None,
        }
    }
    /// Preserve valid focus and invalidate capture after geometry or overlay changes.
    pub fn sync_layout(&mut self, layout: &LayoutSnapshot) {
        if layout.next(None, false).is_none() {
            self.capture = None;
            return;
        }
        if let Some(Modal::Menu(index)) = self.modal
            && layout.contains(Control::MenuItem(index))
        {
            self.focus = Some(Control::MenuItem(index));
        }
        if !self.focus.is_some_and(|focus| layout.contains(focus)) {
            self.focus = if layout.contains(Control::Content) {
                Some(Control::Content)
            } else {
                layout.next(None, false)
            };
        }
        if self
            .capture
            .is_some_and(|capture| capture.generation != layout.generation)
        {
            self.capture = None;
        }
    }
    /// Record display-safe bounded status and schedule a redraw.
    pub fn notify(&mut self, message: StatusMessage) {
        if self.status != message {
            self.status = message;
            self.dirty = true;
        }
    }
    /// Install the latest prepared viewport; an Arc shares drawing and pointer geometry.
    pub fn update(&mut self, screen: Screen, pending: bool) {
        if self.screen.document != screen.document
            || (!pending
                && self.capture.is_some_and(|capture| {
                    capture.control == Control::Content
                        && screen.pointer_press != Some(capture.press)
                }))
        {
            self.capture = None;
        }
        if self
            .screen
            .view
            .as_ref()
            .zip(screen.view.as_ref())
            .is_some_and(|(old, new)| !Arc::ptr_eq(old, new))
        {
            self.dirty = true;
        }
        self.notify(StatusMessage::new(Severity::Info, &screen.message));
        self.screen = screen;
        self.pending = pending;
        self.dirty = true;
    }
    /// Consume asynchronous guard/save events without allowing a stale save to close work.
    pub fn event(&mut self, event: Event) -> Option<Intent> {
        if matches!(&event, Event::Guard(sequence,..)|Event::Proceed(sequence,..)|Event::Uncertain(sequence,..) if self.guard_request!=Some(*sequence))
        {
            return None;
        }
        match event {
            Event::Failure(message) => {
                self.field_paste = None;
                self.field_submit = None;
                self.pending_after = None;
                self.guard_request = None;
                self.save_dialog = None;
                self.help = false;
                self.modal = Some(Modal::Details(message));
                self.help_scroll = 0;
                self.capture = None;
                self.dirty = true;
            }
            Event::Guard(_, after, true) => {
                self.help = false;
                self.modal = Some(Modal::Confirm(after, None));
                self.focus = Some(Control::ConfirmCancel);
                self.capture = None;
                self.dirty = true;
            }
            Event::Guard(_, after, false) | Event::Proceed(_, after) => return self.proceed(after),
            Event::Uncertain(_, after, job) => {
                self.capture = None;
                self.modal = Some(Modal::Confirm(after, Some(job)));
                self.focus = Some(Control::ConfirmCancel);
                self.dirty = true;
            }
            Event::NeedSaveAs(sequence) => {
                if self.save_dialog == Some(sequence) {
                    self.path(PathKind::Save);
                }
            }
            Event::Saved(result, message) => {
                if result == SaveResult::Saved {
                    if let Some(after) = self.pending_after.take() {
                        return Some(Intent::Editor(Action::Guard(after)));
                    }
                } else if result != SaveResult::Cancelled {
                    if result == SaveResult::Failed {
                        // A failure can arrive just before the worker takes a queued
                        // Discard. Revoke that pending reply before it can hide the error.
                        self.guard_request = None;
                    }
                    self.capture = None;
                    self.help = false;
                    self.modal = Some(Modal::SaveError(message));
                    self.focus = Some(Control::PathCancel);
                    self.help_scroll = 0;
                    self.dirty = true;
                }
            }
            Event::FieldPaste(generation, result) => {
                let pending = self.field_paste.take()?;
                if pending.generation != generation {
                    self.field_paste = Some(pending);
                    return None;
                }
                if !matches!(self.modal, Some(Modal::Path(..))) {
                    return None;
                }
                let mut accepted = match result {
                    Ok(text) => self.apply_field_edit(FieldEdit::Text(text)),
                    Err(error) => {
                        self.field_message(&error);
                        false
                    }
                };
                let mut error = self.field_error.clone();
                for edit in pending.edits {
                    if !self.apply_field_edit(edit) {
                        accepted = false;
                        error = self.field_error.clone();
                    }
                }
                if error.is_some() {
                    self.field_error = error;
                }
                if accepted && pending.submit {
                    self.field_submit = Some(generation);
                }
                self.dirty = true;
            }
        }
        None
    }
    /// Register the exact accepted save intention before asynchronous dialog delivery.
    pub fn expect_save_dialog(&mut self, sequence: u64) {
        self.save_dialog = Some(sequence);
    }
    /// Only the latest admitted close/replacement decision may act on the UI.
    pub fn expect_guard(&mut self, sequence: u64) {
        self.guard_request = Some(sequence);
    }
    /// Begin a guarded next action once the document owner has acknowledged safety.
    fn proceed(&mut self, after: After) -> Option<Intent> {
        self.guard_request = None;
        self.modal = None;
        self.help = false;
        self.focus = Some(Control::Content);
        self.dirty = true;
        match after {
            After::Quit => Some(Intent::Quit),
            After::New => Some(Intent::Editor(Action::New)),
            After::Open => {
                self.path(PathKind::Open);
                None
            }
        }
    }
    /// Open a path field with no shell interpretation.
    fn path(&mut self, kind: PathKind) {
        self.field_generation = self.field_generation.wrapping_add(1);
        self.field_paste = None;
        self.field_submit = None;
        self.field_error = None;
        self.guard_request = None;
        self.save_dialog = None;
        self.help = false;
        self.modal = Some(Modal::Path(kind, String::new()));
        self.focus = Some(Control::PathField);
        self.capture = None;
        self.dirty = true;
    }
    /// Route one input to the owning modal, control, or text surface.
    pub fn input(&mut self, input: Input, layout: &LayoutSnapshot) -> Option<Intent> {
        match input {
            Input::Command(command) => self.command(command, layout),
            Input::Text(text) => {
                if matches!(self.modal, Some(Modal::Path(..))) {
                    self.edit_field(FieldEdit::Text(text));
                } else if self.modal.is_none()
                    && !self.help
                    && self.focus == Some(Control::Content)
                    && !self.screen.loading
                    && self.guard_request.is_none()
                {
                    return Some(Intent::Editor(Action::Insert(text)));
                }
                None
            }
            Input::Pointer(mouse) => self.pointer(mouse, layout),
            Input::Paste(paste) => {
                self.dirty = true;
                if self
                    .paste
                    .take()
                    .is_some_and(|cancel| cancel.is_cancelled())
                {
                    self.message("Paste cancelled");
                    return None;
                }
                if matches!(self.modal, Some(Modal::Path(..)))
                    && self.focus == Some(Control::PathField)
                {
                    if self.field_paste.is_some() {
                        self.field_message("Busy: field paste rejected; retry when ready");
                        return None;
                    }
                    self.field_paste = Some(FieldPaste {
                        generation: self.field_generation,
                        edits: Vec::new(),
                        bytes: 0,
                        submit: false,
                    });
                    Some(Intent::Editor(Action::FieldPaste(
                        paste,
                        self.field_generation,
                    )))
                } else if self.modal.is_none()
                    && !self.help
                    && self.focus == Some(Control::Content)
                    && !self.screen.loading
                    && self.guard_request.is_none()
                {
                    Some(Intent::Editor(Action::Paste(paste)))
                } else {
                    self.message("Paste requires editor text focus");
                    None
                }
            }
            Input::PasteStarted(cancel) => {
                self.paste = Some(cancel);
                self.dirty = true;
                None
            }
            Input::Notice(message) => {
                self.paste = None;
                self.message(&message);
                self.dirty = true;
                None
            }
            Input::PasteRejected(message) => {
                self.paste = None;
                // A quick worker update must not erase rejection before it is drawn.
                self.event(Event::Failure(message))
            }
            Input::KeyboardSupported => None,
            Input::FocusLost => {
                self.capture = None;
                None
            }
            Input::FocusGained => {
                self.dirty = true;
                None
            }
            Input::Resize(..) => {
                self.capture = None;
                self.dirty = true;
                None
            }
        }
    }
    /// Handle named commands once for keyboard/menu/buttons, preserving modal ownership.
    fn command(&mut self, mut command: Command, layout: &LayoutSnapshot) -> Option<Intent> {
        if command == Command::Cancel {
            self.field_paste = None;
            self.field_submit = None;
            self.pending_after = None;
            self.save_dialog = None;
            self.guard_request = None;
            if self.cancel_receiving_paste() {
                return None;
            }
            if self.modal.take().is_some() || self.help {
                self.help = false;
                self.focus = self.opener.take().or(Some(Control::Content));
                self.pending_after = None;
                self.capture = None;
                self.dirty = true;
                return None;
            }
            if self.screen.loading || self.pending {
                return Some(Intent::CancelWork);
            }
            self.capture = None;
            return Some(Intent::Editor(Action::ClearSelection));
        }
        if self.help && matches!(command, Command::Move(..)) {
            self.help_scroll =
                if matches!(command, Command::Move(Movement::Up | Movement::PageUp, _)) {
                    self.help_scroll.saturating_sub(1)
                } else {
                    self.help_scroll.saturating_add(1).min(80)
                };
            self.dirty = true;
            return None;
        }
        if matches!(self.modal, Some(Modal::Path(..))) && command == Command::Backspace {
            self.edit_field(FieldEdit::Backspace);
            return None;
        }
        if let Some(Modal::Path(kind, field)) = &mut self.modal {
            match command {
                Command::Newline | Command::Activate
                    if layout.area.width >= 40 && layout.area.height >= 8 =>
                {
                    if self.focus == Some(Control::PathCancel) {
                        self.field_paste = None;
                        self.field_submit = None;
                        self.modal = None;
                        self.focus = Some(Control::Content);
                        self.pending_after = None;
                        self.dirty = true;
                        return None;
                    }
                    if let Some(pending) = &mut self.field_paste {
                        pending.submit = true;
                        return None;
                    }
                    if field.is_empty() {
                        self.field_message("Enter a path or line number");
                        return None;
                    }
                    let kind = *kind;
                    if kind == PathKind::GoTo {
                        let mut values = field.split(':');
                        let line = values
                            .next()
                            .and_then(|n| n.parse::<u64>().ok())
                            .filter(|n| *n > 0);
                        let column = values
                            .next()
                            .map_or(Some(1), |n| n.parse::<u64>().ok())
                            .filter(|n| *n > 0);
                        if let (Some(line), Some(column), None) = (line, column, values.next()) {
                            self.modal = None;
                            self.focus = Some(Control::Content);
                            self.dirty = true;
                            return Some(Intent::Editor(Action::GoTo {
                                line: line - 1,
                                column: column - 1,
                            }));
                        }
                        self.field_message("Use LINE[:CELL], starting at 1");
                        return None;
                    }
                    let path = PathBuf::from(field.as_str());
                    self.modal = None;
                    self.focus = Some(Control::Content);
                    self.dirty = true;
                    return Some(Intent::Editor(if kind == PathKind::Open {
                        Action::Open(path)
                    } else {
                        Action::Save(Some(path))
                    }));
                }
                Command::Tab | Command::FocusNext | Command::FocusPrevious => {
                    self.focus = layout.next(self.focus, command == Command::FocusPrevious);
                    self.dirty = true;
                }
                _ => {}
            }
            return None;
        }
        if let Some(Modal::Confirm(after, uncertain)) = self.modal {
            if layout.area.width < 40 || layout.area.height < 8 {
                return None;
            }
            if matches!(
                command,
                Command::Tab | Command::FocusNext | Command::FocusPrevious | Command::Move(..)
            ) {
                self.focus = layout.next(
                    self.focus,
                    matches!(
                        command,
                        Command::FocusPrevious | Command::Move(Movement::Left | Movement::Up, _)
                    ),
                );
                self.dirty = true;
                return None;
            }
            if matches!(command, Command::Activate | Command::Newline)
                && layout.area.width >= 40
                && layout.area.height >= 8
            {
                match self.focus {
                    Some(Control::ConfirmSave) => {
                        self.pending_after = Some(after);
                        self.dirty = true;
                        return Some(Intent::Editor(Action::Save(None)));
                    }
                    Some(Control::ConfirmDiscard) => {
                        self.pending_after = None;
                        self.dirty = true;
                        return Some(Intent::Editor(Action::Discard(after, uncertain)));
                    }
                    _ => {
                        self.modal = None;
                        self.pending_after = None;
                        self.guard_request = None;
                        self.save_dialog = None;
                        self.focus = Some(Control::Content);
                        self.dirty = true;
                    }
                }
            }
            return None;
        }
        if let Some(Modal::SaveError(_)) = self.modal {
            if matches!(command, Command::Move(..)) {
                self.help_scroll =
                    if matches!(command, Command::Move(Movement::Up | Movement::PageUp, _)) {
                        self.help_scroll.saturating_sub(1)
                    } else {
                        self.help_scroll.saturating_add(1)
                    };
                self.dirty = true;
                return None;
            }
            if command == Command::SaveAs
                || (matches!(command, Command::Activate | Command::Newline)
                    && self.focus == Some(Control::SaveErrorAs))
            {
                self.path(PathKind::Save);
                return None;
            }
            if matches!(command, Command::Activate | Command::Newline) {
                self.modal = None;
                self.pending_after = None;
                self.guard_request = None;
                self.save_dialog = None;
                self.focus = Some(Control::Content);
                self.dirty = true;
                return None;
            }
            if matches!(
                command,
                Command::FocusNext | Command::FocusPrevious | Command::Tab
            ) {
                self.focus = layout.next(self.focus, command == Command::FocusPrevious);
                self.dirty = true;
            }
            return None;
        }
        if let Some(Modal::Details(_)) = self.modal {
            if matches!(command, Command::Move(..)) {
                self.help_scroll =
                    if matches!(command, Command::Move(Movement::Up | Movement::PageUp, _)) {
                        self.help_scroll.saturating_sub(1)
                    } else {
                        self.help_scroll.saturating_add(1)
                    };
                self.dirty = true;
            }
            if matches!(command, Command::Activate | Command::Newline) {
                self.modal = None;
                self.dirty = true;
            }
            return None;
        }
        if let Some(Modal::Menu(index)) = &mut self.modal {
            match command {
                Command::Move(Movement::Up, _) => {
                    *index = index.saturating_sub(1);
                    self.dirty = true;
                    return None;
                }
                Command::Move(Movement::Down, _) => {
                    *index = (*index + 1).min(MENU.len() - 1);
                    self.dirty = true;
                    return None;
                }
                Command::Newline | Command::Activate => {
                    command = MENU[*index].0;
                }
                Command::FocusNext | Command::FocusPrevious | Command::Tab => {
                    *index = (*index
                        + if command == Command::FocusPrevious {
                            MENU.len() - 1
                        } else {
                            1
                        })
                        % MENU.len();
                    self.dirty = true;
                    return None;
                }
                _ => {}
            }
            self.modal = None;
            self.focus = Some(Control::Content);
            self.dirty = true;
        }
        if self.guard_request.is_some()
            && self.modal.is_none()
            && matches!(command, Command::Save | Command::SaveAs | Command::GoTo)
        {
            return None;
        }
        match command {
            Command::Quit => {
                self.pending_after = None;
                self.guard_request = None;
                self.save_dialog = None;
                Some(Intent::Editor(Action::Guard(After::Quit)))
            }
            Command::New | Command::Open => {
                Some(Intent::Editor(Action::Guard(if command == Command::New {
                    After::New
                } else {
                    After::Open
                })))
            }
            Command::Save => Some(Intent::Editor(Action::Save(None))),
            Command::GoTo => {
                self.path(PathKind::GoTo);
                None
            }
            Command::SaveAs => {
                self.path(PathKind::Save);
                None
            }
            Command::Suspend => Some(Intent::Suspend),
            Command::MouseCapture => {
                self.mouse_capture = !self.mouse_capture;
                self.capture = None;
                self.message(if self.mouse_capture {
                    "Mouse capture enabled"
                } else {
                    "Mouse capture paused; F2 to re-enable"
                });
                Some(Intent::MouseCapture(self.mouse_capture))
            }
            Command::Menu => {
                self.guard_request = None;
                self.pending_after = None;
                self.save_dialog = None;
                self.help = false;
                self.opener = self.focus;
                self.modal = Some(Modal::Menu(0));
                self.capture = None;
                self.dirty = true;
                None
            }
            Command::Help if !self.help => {
                self.guard_request = None;
                self.pending_after = None;
                self.save_dialog = None;
                self.opener = self.focus;
                self.help = true;
                self.modal = None;
                self.focus = Some(Control::HelpQuit);
                self.capture = None;
                self.dirty = true;
                None
            }
            Command::FocusNext | Command::FocusPrevious => {
                self.focus = layout.next(self.focus, command == Command::FocusPrevious);
                self.dirty = true;
                None
            }
            Command::Tab if self.help || self.focus != Some(Control::Content) => {
                self.focus = layout.next(self.focus, false);
                self.dirty = true;
                None
            }
            Command::Activate | Command::Newline
                if self.help || self.focus != Some(Control::Content) =>
            {
                self.focus
                    .and_then(Control::command)
                    .and_then(|command| self.command(command, layout))
            }
            Command::Help | Command::Cancel => None,
            _ if self.help
                || self.focus != Some(Control::Content)
                || self.screen.loading
                || self.guard_request.is_some() =>
            {
                None
            }
            _ => Some(Intent::Editor(Action::Command(command))),
        }
    }
    /// Press captures the text/control surface; dragging cannot activate other controls.
    fn pointer(&mut self, mouse: MouseEvent, layout: &LayoutSnapshot) -> Option<Intent> {
        if !self.mouse_capture {
            return None;
        }
        if matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) && (self.help || self.modal.is_some())
        {
            let up = mouse.kind == MouseEventKind::ScrollUp;
            if let Some(Modal::Menu(index)) = &mut self.modal {
                *index = if up {
                    index.saturating_sub(1)
                } else {
                    (*index + 1).min(MENU.len() - 1)
                };
            } else if self.help
                || matches!(self.modal, Some(Modal::Details(_) | Modal::SaveError(_)))
            {
                self.help_scroll = if up {
                    self.help_scroll.saturating_sub(3)
                } else {
                    self.help_scroll.saturating_add(3)
                };
            }
            self.dirty = true;
            return None;
        }
        let hit = layout.hit(mouse.column, mouse.row);
        if hit == Some(Control::Content) && (self.screen.loading || self.guard_request.is_some()) {
            return None;
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.pointer_press = self.pointer_press.wrapping_add(1);
                self.capture = hit.map(|control| Capture {
                    control,
                    generation: layout.generation,
                    press: self.pointer_press,
                    x: mouse.column,
                    y: mouse.row,
                });
                if let Some(control) = hit {
                    self.focus = Some(control);
                    self.dirty = true;
                }
                if hit == Some(Control::Content) {
                    let intent = self.hit(
                        mouse.column,
                        mouse.row,
                        mouse.modifiers.contains(KeyModifiers::SHIFT),
                        layout,
                        PointerGesture::Press(self.pointer_press),
                    );
                    if intent.is_none() {
                        self.capture = None;
                    }
                    return intent;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(capture) = &mut self.capture
                    && capture.control == Control::Content
                    && capture.generation == layout.generation
                {
                    capture.x = mouse.column;
                    capture.y = mouse.row;
                    let press = capture.press;
                    return self.hit(
                        mouse.column,
                        mouse.row,
                        true,
                        layout,
                        PointerGesture::Drag(press),
                    );
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(capture) = self.capture.take()
                    && capture.generation == layout.generation
                {
                    if capture.control == Control::Content {
                        if capture.x != mouse.column || capture.y != mouse.row {
                            return self.hit(
                                mouse.column,
                                mouse.row,
                                true,
                                layout,
                                PointerGesture::Drag(capture.press),
                            );
                        }
                    } else if hit == Some(capture.control)
                        && let Some(command) = capture.control.command()
                    {
                        // Keyboard focus may have changed while the button was held.
                        // Activate the clicked control, not another focused button.
                        self.focus = Some(capture.control);
                        return self.command(command, layout);
                    }
                }
            }
            MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight
                if hit == Some(Control::Content) =>
            {
                let (v, h) = match mouse.kind {
                    MouseEventKind::ScrollUp => (-3, 0),
                    MouseEventKind::ScrollDown => (3, 0),
                    MouseEventKind::ScrollLeft => (0, -4),
                    _ => (0, 4),
                };
                return Some(Intent::Editor(Action::Scroll(v, h)));
            }
            _ => {}
        }
        None
    }
    /// Hit only the exact displayed revision/geometry; stale input is visibly rejected.
    fn hit(
        &mut self,
        x: u16,
        y: u16,
        extend: bool,
        layout: &LayoutSnapshot,
        gesture: PointerGesture,
    ) -> Option<Intent> {
        if self.screen.loading {
            return None;
        }
        let view = self.screen.view.as_ref()?;
        if view.generation != layout.generation
            || view.width != layout.area.width
            || view.height != layout.area.height.saturating_sub(2)
        {
            self.message("View pending; retry selection when ready");
            return None;
        }
        let x = x
            .saturating_sub(layout.area.x)
            .min(layout.area.width.saturating_sub(1));
        let y = y
            .saturating_sub(layout.area.y + 1)
            .min(layout.area.height.saturating_sub(3));
        let caret = view.rows.get(y as usize).map_or_else(
            || view.rows.last().map_or(0, |row| row.end),
            |row| row.hit(view.left + x as u64),
        );
        Some(Intent::Editor(Action::Hit {
            caret,
            extend,
            generation: layout.generation,
            revision: view.revision,
            document: self.screen.document?,
            top: view.top,
            left: view.left,
            gesture,
        }))
    }
    /// Timer-driven drag autoscroll does not depend on another motion event.
    pub fn tick(&mut self, now: Duration, layout: &LayoutSnapshot) -> Option<Intent> {
        if self.field_submit == Some(self.field_generation)
            && matches!(self.modal, Some(Modal::Path(..)))
            && layout.area.width >= 40
            && layout.area.height >= 8
        {
            self.field_submit = None;
            self.focus = Some(Control::PathField);
            return self.command(Command::Newline, layout);
        }
        if now.saturating_sub(self.last_drag) < Duration::from_millis(50) {
            return None;
        }
        self.last_drag = now;
        let capture = self.capture?;
        if capture.control != Control::Content
            || capture.generation != layout.generation
            || self.pending
        {
            return None;
        }
        let vertical = if capture.y <= layout.area.y {
            -1
        } else if capture.y >= layout.area.bottom() - 1 {
            1
        } else {
            0
        };
        let horizontal = if capture.x < layout.area.x {
            -4
        } else if capture.x >= layout.area.right() {
            4
        } else {
            0
        };
        if vertical != 0 || horizontal != 0 {
            return Some(Intent::Editor(Action::Scroll(vertical, horizontal)));
        }
        None
    }
    /// Reapply the held selection pointer after an autoscrolled view has reached the screen.
    pub fn drag_hit(&mut self, layout: &LayoutSnapshot) -> Option<Intent> {
        let capture = self.capture?;
        if capture.control == Control::Content {
            self.hit(
                capture.x,
                capture.y,
                true,
                layout,
                PointerGesture::Drag(capture.press),
            )
        } else {
            None
        }
    }
    /// Cancel a live paste without interpreting bytes inside its terminal frame.
    pub fn cancel_receiving_paste(&mut self) -> bool {
        if let Some(paste) = &self.paste {
            paste.cancel();
            self.dirty = true;
            true
        } else {
            false
        }
    }
    /// Live ingress feedback takes precedence over unrelated worker status updates.
    pub fn receiving_paste_status(&self) -> Option<String> {
        self.paste.as_ref().map(|paste| {
            if paste.is_cancelled() {
                "Cancelling paste".into()
            } else {
                format!("Receiving paste | kill -USR1 {}", std::process::id())
            }
        })
    }
    /// Short feedback escapes through StatusMessage's safe bounded representation.
    fn message(&mut self, text: &str) {
        self.notify(StatusMessage::new(Severity::Info, text));
    }
    /// Keep field failures visible even when a background screen updates the status row.
    fn field_message(&mut self, text: &str) {
        self.field_error = Some(text.into());
        self.message(text);
    }
    /// Admit bounded edits behind the asynchronous paste, preserving their arrival order.
    fn edit_field(&mut self, edit: FieldEdit) {
        if let Some(pending) = &mut self.field_paste {
            let bytes = match &edit {
                FieldEdit::Text(text) => text.len(),
                FieldEdit::Backspace => 0,
            };
            if pending.submit || pending.edits.len() >= 256 || pending.bytes + bytes > 4096 {
                self.field_message("Busy: field input rejected; retry when ready");
            } else {
                pending.bytes += bytes;
                pending.edits.push(edit);
            }
        } else {
            self.apply_field_edit(edit);
        }
    }
    /// Apply field data without interpreting any pasted control bytes as commands.
    fn apply_field_edit(&mut self, edit: FieldEdit) -> bool {
        let Some(Modal::Path(_, field)) = &mut self.modal else {
            return false;
        };
        match edit {
            FieldEdit::Text(text) => {
                if field.len() + text.len() > 4096 || text.contains('\0') {
                    self.field_message("Invalid or oversized field paste/input");
                    return false;
                }
                field.push_str(&text);
            }
            FieldEdit::Backspace => {
                field.pop();
            }
        }
        self.field_error = None;
        self.dirty = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    /// Field paste is asynchronous, ordered with typing/submit, revocable, and cannot leak to text.
    #[test]
    fn field_paste_ordering_and_cancellation() {
        let mut app = AppState::new();
        app.path(PathKind::Save);
        let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 40, 8), &app, 1);
        let mut spool = crate::spool::Spool::new().unwrap();
        spool.append(b"destination").unwrap();
        let paste = Arc::new(spool);
        assert!(matches!(
            app.input(Input::Paste(paste.clone()), &layout),
            Some(Intent::Editor(Action::FieldPaste(_, _)))
        ));
        let generation = app.field_generation;
        app.input(Input::Text("X".into()), &layout);
        app.input(Input::Command(Command::Backspace), &layout);
        app.input(Input::Text(".txt".into()), &layout);
        assert!(
            app.input(Input::Command(Command::Newline), &layout)
                .is_none()
        );
        app.event(Event::FieldPaste(generation, Ok("destination".into())));
        let tiny = LayoutSnapshot::for_app(Rect::new(0, 0, 20, 4), &app, 2);
        assert!(app.tick(Duration::ZERO, &tiny).is_none());
        assert!(
            matches!(app.tick(Duration::ZERO, &layout), Some(Intent::Editor(Action::Save(Some(path)))) if path == std::path::Path::new("destination.txt"))
        );
        app.path(PathKind::Open);
        app.input(Input::Paste(paste), &layout);
        let generation = app.field_generation;
        app.input(Input::Command(Command::Cancel), &layout);
        app.path(PathKind::Save);
        app.event(Event::FieldPaste(generation, Ok("obsolete".into())));
        assert!(matches!(&app.modal, Some(Modal::Path(PathKind::Save, field)) if field.is_empty()));
    }
    /// Dialog ownership protects work and routed text never leaks into the document.
    #[test]
    fn confirm_defaults_cancel_and_path_is_modal() {
        let mut app = AppState::new();
        app.screen.loading = false;
        app.expect_guard(1);
        app.event(Event::Guard(1, After::Quit, true));
        assert_eq!(app.focus, Some(Control::ConfirmCancel));
        let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 40, 8), &app, 1);
        assert!(app.input(Input::Text("ignored".into()), &layout).is_none());
        assert!(app.input(Input::Command(Command::Quit), &layout).is_none());
        assert!(
            app.input(Input::Command(Command::Newline), &layout)
                .is_none()
        );
        assert!(app.modal.is_none());
        app.path(PathKind::Save);
        let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 40, 8), &app, 2);
        app.input(Input::Text("destination".into()), &layout);
        assert!(app.input(Input::Command(Command::Quit), &layout).is_none());
        assert!(
            matches!(app.input(Input::Command(Command::Newline),&layout),Some(Intent::Editor(Action::Save(Some(path)))) if path==std::path::Path::new("destination"))
        );
    }
    /// A focus change during a mouse press cannot turn Save into Discard; new warnings cancel capture.
    #[test]
    fn clicked_confirmation_is_independent_of_keyboard_focus() {
        let mut app = AppState::new();
        app.screen.loading = false;
        app.expect_guard(1);
        app.event(Event::Guard(1, After::Quit, true));
        let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 80, 24), &app, 1);
        let (x, y) = (0..24)
            .flat_map(|y| (0..80).map(move |x| (x, y)))
            .find(|(x, y)| layout.hit(*x, *y) == Some(Control::ConfirmSave))
            .unwrap();
        let mouse = |kind| {
            Input::Pointer(MouseEvent {
                kind,
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            })
        };
        app.input(mouse(MouseEventKind::Down(MouseButton::Left)), &layout);
        app.input(Input::Command(Command::Tab), &layout);
        assert_eq!(app.focus, Some(Control::ConfirmDiscard));
        assert!(matches!(
            app.input(mouse(MouseEventKind::Up(MouseButton::Left)), &layout),
            Some(Intent::Editor(Action::Save(None)))
        ));
        app.input(mouse(MouseEventKind::Down(MouseButton::Left)), &layout);
        app.expect_guard(2);
        app.event(Event::Uncertain(2, After::Quit, JobId(2)));
        assert!(app.capture.is_none());
        assert!(
            app.input(mouse(MouseEventKind::Up(MouseButton::Left)), &layout)
                .is_none()
        );
        assert!(matches!(
            app.modal,
            Some(Modal::Confirm(After::Quit, Some(JobId(2))))
        ));
    }
    /// Ingress rejection remains visible if an earlier paste finishes before the next frame.
    #[test]
    fn paste_rejection_survives_a_worker_update() {
        let mut app = AppState::new();
        app.screen.loading = false;
        let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 80, 24), &app, 1);
        app.input(Input::PasteRejected("Busy: paste rejected".into()), &layout);
        let mut screen = app.screen.clone();
        screen.message = "Ready".into();
        app.update(screen, false);
        assert!(
            matches!(&app.modal, Some(Modal::Details(message)) if message.contains("paste rejected"))
        );
    }
    /// Cancel revokes asynchronous close/path replies, and a waiting Discard keeps editing modal.
    #[test]
    fn cancellation_revokes_late_dialog_and_quit_results() {
        let mut app = AppState::new();
        app.screen.loading = false;
        app.expect_guard(10);
        let checking = LayoutSnapshot::for_app(Rect::new(0, 0, 80, 24), &app, 0);
        assert!(
            app.input(Input::Text("ignored while checking".into()), &checking)
                .is_none()
        );
        app.event(Event::Guard(10, After::Quit, true));
        let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 80, 24), &app, 1);
        app.focus = Some(Control::ConfirmSave);
        assert!(matches!(
            app.input(Input::Command(Command::Newline), &layout),
            Some(Intent::Editor(Action::Save(None)))
        ));
        app.expect_save_dialog(11);
        app.input(Input::Command(Command::Cancel), &layout);
        assert!(app.modal.is_none());
        app.event(Event::NeedSaveAs(11));
        assert!(app.modal.is_none());
        assert!(app.event(Event::Guard(10, After::Quit, false)).is_none());
        assert!(
            app.event(Event::Saved(SaveResult::Saved, "Saved".into()))
                .is_none()
        );
        app.expect_guard(20);
        app.event(Event::Guard(20, After::Quit, true));
        app.focus = Some(Control::ConfirmDiscard);
        assert!(matches!(
            app.input(Input::Command(Command::Newline), &layout),
            Some(Intent::Editor(Action::Discard(After::Quit, None)))
        ));
        app.expect_guard(21);
        assert!(app.modal.is_some());
        assert!(app.input(Input::Text("ignored".into()), &layout).is_none());
        app.focus = Some(Control::ConfirmCancel);
        app.input(Input::Command(Command::Newline), &layout);
        assert!(app.modal.is_none());
        assert!(app.event(Event::Proceed(21, After::Quit)).is_none());
    }

    /// Autoscroll is timer-driven and release, overlays, focus loss and resize stop it immediately.
    #[test]
    fn timer_autoscroll_stops_with_capture() {
        for cancel in [
            Input::FocusLost,
            Input::Resize(80, 24),
            Input::Command(Command::Help),
        ] {
            let mut app = AppState::new();
            let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 80, 24), &app, 3);
            app.capture = Some(Capture {
                control: Control::Content,
                generation: 3,
                press: 1,
                x: 20,
                y: 25,
            });
            assert!(matches!(
                app.tick(Duration::from_millis(50), &layout),
                Some(Intent::Editor(Action::Scroll(1, 0)))
            ));
            app.input(cancel, &layout);
            assert!(app.tick(Duration::from_millis(100), &layout).is_none());
        }
    }

    /// Holding the button at the first visible text cell never counts as leaving the viewport.
    #[test]
    fn first_text_column_does_not_autoscroll() {
        let mut app = AppState::new();
        let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 80, 24), &app, 1);
        app.screen.loading = false;
        app.capture = Some(Capture {
            control: Control::Content,
            generation: 1,
            press: 1,
            x: 0,
            y: 1,
        });
        assert!(app.tick(Duration::from_millis(50), &layout).is_none());
    }

    /// Resize/focus loss invalidate captures; controls crossed by a text drag never activate.
    #[test]
    fn capture_and_stale_geometry_are_safe() {
        let mut app = AppState::new();
        let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 80, 24), &app, 1);
        let pointer = |kind| {
            Input::Pointer(MouseEvent {
                kind,
                column: 73,
                row: 0,
                modifiers: KeyModifiers::NONE,
            })
        };
        app.input(pointer(MouseEventKind::Down(MouseButton::Left)), &layout);
        app.input(Input::FocusLost, &layout);
        assert!(
            app.input(pointer(MouseEventKind::Up(MouseButton::Left)), &layout)
                .is_none()
        );
        app.mouse_capture = false;
        assert!(
            app.input(pointer(MouseEventKind::Down(MouseButton::Left)), &layout)
                .is_none()
        );
        assert!(
            app.input(pointer(MouseEventKind::Up(MouseButton::Left)), &layout)
                .is_none()
        );
        assert!(app.capture.is_none());
        app.input(pointer(MouseEventKind::Down(MouseButton::Left)), &layout);
        app.input(Input::Resize(40, 8), &layout);
        assert!(
            app.input(pointer(MouseEventKind::Up(MouseButton::Left)), &layout)
                .is_none()
        );
    }
}
