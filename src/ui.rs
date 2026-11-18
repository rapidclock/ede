//! Rendering, focus order, and pointer targets derive from the same immutable geometry.
use crate::text::Span;
use crate::{
    app::{AppState, MENU, Modal, PathKind, settings_ui},
    command::Command,
    editor::{After, TabId, TabSummary},
    settings::Context,
    syntax::{Choice, Foreground, Status as SyntaxStatus, ViewKey},
    text::safe,
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use std::ops::Range;
use unicode_width::UnicodeWidthStr;
/// Stable visible control; text byte positions live in the prepared document view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Control {
    SettingsButton(settings_ui::Button),
    SettingsField,
    UseDefaults,
    Search(u8),
    Content,
    WelcomeOpen,
    WelcomeNew,
    Menu,
    Help,
    Quit,
    Tabs,
    Tab(TabId),
    TabClose,
    TabList,
    TabListRow(TabId),
    TabListClose,
    TabListSwitch,
    RecoveryList,
    RecoveryRow(usize),
    RecoveryOpen,
    RecoveryRetry,
    RecoveryDiscard,
    RecoveryPrevious,
    RecoveryNext,
    RetryRefresh,
    ToggleFollow,
    RecoveryErrorRetry,
    RecoveryErrorCancel,
    PickerList,
    PickerRow(usize),
    PickerUp,
    PickerOpen,
    PickerPrevious,
    PickerNext,
    HelpQuit,
    HelpSuspend,
    HelpClose,
    MenuItem(usize),
    LanguageRow(usize),
    PathField,
    PathSubmit,
    PathCancel,
    ConfirmSave,
    ConfirmDiscard,
    ConfirmCancel,
    DetailsClose,
    SaveErrorAs,
}
impl Control {
    /// Resolve buttons/menu items to the shared command handler.
    pub fn command(self) -> Option<Command> {
        match self {
            Self::SettingsButton(_) | Self::SettingsField => None,
            Self::UseDefaults => Some(Command::Cancel),
            Self::Search(0 | 1) => None,
            Self::Search(_) => Some(Command::Activate),
            Self::Content
            | Self::PathField
            | Self::Tab(_)
            | Self::TabListRow(_)
            | Self::PickerRow(_) => None,
            Self::RecoveryRow(_) | Self::RecoveryList => None,
            Self::RetryRefresh => Some(Command::RetryRefresh),
            Self::WelcomeOpen => Some(Command::Open),
            Self::WelcomeNew => Some(Command::New),
            Self::ToggleFollow => Some(Command::ToggleFollow),
            Self::Menu => Some(Command::Menu),
            Self::SaveErrorAs => Some(Command::SaveAs),
            Self::Help => Some(Command::Help),
            Self::Tabs => Some(Command::Tabs),
            Self::TabClose => Some(Command::CloseTab),
            Self::Quit | Self::HelpQuit => Some(Command::Quit),
            Self::HelpSuspend => Some(Command::Suspend),
            Self::HelpClose | Self::PathCancel | Self::DetailsClose => Some(Command::Cancel),
            Self::MenuItem(_) => Some(Command::Activate),
            _ => Some(Command::Activate),
        }
    }
    /// Compact display labels fit the minimum 40×8 geometry.
    fn label(self) -> &'static str {
        match self {
            Self::SettingsButton(button) => button.label(),
            Self::SettingsField => "",
            Self::UseDefaults => "Use defaults for this session",
            Self::WelcomeOpen => "Open File",
            Self::WelcomeNew => "New Scratch",
            Self::Search(_) => "",
            Self::Content
            | Self::PathField
            | Self::Tab(_)
            | Self::TabListRow(_)
            | Self::LanguageRow(_)
            | Self::PickerList
            | Self::PickerRow(_)
            | Self::TabList => "",
            Self::RecoveryList | Self::RecoveryRow(_) => "",
            Self::RecoveryOpen => "Open",
            Self::RecoveryRetry => "Retry",
            Self::RecoveryDiscard => "Discard",
            Self::RecoveryPrevious => "Prev",
            Self::RecoveryNext => "Next",
            Self::RetryRefresh => "Retry refresh",
            Self::ToggleFollow => "Pause View",
            Self::RecoveryErrorRetry => "Retry",
            Self::RecoveryErrorCancel => "Cancel quit",
            Self::Tabs => "Tabs",
            Self::TabClose => "×",
            Self::TabListClose => "Close",
            Self::TabListSwitch => "Switch",
            Self::PickerUp => "Up",
            Self::PickerOpen => "Open",
            Self::PickerPrevious => "Prev",
            Self::PickerNext => "Next",
            Self::Menu => "Menu",
            Self::Help => "Help",
            Self::Quit | Self::HelpQuit => "Quit",
            Self::HelpSuspend => "Suspend",
            Self::HelpClose | Self::DetailsClose => "Close",
            Self::MenuItem(index) => MENU[index].label(),
            Self::PathSubmit => "OK",
            Self::PathCancel | Self::ConfirmCancel => "Cancel",
            Self::ConfirmSave => "Save",
            Self::SaveErrorAs => "Save As",
            Self::ConfirmDiscard => "Discard",
        }
    }
}
/// One visible target, clipped to the frame before it can receive input.
#[derive(Clone, Copy, Debug)]
struct Target {
    control: Control,
    area: Rect,
}
/// Inputs that change geometry, row identity, or the document under a pointer.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Signature {
    help: bool,
    modal: u8,
    detail: u64,
    active: Option<TabId>,
    tabs: Vec<TabId>,
    welcome: bool,
    picker_revision: u64,
    picker_request: u64,
    switching: Option<u64>,
    search: Option<(bool, usize)>,
    stream: bool,
    stream_confirmation: bool,
    settings_epoch: u64,
    settings_page: u64,
    settings_starting: bool,
    settings_save_state: Option<crate::settings::SaveState>,
}
/// Immutable frame geometry; hidden surfaces have no hit targets.
#[derive(Clone, Debug)]
pub(crate) struct LayoutSnapshot {
    pub area: Rect,
    pub generation: u64,
    pub overlay: Option<Rect>,
    pub signature: Signature,
    targets: Vec<Target>,
    list_rows: u16,
}
impl LayoutSnapshot {
    /// Actual text viewport leaves room for the non-modal search controls.
    pub(crate) fn content_height(&self) -> u16 {
        self.targets
            .iter()
            .find(|t| t.control == Control::Content)
            .map_or(self.area.height.saturating_sub(2), |t| t.area.height)
    }
    /// Base geometry retained for shell/scheduler tests.
    pub fn new(area: Rect, help: bool, generation: u64) -> Self {
        let mut result = Self {
            area,
            generation,
            overlay: None,
            signature: Signature {
                help,
                ..Signature::default()
            },
            targets: Vec::new(),
            list_rows: 0,
        };
        if area.width < 40 || area.height < 8 {
            return result;
        }
        if help {
            let overlay = result.overlay_rect(72, 20);
            result.overlay = Some(overlay);
            let y = overlay.bottom() - 2;
            result.add(Control::HelpQuit, Rect::new(overlay.x + 1, y, 7, 1));
            result.add(Control::HelpSuspend, Rect::new(overlay.x + 9, y, 11, 1));
            result.add(Control::HelpClose, Rect::new(overlay.x + 22, y, 9, 1));
        } else {
            result.add(Control::Menu, Rect::new(area.right() - 25, area.y, 8, 1));
            result.add(Control::Help, Rect::new(area.right() - 17, area.y, 8, 1));
            result.add(Control::Quit, Rect::new(area.right() - 8, area.y, 8, 1));
            result.add(
                Control::Content,
                Rect::new(area.x, area.y + 1, area.width, area.height - 2),
            );
        }
        result
    }
    /// Stable key for changes that invalidate pointer capture or hit geometry.
    pub fn signature(app: &AppState) -> Signature {
        let (modal, detail) = match &app.modal {
            Some(Modal::ReplaceAll(_)) => (11, 0),
            None => (0, 0),
            Some(Modal::Menu(index)) => (1, *index as u64),
            Some(Modal::Language { row, .. }) => (12, *row as u64),
            Some(Modal::SyntaxTheme { row, .. }) => (14, *row as u64),
            Some(Modal::Path(..)) => (2, 0),
            Some(Modal::Confirm(after, _)) => (3, after.target().map_or(0, |tab| tab.0)),
            Some(Modal::Details(_)) => (4, 0),
            Some(Modal::TabDetails) => (13, u64::from(app.screen.path.is_some())),
            Some(Modal::SaveError(_)) => (5, 0),
            Some(Modal::PersistenceError(_)) => (10, 0),
            Some(Modal::Open(dialog)) => (6, dialog.highlight as u64),
            Some(Modal::Tabs(tab)) => (7, tab.map_or(0, |tab| tab.0)),
            Some(Modal::Recovery(dialog)) => (8, dialog.revision),
            Some(Modal::DiscardRecovery(dialog, _)) => (9, dialog.revision),
            Some(Modal::Settings) => (15, 0),
        };
        let (picker_revision, picker_request) = match &app.modal {
            Some(Modal::Open(dialog)) => (dialog.revision, dialog.generation),
            Some(Modal::Recovery(dialog) | Modal::DiscardRecovery(dialog, _)) => {
                (dialog.revision, dialog.generation)
            }
            _ => (0, 0),
        };
        Signature {
            help: app.help,
            modal,
            detail,
            active: app.screen.tab,
            tabs: app.screen.tabs.iter().map(|tab| tab.id).collect(),
            welcome: app.screen.tab.is_none() && !app.screen.loading,
            picker_revision,
            picker_request,
            switching: app.switching,
            search: app
                .search
                .open
                .then_some((app.search.replace, app.search.row)),
            stream: app.screen.stream.is_some(),
            stream_confirmation: app.stream_confirmation.is_some(),
            settings_epoch: app.settings_generation,
            settings_page: app.settings_ui.as_ref().map_or(0, |draft| draft.generation),
            settings_starting: app.settings.starting,
            settings_save_state: app.settings_save_state,
        }
    }
    /// Modal targets replace all underlying controls, including at tiny sizes.
    pub fn for_app(area: Rect, app: &AppState, generation: u64) -> Self {
        let mut result = Self::new(area, app.help, generation);
        result.signature = Self::signature(app);
        if app.settings.starting {
            result.targets.clear();
            result.overlay = None;
            if area.width >= 40 && area.height >= 8 {
                result.add(
                    Control::UseDefaults,
                    Rect::new(area.x, area.bottom() - 1, area.width.min(34), 1),
                );
            }
            return result;
        }
        if area.width < 40 || area.height < 8 {
            return result;
        }
        if app.modal.is_none() {
            if !app.help {
                result.add_tab_strip(app);
                if result.signature.welcome {
                    result
                        .targets
                        .retain(|target| target.control != Control::Content);
                    for (offset, control) in [Control::WelcomeOpen, Control::WelcomeNew]
                        .into_iter()
                        .enumerate()
                    {
                        result.add(
                            control,
                            Rect::new(area.x + 2, area.y + 2 + offset as u16, area.width - 4, 1),
                        );
                    }
                }
                if app.screen.stream.is_some() && area.width >= 64 {
                    result.add(
                        Control::ToggleFollow,
                        Rect::new(area.right() - 16, area.bottom() - 1, 16, 1),
                    );
                }
                if app.search.open {
                    if let Some(content) = result
                        .targets
                        .iter_mut()
                        .find(|t| t.control == Control::Content)
                    {
                        content.area.height = content.area.height.saturating_sub(3);
                    }
                    for (row, index) in app
                        .search
                        .controls()
                        .into_iter()
                        .skip(app.search.row)
                        .take(2)
                        .enumerate()
                    {
                        result.add(
                            Control::Search(index),
                            Rect::new(area.x, area.bottom() - 4 + row as u16, area.width, 1),
                        );
                    }
                }
            }
            return result;
        }
        result.targets.clear();
        let overlay = result.overlay_rect(76, 22);
        result.overlay = Some(overlay);
        let y = overlay.bottom() - 2;
        match &app.modal {
            Some(Modal::Settings) => {
                if let Some(draft) = &app.settings_ui {
                    if draft.editing() {
                        result.add(
                            Control::SettingsField,
                            Rect::new(overlay.x + 1, overlay.y + 2, overlay.width - 2, 1),
                        );
                    }
                    for (row, buttons) in draft.buttons(
                        app.settings_save_state,
                        app.config_location.user_directory().is_some(),
                        app.settings.base.is_some(),
                    ) {
                        let row = if draft.info() {
                            overlay.height - 3
                        } else {
                            row
                        };
                        let mut x = overlay.x + 1;
                        for button in buttons {
                            let width = button.label().width() as u16 + 2;
                            result.add(
                                Control::SettingsButton(button),
                                Rect::new(x, overlay.y + 1 + row, width, 1),
                            );
                            x += width + 1;
                        }
                    }
                }
            }
            Some(Modal::ReplaceAll(_)) => {
                result.add(Control::Search(15), Rect::new(overlay.x + 2, y, 16, 1));
                result.add(Control::Search(16), Rect::new(overlay.x + 20, y, 10, 1));
            }
            Some(Modal::Menu(index)) => {
                let count = overlay.height.saturating_sub(2) as usize;
                let start = index.saturating_sub(count.saturating_sub(1));
                for item in start..(start + count).min(MENU.len()) {
                    result.add(
                        Control::MenuItem(item),
                        Rect::new(
                            overlay.x + 1,
                            overlay.y + 1 + (item - start) as u16,
                            overlay.width - 2,
                            1,
                        ),
                    );
                }
            }
            Some(Modal::Language { row, .. } | Modal::SyntaxTheme { row, .. }) => {
                let rows = overlay.height.saturating_sub(3);
                result.list_rows = rows;
                let start = list_start(*row, rows);
                let count = match &app.modal {
                    Some(Modal::SyntaxTheme { config, .. }) => config.themes().count(),
                    _ => Choice::options().count(),
                };
                for index in start..(start + rows as usize).min(count) {
                    result.add(
                        Control::LanguageRow(index),
                        Rect::new(
                            overlay.x + 1,
                            overlay.y + 1 + (index - start) as u16,
                            overlay.width - 2,
                            1,
                        ),
                    );
                }
                result.add(Control::PathCancel, Rect::new(overlay.x + 1, y, 10, 1));
            }
            Some(Modal::Path(..)) => {
                result.add(
                    Control::PathField,
                    Rect::new(overlay.x + 1, overlay.y + 2, overlay.width - 2, 1),
                );
                result.add(Control::PathSubmit, Rect::new(overlay.x + 2, y, 6, 1));
                result.add(Control::PathCancel, Rect::new(overlay.x + 11, y, 10, 1));
            }
            Some(Modal::Recovery(dialog)) => {
                let rows = overlay.height.saturating_sub(4);
                result.list_rows = rows;
                result.add(
                    Control::RecoveryList,
                    Rect::new(overlay.x + 1, overlay.y + 1, overlay.width - 2, rows),
                );
                for (row, index) in (0..dialog.page.rows.len())
                    .skip(list_start(dialog.highlight, rows))
                    .take(rows as usize)
                    .enumerate()
                {
                    result.add(
                        Control::RecoveryRow(index),
                        Rect::new(
                            overlay.x + 1,
                            overlay.y + 1 + row as u16,
                            overlay.width - 2,
                            1,
                        ),
                    );
                }
                let first = overlay.bottom() - 3;
                result.add(Control::RecoveryOpen, Rect::new(overlay.x + 1, first, 6, 1));
                result.add(
                    Control::RecoveryRetry,
                    Rect::new(overlay.x + 8, first, 7, 1),
                );
                result.add(
                    Control::RecoveryDiscard,
                    Rect::new(overlay.x + 16, first, 9, 1),
                );
                result.add(Control::PathCancel, Rect::new(overlay.x + 26, first, 8, 1));
                if dialog.page.previous {
                    result.add(Control::RecoveryPrevious, Rect::new(overlay.x + 1, y, 6, 1));
                }
                if dialog.page.next {
                    result.add(Control::RecoveryNext, Rect::new(overlay.x + 8, y, 6, 1));
                }
            }
            Some(Modal::DiscardRecovery(..)) => {
                result.add(Control::ConfirmDiscard, Rect::new(overlay.x + 1, y, 10, 1));
                result.add(Control::ConfirmCancel, Rect::new(overlay.x + 13, y, 10, 1));
            }
            Some(Modal::Tabs(selected)) => {
                let rows = overlay.height.saturating_sub(3);
                result.list_rows = rows;
                result.add(
                    Control::TabList,
                    Rect::new(overlay.x + 1, overlay.y + 1, overlay.width - 2, rows),
                );
                let highlight = app
                    .screen
                    .tabs
                    .iter()
                    .position(|tab| Some(tab.id) == *selected)
                    .unwrap_or(0);
                for (row, tab) in app
                    .screen
                    .tabs
                    .iter()
                    .skip(list_start(highlight, rows))
                    .take(rows as usize)
                    .enumerate()
                {
                    result.add(
                        Control::TabListRow(tab.id),
                        Rect::new(
                            overlay.x + 1,
                            overlay.y + 1 + row as u16,
                            overlay.width - 2,
                            1,
                        ),
                    );
                }
                result.add(Control::TabListClose, Rect::new(overlay.x + 1, y, 8, 1));
                result.add(Control::PathCancel, Rect::new(overlay.x + 10, y, 9, 1));
                result.add(Control::TabListSwitch, Rect::new(overlay.x + 20, y, 9, 1));
            }
            Some(Modal::Open(dialog)) => {
                let rows = overlay.height.saturating_sub(5);
                result.list_rows = rows;
                result.add(
                    Control::PickerList,
                    Rect::new(overlay.x + 1, overlay.y + 2, overlay.width - 2, rows),
                );
                let start = dialog.list_start(rows);
                for (offset, row) in (start..dialog.count().min(start + rows as usize)).enumerate()
                {
                    result.add(
                        Control::PickerRow(row),
                        Rect::new(
                            overlay.x + 1,
                            overlay.y + 2 + offset as u16,
                            overlay.width - 2,
                            1,
                        ),
                    );
                }
                result.add(
                    Control::PathField,
                    Rect::new(overlay.x + 6, y - 1, overlay.width - 7, 1),
                );
                if dialog.parent().is_some() {
                    result.add(Control::PickerUp, Rect::new(overlay.x + 1, y, 5, 1));
                }
                if dialog.page_request(true).is_some() {
                    result.add(Control::PickerPrevious, Rect::new(overlay.x + 6, y, 6, 1));
                }
                if dialog.page_request(false).is_some() {
                    result.add(Control::PickerNext, Rect::new(overlay.x + 12, y, 6, 1));
                }
                result.add(Control::PickerOpen, Rect::new(overlay.x + 19, y, 6, 1));
                result.add(Control::PathCancel, Rect::new(overlay.x + 26, y, 8, 1));
            }
            Some(Modal::Confirm(..)) => {
                if app.stream_confirmation.is_none() {
                    result.add(Control::ConfirmSave, Rect::new(overlay.x + 1, y, 7, 1));
                }
                result.add(Control::ConfirmDiscard, Rect::new(overlay.x + 9, y, 10, 1));
                result.add(Control::ConfirmCancel, Rect::new(overlay.x + 21, y, 10, 1));
            }
            Some(Modal::SaveError(_)) => {
                result.add(Control::SaveErrorAs, Rect::new(overlay.x + 1, y, 11, 1));
                result.add(Control::PathCancel, Rect::new(overlay.x + 15, y, 10, 1));
            }
            Some(Modal::PersistenceError(_)) => {
                result.add(
                    Control::RecoveryErrorRetry,
                    Rect::new(overlay.x + 1, y, 7, 1),
                );
                result.add(Control::SaveErrorAs, Rect::new(overlay.x + 9, y, 10, 1));
                result.add(
                    Control::RecoveryErrorCancel,
                    Rect::new(overlay.x + 21, y, 13, 1),
                );
            }
            Some(Modal::Details(_) | Modal::TabDetails) => {
                result.add(Control::DetailsClose, Rect::new(overlay.x + 2, y, 9, 1));
                if app.screen.path.is_some() && app.screen.stream.is_none() {
                    result.add(Control::RetryRefresh, Rect::new(overlay.x + 13, y, 17, 1));
                }
            }
            None => {}
        }
        result
    }
    /// Allocate a compact first-row strip around the active tab.
    fn add_tab_strip(&mut self, app: &AppState) {
        let area = self.area;
        let tabs = &app.screen.tabs;
        let right = area.right() - 25;
        self.add(Control::Tabs, Rect::new(right - 6, area.y, 6, 1));
        let Some(active) = tabs.iter().position(|t| Some(t.id) == app.screen.tab) else {
            return;
        };
        self.add(Control::TabClose, Rect::new(right - 9, area.y, 3, 1));
        let width = area.width - 34;
        let count = (usize::from(width) / 12).max(1).min(tabs.len());
        let start = active.saturating_sub(count - 1).min(tabs.len() - count);
        let mut x = area.x;
        for (index, tab) in tabs.iter().skip(start).take(count).enumerate() {
            let cells =
                (width / count as u16 + u16::from(index == 0) * (width % count as u16)).min(24);
            self.add(Control::Tab(tab.id), Rect::new(x, area.y, cells, 1));
            x += cells;
        }
    }

