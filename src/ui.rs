//! Shared geometry for rendering, focus traversal, and pointer hit-testing.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::{
    app::AppState,
    command::{BINDINGS, Command},
    status::Severity,
};

/// A stable focus/hit target; document text positions will be separate from these cell targets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Control {
    Content,
    Help,
    Quit,
    HelpQuit,
    HelpSuspend,
    HelpClose,
}

impl Control {
    /// Buttons resolve to the same commands as keyboard bindings.
    pub(crate) fn command(self) -> Option<Command> {
        match self {
            Self::Content => None,
            Self::Help => Some(Command::Help),
            Self::Quit | Self::HelpQuit => Some(Command::Quit),
            Self::HelpSuspend => Some(Command::Suspend),
            Self::HelpClose => Some(Command::Cancel),
        }
    }

    /// Short labels stay usable in the minimum-size layout.
    fn label(self) -> &'static str {
        match self {
            Self::Content => "",
            Self::Help => "Help",
            Self::Quit | Self::HelpQuit => "Quit",
            Self::HelpSuspend => "Suspend",
            Self::HelpClose => "Close",
        }
    }
}

/// One visible rectangle. Later entries are above earlier ones in hit-testing order.
#[derive(Clone, Copy, Debug)]
struct Target {
    control: Control,
    area: Rect,
}

/// Immutable geometry for a rendered frame; hidden controls are never in focus/hit order.
#[derive(Clone, Debug)]
pub(crate) struct LayoutSnapshot {
    pub area: Rect,
    pub generation: u64,
    pub help_open: bool,
    pub overlay: Option<Rect>,
    targets: Vec<Target>,
}

impl LayoutSnapshot {
    /// Derive geometry without querying the terminal or mutating application state.
    pub(crate) fn new(area: Rect, help: bool, generation: u64) -> Self {
        let mut layout = Self {
            area,
            generation,
            help_open: help,
            overlay: None,
            targets: Vec::new(),
        };
        if area.width < 40 || area.height < 8 {
            return layout;
        }
        if help {
            let width = area.width.min(72).saturating_sub(4);
            let height = area.height.min(16).saturating_sub(2);
            let overlay = Rect::new(
                area.x + (area.width - width) / 2,
                area.y + (area.height - height) / 2,
                width,
                height,
            );
            layout.overlay = Some(overlay);
            let y = overlay.bottom() - 2;
            layout.add(Control::HelpQuit, Rect::new(overlay.x + 2, y, 8, 1));
            layout.add(Control::HelpSuspend, Rect::new(overlay.x + 11, y, 11, 1));
            layout.add(Control::HelpClose, Rect::new(overlay.x + 23, y, 9, 1));
        } else {
            layout.add(Control::Help, Rect::new(area.right() - 17, area.y, 8, 1));
            layout.add(Control::Quit, Rect::new(area.right() - 8, area.y, 8, 1));
            layout.add(
                Control::Content,
                Rect::new(area.x, area.y + 1, area.width, area.height - 2),
            );
        }
        layout
    }

    /// Insert only nonempty visible targets, maintaining visual/focus order.
    fn add(&mut self, control: Control, area: Rect) {
        if area.width > 0 && area.height > 0 {
            self.targets.push(Target { control, area });
        }
    }

    /// Whether a control currently exists in the visible interaction surface.
    pub(crate) fn contains(&self, control: Control) -> bool {
        self.targets.iter().any(|target| target.control == control)
    }

    /// Cell hit-test uses exclusive right/bottom edges; overlays exclude the underlying targets.
    pub(crate) fn hit(&self, x: u16, y: u16) -> Option<Control> {
        self.targets
            .iter()
            .rev()
            .find(|target| {
                x >= target.area.x
                    && x < target.area.right()
                    && y >= target.area.y
                    && y < target.area.bottom()
            })
            .map(|target| target.control)
    }

