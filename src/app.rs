//! Pure application reducer; no terminal operations or document I/O occur here.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use crate::{
    command::Command,
    input::Input,
    status::{Severity, StatusMessage},
    ui::{Control, LayoutSnapshot},
};

/// Side effects that the runtime must perform outside the reducer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Intent {
    Quit,
    Suspend,
}

/// Captured pointer target and the exact rendered layout that owns it.
#[derive(Clone, Copy, Debug)]
struct PointerCapture {
    control: Control,
    generation: u64,
}

/// Small shell state, independent of terminal and rendering backends.
pub(crate) struct AppState {
    pub help: bool,
    pub focus: Option<Control>,
    pub status: StatusMessage,
    pub dirty: bool,
    opener: Option<Control>,
    capture: Option<PointerCapture>,
}

impl AppState {
    /// Empty shell with content focus and no pretend document buffer.
    pub(crate) fn new() -> Self {
        Self {
            help: false,
            focus: Some(Control::Content),
            status: StatusMessage::new(Severity::Info, "Ready"),
            dirty: true,
            opener: None,
            capture: None,
        }
    }

    /// Ensure focus exists in a freshly computed layout before drawing or accepting pointer input.
    pub(crate) fn sync_layout(&mut self, layout: &LayoutSnapshot) {
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

    /// Record bounded status feedback and request a redraw only when its value changed.
    pub(crate) fn notify(&mut self, message: StatusMessage) {
        if self.status != message {
            self.status = message;
            self.dirty = true;
        }
    }

    /// Convert application input into state changes and optional runtime intent.
    pub(crate) fn input(&mut self, input: Input, layout: &LayoutSnapshot) -> Option<Intent> {
        match input {
            Input::Command(command) => return self.command(command, layout),
            Input::Pointer(mouse) => return self.pointer(mouse, layout),
            Input::Paste => self.message("No document open"),
            Input::FocusLost => {
                self.capture = None;
            }
            Input::FocusGained => self.dirty = true,
            Input::Resize(..) => {
                self.capture = None;
                self.dirty = true;
            }
        }
        None
    }

    /// Single command handler shared by keyboard bindings and all controls.
    fn command(&mut self, command: Command, layout: &LayoutSnapshot) -> Option<Intent> {
        match command {
            Command::Quit => return Some(Intent::Quit),
            Command::Suspend => return Some(Intent::Suspend),
            Command::Help if !self.help => {
                self.opener = self.focus;
                self.help = true;
                self.focus = Some(Control::HelpQuit);
                self.capture = None;
                self.dirty = true;
            }
            Command::Cancel if self.help => {
                self.help = false;
                self.focus = self.opener.take().or(Some(Control::Content));
                self.capture = None;
                self.dirty = true;
            }
            Command::FocusNext | Command::FocusPrevious => {
                self.focus = layout.next(self.focus, command == Command::FocusPrevious);
                self.dirty = true;
            }
            Command::Activate => {
                if let Some(command) = self.focus.and_then(Control::command) {
                    return self.command(command, layout);
                }
            }
            Command::Save => self.message("No document open"),
            Command::Copy => self.message("No selection"),
            Command::Help | Command::Cancel => {}
        }
        None
    }

    /// Capture on press and activate on release over the same control in the same layout.
    fn pointer(&mut self, mouse: MouseEvent, layout: &LayoutSnapshot) -> Option<Intent> {
        let hit = layout.hit(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.capture = hit.map(|control| PointerCapture {
                    control,
                    generation: layout.generation,
                });
                if let Some(control) = hit {
                    self.focus = Some(control);
                    self.dirty = true;
                    if control == Control::Content {
                        self.message("Mouse press");
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(capture) = self.capture.take()
                    && capture.generation == layout.generation
                    && hit == Some(capture.control)
                {
                    if let Some(command) = capture.control.command() {
                        return self.command(command, layout);
                    }
                    self.message("Mouse release");
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.capture.is_some_and(|capture| {
                    capture.control == Control::Content && capture.generation == layout.generation
                }) {
                    self.message("Mouse drag");
                }
            }
            MouseEventKind::ScrollDown
            | MouseEventKind::ScrollUp
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight
                if hit == Some(Control::Content) =>
            {
                self.message("Mouse wheel")
            }
            _ => {}
        }
        None
    }

    /// Short informational feedback, never raw terminal input.
    fn message(&mut self, text: &str) {
        self.notify(StatusMessage::new(Severity::Info, text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use ratatui::layout::Rect;

    /// Standard-size layout for reducer tests; rebuild when an overlay changes.
    fn layout(app: &AppState, generation: u64) -> LayoutSnapshot {
        LayoutSnapshot::new(Rect::new(0, 0, 80, 24), app.help, generation)
    }

    /// Construct one pointer event without terminal I/O.
    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Input {
        Input::Pointer(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    /// Keyboard and mouse invoke identical help/quit commands; closing restores the opener.
    #[test]
    fn shared_commands_and_focus_restoration() {
        let mut app = AppState::new();
        let base = layout(&app, 1);
        app.input(mouse(MouseEventKind::Down(MouseButton::Left), 63, 0), &base);
        app.input(mouse(MouseEventKind::Up(MouseButton::Left), 63, 0), &base);
        assert!(app.help);
        let overlay = layout(&app, 2);
        app.sync_layout(&overlay);
        assert_eq!(app.focus, Some(Control::HelpQuit));
        assert_eq!(
            app.input(Input::Command(Command::Activate), &overlay),
            Some(Intent::Quit)
        );
        app.input(Input::Command(Command::Cancel), &overlay);
        app.sync_layout(&base);
        assert_eq!(app.focus, Some(Control::Help));
        app.input(Input::Command(Command::Save), &base);
        assert_eq!(app.status.text(), "No document open");
        app.input(Input::Command(Command::Copy), &base);
        assert_eq!(app.status.text(), "No selection");
        app.input(Input::Paste, &base);
        assert_eq!(app.status.text(), "No document open");
    }

    /// Dragging across a button, resizing, or losing focus cannot activate a stale capture.
    #[test]
    fn pointer_capture_is_cancelled_or_kept_within_its_target() {
        for cancel in [Input::FocusLost, Input::Resize(80, 24)] {
            let mut app = AppState::new();
            let base = layout(&app, 1);
            app.input(mouse(MouseEventKind::Down(MouseButton::Left), 72, 0), &base);
            app.input(cancel, &base);
            assert_eq!(
                app.input(mouse(MouseEventKind::Up(MouseButton::Left), 72, 0), &base),
                None
            );
        }
        let mut app = AppState::new();
        let base = layout(&app, 1);
        app.input(mouse(MouseEventKind::Down(MouseButton::Left), 72, 0), &base);
        assert_eq!(
            app.input(
                mouse(MouseEventKind::Up(MouseButton::Left), u16::MAX, u16::MAX),
                &base
            ),
            None
        );
        app.input(Input::Command(Command::Help), &base);
        let overlay = layout(&app, 2);
        app.sync_layout(&overlay);
        app.input(
            mouse(MouseEventKind::Down(MouseButton::Left), 72, 0),
            &overlay,
        );
        assert_eq!(
            app.input(
                mouse(MouseEventKind::Up(MouseButton::Left), 72, 0),
                &overlay
            ),
            None
        );
    }
}