    pub fn list_rows(&self) -> u16 {
        self.list_rows
    }

    /// Center an overlay entirely within a supported terminal geometry.
    fn overlay_rect(&self, max_width: u16, max_height: u16) -> Rect {
        let width = self.area.width.min(max_width).saturating_sub(4);
        let height = self.area.height.min(max_height).saturating_sub(2);
        Rect::new(
            self.area.x + (self.area.width - width) / 2,
            self.area.y + (self.area.height - height) / 2,
            width,
            height,
        )
    }
    /// Admit only nonempty visible controls.
    fn add(&mut self, control: Control, area: Rect) {
        if area.width > 0
            && area.height > 0
            && area.right() <= self.area.right()
            && area.bottom() <= self.area.bottom()
        {
            self.targets.push(Target { control, area });
        }
    }
    /// Test whether a focus target exists in this frame.
    pub fn contains(&self, control: Control) -> bool {
        self.targets.iter().any(|t| t.control == control)
    }
    /// Exclusive right/bottom boundaries; later targets are on top.
    pub fn hit(&self, x: u16, y: u16) -> Option<Control> {
        self.targets
            .iter()
            .rev()
            .find(|t| x >= t.area.x && x < t.area.right() && y >= t.area.y && y < t.area.bottom())
            .map(|t| t.control)
    }
    /// Cycle through only the currently visible interaction surface.
    pub fn next(&self, current: Option<Control>, backwards: bool) -> Option<Control> {
        let controls: Vec<_> = self
            .targets
            .iter()
            .filter(|t| {
                !matches!(
                    t.control,
                    Control::PickerRow(_) | Control::TabListRow(_) | Control::RecoveryRow(_)
                )
            })
            .collect();
        let count = controls.len();
        if count == 0 {
            return None;
        }
        let index = controls.iter().position(|t| Some(t.control) == current);
        let next = match index {
            None => {
                if backwards {
                    count - 1
                } else {
                    0
                }
            }
            Some(i) => {
                if backwards {
                    (i + count - 1) % count
                } else {
                    (i + 1) % count
                }
            }
        };
        Some(controls[next].control)
    }
}
fn list_start(highlight: usize, rows: u16) -> usize {
    highlight.saturating_sub(rows.saturating_sub(1) as usize)
}