    /// Cycle through only enabled visible controls; no targets yields no focus.
    pub(crate) fn next(&self, current: Option<Control>, backwards: bool) -> Option<Control> {
        let count = self.targets.len();
        if count == 0 {
            return None;
        }
        let index = self
            .targets
            .iter()
            .position(|target| Some(target.control) == current);
        let next = match index {
            None => {
                if backwards {
                    count - 1
                } else {
                    0
                }
            }
            Some(index) if backwards => (index + count - 1) % count,
            Some(index) => (index + 1) % count,
        };
        Some(self.targets[next].control)
    }
}

/// Render only from the snapshot also used for input routing.
pub(crate) fn render(frame: &mut Frame<'_>, app: &AppState, layout: &LayoutSnapshot) {
    let area = layout.area;
    if area.width < 40 || area.height < 8 {
        frame.render_widget(Paragraph::new("Resize to 40x8 | Ctrl+Q quit"), area);
        return;
    }
    frame.render_widget(
        Paragraph::new(format!("ede {}", env!("CARGO_PKG_VERSION"))),
        Rect::new(area.x, area.y, area.width - 18, 1),
    );
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 2);
    let content = if app.focus == Some(Control::Content) {
        "> No document open"
    } else {
        "  No document open"
    };
    frame.render_widget(Paragraph::new(content), body);
    let severity = match app.status.severity {
        Severity::Info => "",
        Severity::Warning => "Warning: ",
        Severity::Error => "Error: ",
    };
    let footer = format!("{severity}{}", app.status.text());
    frame.render_widget(
        Paragraph::new(footer),
        Rect::new(area.x, area.bottom() - 1, area.width - 22, 1),
    );
    frame.render_widget(
        Paragraph::new("F1 Help | Ctrl+Q Quit"),
        Rect::new(area.right() - 21, area.bottom() - 1, 21, 1),
    );

    if let Some(overlay) = layout.overlay {
        frame.render_widget(Clear, overlay);
        frame.render_widget(
            Block::default().title(" Help ").borders(Borders::ALL),
            overlay,
        );
        let text_area = Rect::new(
            overlay.x + 1,
            overlay.y + 1,
            overlay.width - 2,
            overlay.height - 3,
        );
        let text = if text_area.height >= BINDINGS.len() as u16 {
            BINDINGS
                .iter()
                .map(|binding| format!("{:<12} {}", binding.shortcut, binding.command.id()))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            "F1 Help | Ctrl+Q Quit | Esc Close\nTab/Shift+Tab Focus | Enter Run\nCtrl+S Save | Ctrl+C Copy".to_owned()
        };
        frame.render_widget(Paragraph::new(text), text_area);
    }
    for target in &layout.targets {
        if target.control == Control::Content {
            continue;
        }
        let focused = app.focus == Some(target.control);
        let label = if focused {
            format!(">{}<", target.control.label())
        } else {
            format!("[{}]", target.control.label())
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    /// Every dimension including zero is safe, and every hit target stays inside the frame.
    #[test]
    fn geometry_and_rendering_cover_tiny_and_normal_sizes() {
        for (width, height) in [(0, 0), (1, 1), (39, 7), (40, 8), (80, 24), (200, 100)] {
            for help in [false, true] {
                let layout = LayoutSnapshot::new(Rect::new(0, 0, width, height), help, 1);
                let mut app = AppState::new();
                app.help = help;
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
                assert_eq!(layout.hit(width, height), None);
            }
        }
    }

    /// Overlay geometry never exposes underlying buttons or content to focus or hit-testing.
    #[test]
    fn overlay_is_modal_and_edges_are_exclusive() {
        let area = Rect::new(0, 0, 80, 24);
        let base = LayoutSnapshot::new(area, false, 1);
        let target = base.targets[0];
        assert_eq!(base.hit(target.area.right(), target.area.y), None);
        let overlay = LayoutSnapshot::new(area, true, 2);
        assert_eq!(overlay.hit(target.area.x, target.area.y), None);
        assert!(!overlay.contains(Control::Content));
        assert_eq!(
            overlay.next(Some(Control::HelpQuit), true),
            Some(Control::HelpClose)
        );
    }
}