fn tab_text(tab: &TabSummary, active: bool) -> String {
    format!(
        "{}{}{}{} {}{}",
        if active { ">" } else { " " },
        if tab.modified { "*" } else { "" },
        if tab.attention { "!" } else { "" },
        if tab.waiting {
            "…"
        } else if tab.loading {
            "↓"
        } else if tab.saving {
            "↑"
        } else if tab.queued_save {
            "⇡"
        } else {
            ""
        },
        if tab.scratch { "~" } else { "" },
        tab.label
    )
}

/// Render a prepared view without performing text reads or recomputing Unicode boundaries.
pub(crate) fn render(frame: &mut Frame<'_>, app: &AppState, layout: &LayoutSnapshot) {
    let area = layout.area;
    if area.width < 40 || area.height < 8 {
        let context = app.key_context();
        let (context, command) = if context.has_global() {
            (Context::Global, Command::Quit)
        } else {
            (context, Command::Cancel)
        };
        let hint = app.settings.applied.map.portable_hint(context, command);
        let text = if app.settings.starting {
            "Escape uses defaults".to_owned()
        } else if matches!(app.modal, Some(Modal::Settings))
            && app
                .settings_ui
                .as_ref()
                .is_some_and(|draft| draft.capturing())
        {
            format!("{hint} exit capture")
        } else {
            format!("{hint} {}", command.label())
        };
        let message = if area.height > 1 {
            format!("Resize to 40x8\n{text}")
        } else {
            format!("{text} | Resize to 40x8")
        };
        frame.render_widget(Paragraph::new(message), area);
        return;
    }
    if app.screen.tab.is_none() {
        frame.render_widget(
            Paragraph::new(if app.screen.loading {
                "Preparing document"
            } else {
                "No open tabs"
            }),
            Rect::new(area.x, area.y, area.width - 31, 1),
        );
    }
    if app.switching.is_some() {
        frame.render_widget(
            Paragraph::new("Preparing tab view"),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
    } else if let Some(preview) = &app.screen.preview {
        let visible = preview
            .lines()
            .take(layout.content_height() as usize)
            .map(|line| safe(&line.chars().take(area.width as usize).collect::<String>()))
            .collect::<Vec<_>>()
            .join("\n");
        frame.render_widget(
            Paragraph::new(visible),
            Rect::new(area.x, area.y + 1, area.width, area.height - 2),
        );
    } else if let Some(view) = &app.screen.view {
        let selection = app.screen.position.selection();
        let syntax = app
            .screen
            .tab
            .zip(app.screen.document)
            .and_then(|(tab, document)| {
                app.screen.syntax_projection.as_ref().filter(|projection| {
                    projection.key == ViewKey::new(tab, document, view)
                        && view.generation == layout.generation
                })
            });
        for (y, row) in view
            .rows
            .iter()
            .take(layout.content_height() as usize)
            .enumerate()
        {
            for glyph in &row.spans {
                let start = glyph.column.max(view.left);
                let end = (glyph.column + glyph.width).min(view.left + area.width as u64);
                if end <= start {
                    continue;
                }
                let x = area.x + (start - view.left) as u16;
                let width = (end - start) as u16;
                if glyph.unit.is_some() {
                    render_run(
                        frame,
                        glyph,
                        &selection,
                        start,
                        Rect::new(x, area.y + 1 + y as u16, width, 1),
                    );
                } else {
                    let selected =
                        glyph.bytes.start < selection.end && glyph.bytes.end > selection.start;
                    let clipped = glyph.column < view.left
                        || glyph.column + glyph.width > view.left + area.width as u64;
                    let text = if clipped {
                        "·".repeat(width as usize)
                    } else {
                        glyph.text.clone()
                    };
                    frame.render_widget(
                        Paragraph::new(text).style(selection_style(selected)),
                        Rect::new(x, area.y + 1 + y as u16, width, 1),
                    );
                }
                if let Some(syntax) = syntax {
                    paint_syntax(
                        frame,
                        glyph,
                        &selection,
                        &syntax.ranges,
                        app.screen.syntax_palette,
                        start,
                        Rect::new(x, area.y + 1 + y as u16, width, 1),
                    );
                }
            }
        }
        // Paint search decorations from exact logical ranges over the prepared
        // cell map. Grapheme-bisecting matches color the intersecting cells.
        let status = &app.screen.search;
        let ordinary = status.decorated.is_some_and(|(tab, revision, _, _, _)| {
            Some(tab) == app.screen.tab && revision == view.revision
        });
        let current = status
            .current
            .as_ref()
            .filter(|found| Some(found.tab) == app.screen.tab && found.revision == view.revision);
        for (range, selected) in status
            .decorations
            .iter()
            .filter(|_| ordinary)
            .map(|r| (r, false))
            .chain(current.map(|found| (&found.range, true)))
        {
            for (y, row) in view
                .rows
                .iter()
                .take(layout.content_height() as usize)
                .enumerate()
            {
                for span in &row.spans {
                    if range.start >= span.bytes.end || range.end <= span.bytes.start {
                        continue;
                    }
                    let (lo, hi) = if let Some((bytes, cells)) = span.unit {
                        let (bytes, cells) = (u64::from(bytes), u64::from(cells));
                        (
                            span.column
                                + range.start.saturating_sub(span.bytes.start) / bytes * cells,
                            span.column
                                + range
                                    .end
                                    .min(span.bytes.end)
                                    .saturating_sub(span.bytes.start)
                                    .div_ceil(bytes)
                                    * cells,
                        )
                    } else {
                        (span.column, span.column + span.width)
                    };
                    for column in lo.max(view.left)..hi.min(view.left + u64::from(area.width)) {
                        let style = Style::default()
                            .bg(if selected {
                                Color::Magenta
                            } else {
                                Color::Yellow
                            })
                            .fg(Color::Black);
                        if let Some(cell) = frame
                            .buffer_mut()
                            .cell_mut((area.x + (column - view.left) as u16, area.y + 1 + y as u16))
                        {
                            cell.set_style(style);
                        }
                    }
                }
            }
        }
        if !app.help
            && app.modal.is_none()
            && !app.screen.loading
            && !app.pending
            && view.generation == layout.generation
            && app.focus == Some(Control::Content)
            && let Some((x, y)) = view.cursor
            && x < area.width
            && y < layout.content_height()
        {
            frame.set_cursor_position((area.x + x, area.y + 1 + y));
        }
    }
    let coordinates = app.screen.view.as_ref().map_or_else(
        || "Ln … Col …".into(),
        |v| format!("Ln {} Col {}", v.line + 1, v.column + 1),
    );
    let severity = match app.status.severity {
        crate::status::Severity::Info => "",
        crate::status::Severity::Warning => "Warning: ",
        crate::status::Severity::Error => "Error: ",
    };
    let footer = app.receiving_paste_status().unwrap_or_else(|| {
        if app.guard_waiting() {
            let cancel = app
                .settings
                .applied
                .map
                .portable_hint(Context::Dialog, Command::Cancel);
            return format!("Waiting for tab state or save | {cancel} cancels");
        }
        if app.modal.is_none() && !app.help && app.reports_clipboard() {
            return app.status.text().to_owned();
        }
        let syntax = match (
            matches!(app.screen.syntax_status, SyntaxStatus::Fallback(_)),
            app.screen.syntax_notice.is_some(),
        ) {
            (true, true) => "Plain syntax | Config error | ",
            (true, false) => "Plain syntax | ",
            (false, true) => "Syntax config error | ",
            (false, false) => "",
        };
        let config = if app.config_location.safe() {
            "Safe config | "
        } else if app.settings_save_state == Some(crate::settings::SaveState::Finishing) {
            "Finishing save | "
        } else if app.settings.diagnostic.is_some() {
            "Settings error | "
        } else if app.settings_saved {
            "Settings saved | "
        } else if app.settings.user_config {
            "User config | "
        } else {
            ""
        };
        format!(
            "{config}{syntax}{}{} | {}{}{}",
            severity,
            app.status.text(),
            coordinates,
            if app.pending { " | working" } else { "" },
            if !app.search.open && !app.screen.search.message.is_empty() {
                format!(" | {}", app.screen.search.message)
            } else {
                String::new()
            }
        )
    });
    frame.render_widget(
        Paragraph::new(footer),
        Rect::new(
            area.x,
            area.bottom() - 1,
            area.width
                - if layout
                    .targets
                    .iter()
                    .any(|target| target.control == Control::ToggleFollow)
                {
                    16
                } else {
                    0
                },
            1,
        ),
    );
    if app.search.open && app.modal.is_none() && !app.help {
        let status = &app.screen.search;
        let mut text = if status.busy {
            format!(
                "{} | {}/{} bytes; {} matches",
                status.message,
                status.progress.scanned,
                status.progress.total,
                status.progress.matches
            )
        } else {
            status.message.clone()
        };
        if let Some(error) = &app.field_error {
            text = error.clone();
        }
        if status.partial_highlighting {
            text.push_str(" | Partial highlighting");
        }
        frame.render_widget(
            Paragraph::new(text),
            Rect::new(area.x, area.bottom() - 2, area.width, 1),
        );
    }
    if let Some(overlay) = layout.overlay {
        if matches!(app.modal, Some(Modal::Settings)) {
            render_settings(frame, app, layout, overlay);
            return;
        }
        frame.render_widget(Clear, overlay);
        let activate = app
            .settings
            .applied
            .map
            .portable_hint(Context::Dialog, Command::Activate);
        let language_title = format!(" Language — {activate} to apply ");
        let theme_title = format!(" Syntax Theme — {activate} to apply ");
        let tabs_title = format!(" Tabs — {activate} to switch ");
        let title = match &app.modal {
            Some(Modal::Settings) => unreachable!(),
            Some(Modal::ReplaceAll(_)) => " Replace all — confirm scope ",
            Some(Modal::Menu(_)) => " Actions (wheel/arrows) ",
            Some(Modal::Language { .. }) => &language_title,
            Some(Modal::SyntaxTheme { .. }) => &theme_title,
            Some(Modal::Open(dialog)) if dialog.follow => " Follow file ",
            Some(Modal::Open(_)) => " Open file ",
            Some(Modal::Tabs(_)) => &tabs_title,
            Some(Modal::Recovery(_)) => " Recovery — retained scratches ",
            Some(Modal::DiscardRecovery(..)) => " Discard retained scratch? ",
            Some(Modal::Path(PathKind::Save, _)) => " Save As (new path) ",
            Some(Modal::Path(PathKind::GoTo, _)) => " Go to line[:cell] (from 1) ",
            Some(Modal::Path(PathKind::Export, _)) => " Export Retained Copy (new path) ",
            Some(Modal::Confirm(_, Some(_))) => " Destination may have changed ",
            Some(Modal::Confirm(..)) if app.stream_confirmation.is_some() => {
                " Discard stream history "
            }
            Some(Modal::Confirm(..)) => " Unsaved changes ",
            Some(Modal::Details(_) | Modal::TabDetails) => " Details ",
            Some(Modal::SaveError(_)) => " Save failed — work retained ",
            Some(Modal::PersistenceError(_)) => " Scratch persistence failed ",
            None => " Help ",
        };
        frame.render_widget(Block::default().title(title).borders(Borders::ALL), overlay);
        let inner = Rect::new(
            overlay.x + 1,
            overlay.y + 1,
            overlay.width - 2,
            overlay.height - 3,
        );
        let text = match &app.modal {
            Some(Modal::Settings) => unreachable!(),
            Some(Modal::ReplaceAll(request)) => format!(
                "{}\nFind: {}\nReplace: {}\nMatch case: {} | Whole word: {}\nOne undo step per changed document; no disk save.",
                if request.all {
                    "All open documents"
                } else {
                    "Current document"
                },
                crate::app::search::display(&request.query.literal),
                crate::app::search::display(&request.replacement),
                request.query.match_case,
                request.query.whole_word
            ),
            Some(
                Modal::Menu(_)
                | Modal::Language { .. }
                | Modal::SyntaxTheme { .. }
                | Modal::Path(..)
                | Modal::Open(_)
                | Modal::Tabs(_),
            ) => String::new(),
            Some(Modal::Recovery(dialog)) => {
                if dialog.loading {
                    "Loading retained scratches…".into()
                } else if dialog.page.rows.is_empty() {
                    "No unloaded scratches on this page".into()
                } else {
                    String::new()
                }
            }
            Some(Modal::DiscardRecovery(dialog, scratch)) => {
                let label = dialog
                    .page
                    .rows
                    .iter()
                    .find(|row| row.id == *scratch)
                    .map_or("Scratch", |row| row.label.as_str());
                format!("{label}\nPermanently remove these retained contents?")
            }
            Some(Modal::Confirm(after, uncertain)) => {
                if let Some(message) = &app.stream_confirmation {
                    message.clone()
                } else {
                    let verb = if matches!(after, After::Close(_)) {
                        "Close"
                    } else {
                        "Quit"
                    };
                    let id = after.target().or(app.screen.tab);
                    let label = app.tab_path_hint(id).map_or_else(
                        || app.tab_label(id).to_owned(),
                        |path| tail_cells(path, inner.width.saturating_sub(verb.len() as u16 + 3)),
                    );
                    let guidance = if uncertain.is_some() {
                        "A save may have changed the destination. Discard acknowledges this."
                    } else {
                        "Save your changes before continuing?"
                    };
                    format!("{verb} — {label}\n{guidance}")
                }
            }
            Some(Modal::TabDetails) => app.tab_details(),
            Some(Modal::Details(text) | Modal::SaveError(text) | Modal::PersistenceError(text)) => {
                safe(text)
            }
            None => {
                let map = &app.settings.applied.map;
                let up = map.portable_hint(
                    Context::Dialog,
                    Command::Move(crate::command::Movement::Up, false),
                );
                let down = map.portable_hint(
                    Context::Dialog,
                    Command::Move(crate::command::Movement::Down, false),
                );
                let cancel = map.portable_hint(Context::Dialog, Command::Cancel);
                let actions = map.portable_hint(Context::Global, Command::Menu);
                let mut lines = vec![
                    format!("{up}/{down} scroll help; {cancel} closes"),
                    if app.enhanced_keyboard {
                        "Enhanced keys negotiated. Cmd needs terminal delivery.".into()
                    } else {
                        "Cmd aliases require terminal delivery.".into()
                    },
                    "Terminal paste inserts text through bracketed paste.".into(),
                    format!("{actions} opens Actions and the mouse capture toggle."),
                    "Selection modifiers vary; tmux: copy mode.".into(),
                    "Tabs: > active, * modified, ! save attention".into(),
                    "Tabs: … waiting, ↓ loading, ⇡ queued save, ↑ saving".into(),
                    "Actions contains Language, Syntax Theme and Settings.".into(),
                ];
                if app.config_location.safe() {
                    lines.splice(
                        1..1,
                        [
                            "Safe config uses bundled settings. Reload disabled.".into(),
                            app.config_location.display_file().map_or_else(
                                || "Configuration path unavailable".into(),
                                |path| format!("Expected config file {path}"),
                            ),
                        ],
                    );
                } else if app.config_location.user_directory().is_none() {
                    lines.insert(
                        1,
                        "Config unavailable. Use an absolute XDG_CONFIG_HOME or HOME.".into(),
                    );
                }
                lines.extend(app.settings.applied.map.rows.iter().map(|row| {
                    let keys = row
                        .keys()
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" / ");
                    format!(
                        "{} • {} • {}",
                        row.identifier(),
                        row.command.label(),
                        if keys.is_empty() { "Unbound" } else { &keys }
                    )
                }));
                lines.join("\n")
            }
        };
        if let Some(Modal::Open(dialog)) = &app.modal {
            let directory = safe(&dialog.directory.to_string_lossy());
            let directory_width = if app.field_error.is_some() {
                inner.width / 3
            } else {
                inner.width
            };
            let status = if dialog.loading { "Loading " } else { "Dir " };
            frame.render_widget(
                Paragraph::new(format!(
                    "{status}{}",
                    tail_cells(
                        &directory,
                        directory_width.saturating_sub(status.len() as u16)
                    )
                )),
                Rect::new(inner.x, inner.y, directory_width, 1),
            );
            if let Some(error) = &app.field_error {
                frame.render_widget(
                    Paragraph::new(error.as_str()).style(Style::default().fg(Color::Red)),
                    Rect::new(
                        inner.x + directory_width,
                        inner.y,
                        inner.width - directory_width,
                        1,
                    ),
                );
            }
            let y = overlay.bottom() - 3;
            frame.render_widget(Paragraph::new("Path"), Rect::new(inner.x, y, 5, 1));
            if let Some(target) = layout
                .targets
                .iter()
                .find(|target| target.control == Control::PathField)
            {
                render_field(frame, app, &dialog.field, target.area);
            }
        } else if let Some(Modal::Path(kind, field)) = &app.modal {
            let label = match kind {
                PathKind::GoTo => "Line[:cell], starting at 1:",
                PathKind::Export
                    if app
                        .screen
                        .stream
                        .as_ref()
                        .is_some_and(|stream| !stream.retry) =>
                {
                    "Only the committed prefix will be copied. Enter a new path."
                }
                PathKind::Export => "Export the complete retained extent to a new path.",
                PathKind::Save => "Path (no shell expansion):",
            };
            frame.render_widget(
                Paragraph::new(label),
                Rect::new(inner.x, inner.y, inner.width, 1),
            );
            render_field(
                frame,
                app,
                field,
                Rect::new(inner.x, inner.y + 1, inner.width, 1),
            );
            if let Some(error) = &app.field_error {
                frame.render_widget(
                    Paragraph::new(safe(error)).style(Style::default().fg(Color::Red)),
                    Rect::new(inner.x, inner.y + 2, inner.width, 1),
                );
            }
        } else {
            frame.render_widget(
                Paragraph::new(text).wrap(Wrap { trim: false }).scroll((
                    if app.help
                        || matches!(
                            app.modal,
                            Some(
                                Modal::Details(_)
                                    | Modal::TabDetails
                                    | Modal::ReplaceAll(_)
                                    | Modal::SaveError(_)
                                    | Modal::PersistenceError(_)
                            )
                        )
                    {
                        app.help_scroll
                    } else {
                        0
                    },
                    0,
                )),
                inner,
            );
        }
    }
    for target in &layout.targets {
        if let Control::Tab(id) = target.control {
            if let Some(tab) = app.screen.tabs.iter().find(|tab| tab.id == id) {
                let active = app.screen.tab == Some(id);
                let label = tab_text(tab, false);
                let label = label.trim_start();
                let width = (UnicodeWidthStr::width(label) as u16).min(target.area.width - 2);
                let style = if active {
                    Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)
                } else if app.focus == Some(target.control) {
                    Style::default().add_modifier(Modifier::UNDERLINED)
                } else {
                    Style::default()
                };
                let x = target.area.x;
                let y = target.area.y;
                frame.render_widget(Paragraph::new("[").style(style), Rect::new(x, y, 1, 1));
                frame.render_widget(
                    Paragraph::new(label).style(style),
                    Rect::new(x + 1, y, width, 1),
                );
                frame.render_widget(
                    Paragraph::new("]").style(style),
                    Rect::new(x + 1 + width, y, 1, 1),
                );
            }
            continue;
        }
        if matches!(target.control, Control::WelcomeOpen | Control::WelcomeNew) {
            let command = target.control.command().expect("welcome action");
            let key = app
                .settings
                .applied
                .map
                .portable_hint(Context::Global, command);
            let label = if key.is_empty() {
                target.control.label().to_owned()
            } else {
                format!("{} ({key})", target.control.label())
            };
            let style = if app.focus == Some(target.control) {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            frame.render_widget(Paragraph::new(label).style(style), target.area);
            continue;
        }
        let row = match (target.control, &app.modal) {
            (Control::ToggleFollow, _) => app.screen.stream.as_ref().map(|status| {
                (
                    if status.following {
                        "Pause View".into()
                    } else {
                        "Resume Follow".into()
                    },
                    app.focus == Some(Control::ToggleFollow),
                )
            }),
            (Control::PickerOpen, Some(Modal::Open(dialog))) if dialog.follow => {
                Some(("Follow".into(), app.focus == Some(Control::PickerOpen)))
            }
            (Control::TabListRow(id), _) => {
                app.screen.tabs.iter().find(|tab| tab.id == id).map(|tab| {
                    let highlighted =
                        matches!(app.modal, Some(Modal::Tabs(Some(highlight))) if highlight == id);
                    (
                        tab_text(tab, app.screen.tab == Some(id)),
                        highlighted
                            || app.focus == Some(target.control)
                            || app.screen.tab == Some(id),
                    )
                })
            }
            (Control::PickerRow(index), Some(Modal::Open(dialog))) => {
                dialog.row(index).map(|row| {
                    let selected = index == dialog.highlight;
                    (
                        format!("{} {}", if selected { ">" } else { " " }, row.label()),
                        selected,
                    )
                })
            }
            (Control::LanguageRow(index), Some(Modal::Language { tab, selected, row })) => {
                Choice::options().nth(index).map(|choice| {
                    let active = app.screen.tab == Some(*tab);
                    let name = if choice == Choice::Auto && active {
                        let detected = Choice::Auto
                            .name(app.screen.path.as_deref(), &app.screen.syntax_config);
                        format!("Auto ({detected})")
                    } else if choice == Choice::Auto {
                        "Auto".to_owned()
                    } else {
                        choice.name(None, &app.screen.syntax_config).to_owned()
                    };
                    let selection = if active {
                        app.screen.syntax_language
                    } else {
                        *selected
                    };
                    let current = if choice == selection {
                        " (current)"
                    } else {
                        ""
                    };
                    (format!("{name}{current}"), index == *row)
                })
            }
            (Control::LanguageRow(index), Some(Modal::SyntaxTheme { config, row })) => {
                config.themes().nth(index).map(|name| {
                    let current = if app.screen.syntax_theme.as_ref() == name {
                        " (current)"
                    } else {
                        ""
                    };
                    (format!("{name}{current}"), index == *row)
                })
            }
            (Control::RecoveryRow(index), Some(Modal::Recovery(dialog))) => {
                dialog.page.rows.get(index).map(|row| {
                    (
                        format!(
                            "{} {}",
                            if index == dialog.highlight { ">" } else { " " },
                            row.label
                        ),
                        index == dialog.highlight,
                    )
                })
            }
            _ => None,
        };
        if let Some((label, focused)) = row {
            let style = if focused {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            frame.render_widget(Paragraph::new(label).style(style), target.area);
            continue;
        }
        if matches!(
            target.control,
            Control::Content
                | Control::PathField
                | Control::PickerList
                | Control::TabList
                | Control::RecoveryList
        ) {
            continue;
        }
        if let Control::Search(index) = target.control {
            let label = if index <= 1 {
                let (prefix, field) = if index == 0 {
                    ("Find: ", &app.search.query.literal)
                } else {
                    ("Replace: ", &app.search.replacement)
                };
                format!(
                    "{prefix}{}",
                    tail_cells(
                        &crate::app::search::display(field),
                        target.area.width.saturating_sub(prefix.len() as u16 + 2)
                    )
                )
            } else {
                let label = app.search.label(index);
                let command = match index {
                    5 => Some(Command::FindPrevious),
                    6 => Some(Command::FindNext),
                    7 => Some(Command::ReplaceNext),
                    8 => Some(Command::ReplaceAll),
                    9 => Some(Command::CancelSearch),
                    _ => None,
                };
                let keys = command
                    .map(|command| app.settings.applied.map.shortcuts(Context::Global, command))
                    .unwrap_or_default();
                if keys.is_empty() {
                    label
                } else {
                    format!("{label} ({keys})")
                }
            };
            let focused = app.focus == Some(target.control);
            let style = if focused {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            frame.render_widget(
                Paragraph::new(tail_cells(
                    &format!("{}{label}", if focused { ">" } else { " " }),
                    target.area.width,
                ))
                .style(style),
                target.area,
            );
            if focused && index <= 1 {
                frame.set_cursor_position((
                    target.area.x
                        + (label.width() as u16 + 1).min(target.area.width.saturating_sub(1)),
                    target.area.y,
                ));
            }
            continue;
        }
        let focused = app.focus == Some(target.control)
            || matches!((app.modal.as_ref(),target.control),(Some(Modal::Menu(index)),Control::MenuItem(item)) if *index==item);
        let title = match target.control {
            Control::MenuItem(index) if MENU[index] == Command::MouseCapture => {
                if app.mouse_capture {
                    "Mouse capture: enabled (toggle)".to_owned()
                } else {
                    "Mouse capture: paused (toggle)".to_owned()
                }
            }
            Control::MenuItem(index)
                if matches!(
                    MENU[index],
                    Command::ReloadHighlighting | Command::ReloadSettings
                ) && app.config_location.user_directory().is_none() =>
            {
                format!("{} (disabled)", MENU[index].label())
            }
            Control::MenuItem(index) => {
                let command = MENU[index];
                let keys = app.settings.applied.map.shortcuts(Context::Global, command);
                let keys = if keys.is_empty() {
                    app.settings.applied.map.shortcuts(Context::Editor, command)
                } else {
                    keys
                };
                if keys.is_empty() {
                    command.label().to_owned()
                } else {
                    format!("{:<19} {keys}", command.label())
                }
            }
            control => control.label().to_owned(),
        };
        let label = if focused {
            format!(">{title}<")
        } else {
            format!("[{title}]")
        };
        let style = if focused {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        frame.render_widget(Paragraph::new(label).style(style), target.area);
    }
}
/// Display the right end of an editable field using the prepared hit geometry.
fn render_settings(frame: &mut Frame<'_>, app: &AppState, layout: &LayoutSnapshot, overlay: Rect) {
    let Some(draft) = &app.settings_ui else {
        return;
    };
    frame.render_widget(Clear, overlay);
    frame.render_widget(
        Block::default().title(" Settings ").borders(Borders::ALL),
        overlay,
    );
    for (row, text) in draft.lines(&app.settings, &app.config_location) {
        let height = if draft.info() {
            overlay.height.saturating_sub(3)
        } else {
            1
        };
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .scroll((draft.info_scroll, 0)),
            Rect::new(
                overlay.x + 1,
                overlay.y + 1 + row,
                overlay.width - 2,
                height,
            ),
        );
    }
    for target in &layout.targets {
        let focus = app.focus == Some(target.control);
        let style = if focus {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        match target.control {
            Control::SettingsButton(button) => {
                let (open, close) = if focus { ('>', '<') } else { ('[', ']') };
                frame.render_widget(
                    Paragraph::new(format!("{open}{}{close}", button.label())).style(style),
                    target.area,
                );
            }
            Control::SettingsField => {
                let text = tail_cells(&safe(&draft.field), target.area.width);
                if focus {
                    let column = (text.width() as u16).min(target.area.width.saturating_sub(1));
                    frame.set_cursor_position((target.area.x + column, target.area.y));
                }
                frame.render_widget(Paragraph::new(text).style(style), target.area);
            }
            _ => {}
        }
    }
}

fn render_field(frame: &mut Frame<'_>, app: &AppState, field: &str, area: Rect) {
    let field = safe(field);
    let width = field.width();
    let offset = width.saturating_sub(area.width.saturating_sub(1) as usize);
    frame.render_widget(Paragraph::new(field).scroll((0, offset as u16)), area);
    if app.focus == Some(Control::PathField) {
        frame.set_cursor_position((area.x + (width - offset) as u16, area.y));
    }
}

/// Keep the basename visible in a clipped directory without splitting a scalar.
fn tail_cells(text: &str, width: u16) -> String {
    use unicode_width::UnicodeWidthChar;
    if text.width() <= usize::from(width) {
        return text.to_owned();
    }
    let mut left = usize::from(width.saturating_sub(1));
    let mut start = text.len();
    for (index, ch) in text.char_indices().rev() {
        let cells = ch.width().unwrap_or(0);
        if cells > left {
            break;
        }
        left -= cells;
        start = index;
    }
    format!("…{}", &text[start..])
}

/// Draw a prepared stride run in at most three full substrings and two clipped tokens.
/// All byte/cell boundaries come from the worker; drawing does no Unicode segmentation.
fn render_run(frame: &mut Frame<'_>, span: &Span, selection: &Range<u64>, start: u64, area: Rect) {
    let (bytes, cells) = span.unit.expect("prepared run");
    let (bytes, cells) = (u64::from(bytes), u64::from(cells));
    let end = start + u64::from(area.width);
    let first = (start - span.column).div_ceil(cells);
    let last = (end - span.column) / cells;
    let partial_end = end.min(span.column + first * cells);
    for (lo, hi, index) in [
        (start, partial_end, (start - span.column) / cells),
        ((span.column + last * cells).max(partial_end), end, last),
    ] {
        if hi > lo {
            let byte = span.bytes.start + index * bytes;
            let style = selection_style(byte < selection.end && byte + bytes > selection.start);
            frame.render_widget(
                Paragraph::new("·".repeat((hi - lo) as usize)).style(style),
                Rect::new(area.x + (lo - start) as u16, area.y, (hi - lo) as u16, 1),
            );
        }
    }
    if first >= last {
        return;
    }
    let (selected_start, selected_end) = if selection.is_empty() {
        (first, first)
    } else {
        (
            (selection.start.saturating_sub(span.bytes.start) / bytes).clamp(first, last),
            (selection
                .end
                .saturating_sub(span.bytes.start)
                .div_ceil(bytes))
            .clamp(first, last),
        )
    };
    for (lo, hi, selected) in [
        (first, selected_start, false),
        (selected_start, selected_end, true),
        (selected_end, last, false),
    ] {
        if hi > lo {
            frame.render_widget(
                Paragraph::new(&span.text[(lo * bytes) as usize..(hi * bytes) as usize])
                    .style(selection_style(selected)),
                Rect::new(
                    area.x + (span.column + lo * cells - start) as u16,
                    area.y,
                    ((hi - lo) * cells) as u16,
                    1,
                ),
            );
        }
    }
}
/// Shared occupied-cell selection styling for full and clipped run fragments.
fn selection_style(selected: bool) -> Style {
    if selected {
        Style::default().bg(Color::Blue).fg(Color::White)
    } else {
        Style::default()
    }
}

/// Paint foreground only on visible, unselected cells of a validated glyph interval.
fn paint_syntax(
    frame: &mut Frame<'_>,
    glyph: &Span,
    selection: &Range<u64>,
    ranges: &[crate::syntax::Styled],
    palette: crate::syntax::Palette,
    left: u64,
    area: Rect,
) {
    let first = ranges.partition_point(|range| u64::from(range.end) <= glyph.bytes.start);
    let unit = glyph
        .unit
        .map(|(bytes, cells)| (u64::from(bytes), u64::from(cells)));
    for range in ranges[first..]
        .iter()
        .take_while(|range| u64::from(range.start) < glyph.bytes.end)
    {
        let (lo, hi) = unit.map_or(
            (glyph.column, glyph.column + glyph.width),
            |(bytes, cells)| {
                (
                    glyph.column
                        + u64::from(range.start).saturating_sub(glyph.bytes.start) / bytes * cells,
                    glyph.column
                        + (u64::from(range.end).min(glyph.bytes.end) - glyph.bytes.start) / bytes
                            * cells,
                )
            },
        );
        let foreground = match palette.color(range.category) {
            Foreground::Ansi(index) => Color::Indexed(index),
            Foreground::Rgb(r, g, b) => Color::Rgb(r, g, b),
        };
        for column in lo.max(left)..hi.min(left + u64::from(area.width)) {
            let (start, end) =
                unit.map_or((glyph.bytes.start, glyph.bytes.end), |(bytes, cells)| {
                    let start = glyph.bytes.start + (column - glyph.column) / cells * bytes;
                    (start, start + bytes)
                });
            if start < selection.end && selection.start < end {
                continue;
            }
            if let Some(cell) = frame
                .buffer_mut()
                .cell_mut((area.x + (column - left) as u16, area.y))
            {
                cell.fg = foreground;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::After;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn help_explains_unavailable_configuration_directory() {
        let mut app = AppState::new();
        app.help = true;
        let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 80, 24), &app, 1);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(frame, &app, &layout)).unwrap();
        let buffer = terminal.backend().buffer();
        let message = "Config unavailable. Use an absolute XDG_CONFIG_HOME or HOME.";
        assert!((0..24).any(|y| {
            let line: String = (0..80).map(|x| buffer[(x, y)].symbol()).collect();
            line.contains(message)
        }));
    }

    /// Compact ASCII spans paint only the selected cells, preserving their individual hit positions.
    #[test]
    fn ascii_span_selection_is_cell_exact() {
        use crate::{
            document::{Cancellation, Document, Edit, OpenOptions, TextRange},
            text::{Checkpoints, Position, Text, ViewRequest},
        };
        use std::sync::Arc;
        let mut document = Document::new(OpenOptions::default()).unwrap();
        document
            .apply(
                document.revision(),
                [Edit {
                    range: TextRange::new(0, 0).unwrap(),
                    text: "abcdef",
                }],
                &Cancellation::default(),
            )
            .unwrap();
        let snapshot = document.snapshot().unwrap();
        let cancel = Cancellation::default();
        let position = Position {
            caret: 4,
            anchor: 2,
        };
        let view = Text::new(&snapshot, &cancel)
            .view(
                ViewRequest {
                    position,
                    top: 0,
                    left: 0,
                    width: 80,
                    height: 22,
                    reveal: false,
                    generation: 1,
                },
                &mut Checkpoints::default(),
            )
            .unwrap();
        assert_eq!(view.rows[0].spans.len(), 1);
        assert_eq!(view.rows[0].hit(3), 3);
        let mut app = AppState::new();
        app.screen.loading = false;
        app.screen.position = position;
        app.screen.view = Some(Arc::new(view));
        let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 80, 24), &app, 1);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(frame, &app, &layout)).unwrap();
        for x in 0..6 {
            assert_eq!(
                terminal.backend().buffer()[(x, 1)].bg,
                if (2..4).contains(&x) {
                    Color::Blue
                } else {
                    Color::Reset
                }
            );
        }
    }

    /// Modal targets and all rendered cells remain safe at zero/minimum/large dimensions.
    #[test]
    fn geometry_render_and_modal_isolation() {
        for (width, height) in [(0, 0), (1, 1), (39, 7), (40, 8), (80, 24), (200, 100)] {
            for modal in [
                None,
                Some(Modal::Menu(13)),
                Some(Modal::Path(PathKind::GoTo, "test".into())),
                Some(Modal::Confirm(After::Quit, None)),
            ] {
                let mut app = AppState::new();
                app.modal = modal;
                let layout = LayoutSnapshot::for_app(Rect::new(0, 0, width, height), &app, 1);
                app.sync_layout(&layout);
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| render(frame, &app, &layout)).unwrap();
                for target in &layout.targets {
                    assert!(target.area.right() <= width && target.area.bottom() <= height);
                    assert_eq!(
                        layout.hit(target.area.x, target.area.y),
                        Some(target.control)
                    );
                }
                if app.modal.is_some() {
                    assert!(!layout.contains(Control::Content));
                }
                assert_eq!(layout.hit(width, height), None);
            }
        }
    }
    /// A wide Unicode run clipped at both sides paints exact selected occupied cells.
    #[test]
    fn unicode_run_clipping_and_selection() {
        use ratatui::{Terminal, backend::TestBackend};
        let span = Span {
            bytes: 0..90,
            column: 0,
            width: 60,
            text: "界".repeat(30),
            unit: Some((3, 2)),
        };
        let mut terminal = Terminal::new(TestBackend::new(40, 1)).unwrap();
        let mut painted = None;
        terminal
            .draw(|frame| {
                render_run(frame, &span, &(0..6), 1, Rect::new(0, 0, 40, 1));
                // Backend diffs omit trailing wide-character cells; inspect the
                // prepared frame's occupied-cell styling before that conversion.
                painted = Some(frame.buffer_mut().clone());
            })
            .unwrap();
        let buffer = painted.as_ref().unwrap();
        assert_eq!(buffer[(0, 0)].symbol(), "·");
        assert_eq!(buffer[(39, 0)].symbol(), "·");
        assert_eq!(buffer[(1, 0)].symbol(), "界");
        for x in 0..40 {
            assert_eq!(
                buffer[(x, 0)].bg,
                if x < 3 { Color::Blue } else { Color::Reset },
                "cell {x}"
            );
        }
    }
}
