//! Geometry and rendering for the floating Find and Find & Replace windows.
use super::*;
use crate::{
    app::search::{FORM_DOWN, FORM_UP, OPTIONS, SHOW_RESULTS, SUBMIT, display},
    editor::search::results::{PAGE_SIZE, Row},
};
use ratatui::text::{Line, Span};

/// Everything that can change a search click target between press and release.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Signature {
    replace: bool,
    expanded: bool,
    all: bool,
    busy: bool,
    form_row: usize,
    list_open: bool,
    list_top: u64,
    page: Option<(u64, u64, usize, u64)>,
    caret: Option<(u16, u16)>,
    highlight: Option<(u16, u16)>,
}

impl Signature {
    pub(super) fn new(app: &AppState) -> Self {
        let current = app
            .screen
            .search
            .current
            .as_ref()
            .zip(app.screen.view.as_ref())
            .filter(|(found, view)| {
                Some(found.tab) == app.screen.tab && found.revision == view.revision
            });
        let highlight = current.and_then(|(found, view)| {
            let edge = u64::from(view.width.saturating_sub(60)) + view.left;
            let mut rows = view.rows.iter().enumerate().filter(|(_, row)| {
                row.spans.iter().any(|span| {
                    span.bytes.start < found.range.end
                        && span.bytes.end > found.range.start
                        && columns_for_range(span, &found.range).end > edge
                })
            });
            let first = rows.next()?.0 as u16;
            Some((
                first,
                rows.next_back().map_or(first, |(last, _)| last as u16),
            ))
        });
        Self {
            replace: app.search.replace,
            expanded: app.search.expanded,
            all: app.search.all,
            busy: app.screen.search.busy,
            form_row: app.search.row,
            list_open: app.search.list_open,
            list_top: app.search.result_top,
            page: app
                .search
                .current_rows(&app.screen.search.rows)
                .map(|rows| (rows.generation, rows.start, rows.page.len(), rows.count)),
            caret: current.and_then(|(_, view)| view.cursor),
            highlight,
        }
    }
}

impl LayoutSnapshot {
    /// Add hit regions above the ordinary editor without resizing its document view.
    pub(super) fn add_search_window(&mut self, app: &AppState) {
        let width = self.area.width.min(60);
        let height = ((self.area.height - 2) / 2).min(PAGE_SIZE as u16 + 2);
        let mut window = Rect::new(self.area.right() - width, self.area.y + 1, width, height);
        let lower = self.area.height - height - 2;
        if let Some(signature) = &self.signature.search {
            let caret = signature
                .caret
                .filter(|(x, _)| *x >= self.area.width - width);
            if caret.is_some_and(|(_, y)| y < height)
                || (caret.is_none_or(|(_, y)| y < lower)
                    && signature
                        .highlight
                        .is_some_and(|(first, last)| first < height && last < lower))
            {
                window.y = self.area.y + 1 + lower;
            }
        }
        self.search_window = Some(window);
        self.add(Control::SearchSurface, window);
        let body = Rect::new(window.x + 1, window.y + 1, width - 2, height - 2);
        let groups = app.search.groups(app.screen.search.busy);
        let compact = body.height as usize <= groups.len();
        let mut right = window.right() - 1;
        let mut header = vec![10];
        if compact {
            header.push(SHOW_RESULTS);
            if !app.search.list_open {
                header.extend([FORM_DOWN, FORM_UP]);
            }
        }
        for index in header {
            let width = button_width(app, index);
            right -= width;
            self.add(Control::Search(index), Rect::new(right, window.y, width, 1));
            right -= 1;
        }
        let mut form_height = 0;
        if !compact || !app.search.list_open {
            let capacity = usize::from(body.height);
            let start = if compact {
                app.search.row.min(groups.len().saturating_sub(capacity))
            } else {
                0
            };
            for (row, group) in groups.iter().skip(start).take(capacity).enumerate() {
                self.add_search_group(
                    app,
                    group,
                    Rect::new(body.x, body.y + row as u16, body.width, 1),
                );
                form_height += 1;
            }
        }
        let results = Rect::new(
            body.x,
            body.y + form_height,
            body.width,
            body.height - form_height,
        );
        if results.height == 0 {
            return;
        }
        self.search_list = Some(results);
        self.add(Control::SearchList, results);
        if let Some(rows) = app.search.current_rows(&app.screen.search.rows) {
            let top = app.search.result_offset(rows, results.height);
            for offset in 0..u64::from(results.height) {
                let index = top + offset;
                if index >= rows.start && index - rows.start < rows.page.len() as u64 {
                    self.add(
                        Control::SearchResult(index),
                        Rect::new(results.x, results.y + offset as u16, results.width, 1),
                    );
                }
            }
        }
    }

    fn add_search_group(&mut self, app: &AppState, group: &[u8], area: Rect) {
        let add = |layout: &mut Self, index, x, width| {
            layout.add(Control::Search(index), Rect::new(x, area.y, width, 1));
        };
        match group {
            [0, SUBMIT] => {
                let button = button_width(app, SUBMIT);
                add(self, 0, area.x, area.width - button - 1);
                add(self, SUBMIT, area.right() - button, button);
            }
            [1] | [OPTIONS] => add(self, group[0], area.x, area.width),
            [7, 8] => {
                let all = button_width(app, 8);
                let next = button_width(app, 7);
                add(self, 7, area.right() - all - next - 1, next);
                add(self, 8, area.right() - all, all);
            }
            _ => {
                let mut x = area.x + u16::from(matches!(group[0], 2 | 11)) * 2;
                if group[0] == 11 {
                    x += 7;
                }
                for &index in group {
                    let width = button_width(app, index);
                    if index == 4 {
                        x = area.right() - width;
                    }
                    add(self, index, x, width.min(area.right().saturating_sub(x)));
                    x += width + 1;
                }
            }
        }
    }
}

fn button_width(app: &AppState, index: u8) -> u16 {
    app.search.label(index).width() as u16 + if matches!(index, 2 | 3) { 0 } else { 2 }
}

/// Paint the shared geometry in terminal cells; the same regions receive pointer events.
pub(super) fn render(frame: &mut Frame<'_>, app: &AppState, layout: &LayoutSnapshot, window: Rect) {
    let status = &app.screen.search;
    let rows = app.search.current_rows(&status.rows);
    let title = if app.search.replace {
        " Find & Replace "
    } else {
        " Find "
    };
    let nav = layout
        .targets
        .iter()
        .find(|target| target.control == Control::Search(14));
    let ordinal = rows.filter(|rows| rows.live != 0).map(|rows| {
        let matches = if rows.live == 1 { "match" } else { "matches" };
        if status.busy {
            format!("{} {matches} so far", rows.live)
        } else {
            rows.ordinal.map_or_else(
                || format!("{} {matches}", rows.live),
                |ordinal| format!("{ordinal} of {} {matches}", rows.live),
            )
        }
    });
    let message = if let Some(error) = &app.field_error {
        error.clone()
    } else if let Some(error) = rows.and_then(|rows| rows.error.as_ref()) {
        error.clone()
    } else if rows.is_some_and(|rows| rows.feedback_incomplete) {
        "Replacement feedback incomplete · see Summary".into()
    } else if status.busy {
        format!(
            "{} · {}/{} bytes",
            status.message, status.progress.scanned, status.progress.total
        )
    } else if rows.is_none() {
        "Press Find".into()
    } else if nav.is_none()
        && layout.search_result_rows() != 0
        && status.current.is_some()
        && status
            .outcomes
            .iter()
            .all(|outcome| outcome.message == "Complete")
        && let Some(ordinal) = &ordinal
    {
        ordinal.clone()
    } else {
        status.message.clone()
    };
    frame.render_widget(Clear, window);
    frame.render_widget(Block::default().title(title).borders(Borders::ALL), window);
    if !message.is_empty() {
        frame.render_widget(
            Paragraph::new(format!(" {message} ")),
            Rect::new(window.x + 1, window.bottom() - 1, window.width - 2, 1),
        );
    }

    if let (Some(label), Some(nav)) = (ordinal, nav) {
        let width = label.width() as u16;
        let right = window.right() - 2;
        if nav.area.right() < right.saturating_sub(width) {
            frame.render_widget(
                Paragraph::new(label),
                Rect::new(right - width, nav.area.y, width, 1),
            );
        }
    }

    for target in &layout.targets {
        let focused = app.focus == Some(target.control);
        match target.control {
            Control::Search(index @ (0 | 1)) => {
                let text = if index == 0 {
                    &app.search.query.literal
                } else {
                    &app.search.replacement
                };
                let cursor = app.search.field_cursor(index, text);
                let prefix = if index == 0 { "Find: " } else { "Replace: " };
                let before = tail_cells(
                    &display(&text[..cursor]),
                    target.area.width.saturating_sub(prefix.len() as u16 + 2),
                );
                let label = format!(
                    "{}{prefix}{before}{}",
                    if focused { '>' } else { ' ' },
                    display(&text[cursor..])
                );
                let color = focused.then_some(Color::Cyan);
                frame.render_widget(
                    Paragraph::new(label).style(button_style(color, focused)),
                    target.area,
                );
                if focused {
                    let x = (prefix.len() + before.width() + 1) as u16;
                    frame.set_cursor_position((
                        target.area.x + x.min(target.area.width - 1),
                        target.area.y,
                    ));
                }
            }
            Control::Search(index) => {
                if index == 11 {
                    frame.render_widget(
                        Paragraph::new("Insert "),
                        Rect::new(target.area.x - 7, target.area.y, 7, 1),
                    );
                }
                let label = app.search.label(index);
                let label = if index == OPTIONS
                    && !app.search.expanded
                    && label.width() > target.area.width as usize
                {
                    format!(
                        "› Options Case:{} Word:{} {}",
                        if app.search.query.match_case {
                            "on"
                        } else {
                            "off"
                        },
                        if app.search.query.whole_word {
                            "on"
                        } else {
                            "off"
                        },
                        if app.search.all {
                            "All open"
                        } else {
                            "Current"
                        }
                    )
                } else {
                    label
                };
                let text = match index {
                    OPTIONS if focused => format!(">{label}"),
                    OPTIONS => label,
                    2 | 3 => label,
                    _ if focused => format!(">{label}<"),
                    _ => format!("[{label}]"),
                };
                let color = match index {
                    SUBMIT => Some(Color::Green),
                    7 => Some(Color::Cyan),
                    8 => Some(Color::Yellow),
                    _ if focused => Some(Color::Cyan),
                    _ => None,
                };
                let mut style = button_style(color, focused);
                if app.search.query.literal.is_empty() && matches!(index, 5..=8 | SUBMIT) {
                    style = style.add_modifier(Modifier::DIM);
                }
                if focused && matches!(index, 2 | 3) {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                frame.render_widget(Paragraph::new(text).style(style), target.area);
            }
            Control::SearchResult(index) => {
                if let Some(rows) = rows
                    && let Some(row) = rows.page.get(index.saturating_sub(rows.start) as usize)
                {
                    render_row(
                        frame,
                        app,
                        row,
                        target.area,
                        rows.current == Some(index),
                        matches!(
                            app.focus,
                            Some(Control::SearchList | Control::SearchResult(_))
                        ) && app.search.result_focus == Some(index),
                    );
                }
            }
            _ => {}
        }
    }
}

fn render_row(
    frame: &mut Frame<'_>,
    app: &AppState,
    row: &Row,
    area: Rect,
    current: bool,
    focused: bool,
) {
    let mut prefix = if current || focused {
        "> ".to_owned()
    } else {
        "  ".to_owned()
    };
    if let Some(change) = row.change {
        prefix.push_str(change);
        prefix.push_str(" · ");
    }
    prefix.push_str(&format!("{}:{} ", row.line, row.column));
    if app.search.all
        && let Some(tab) = app.screen.tabs.iter().find(|tab| tab.id == row.tab)
    {
        let duplicate = app
            .screen
            .tabs
            .iter()
            .any(|other| other.id != row.tab && other.label == tab.label);
        let label = safe(tab.label.as_ref());
        prefix.push_str(&tail_cells(&label, (area.width / 3).max(6)));
        if duplicate {
            prefix.push_str(&format!(" #{}", tab.id.0));
        }
        prefix.push_str("  ");
    }
    let style = if current || focused {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(prefix),
            Span::raw(&row.before),
            Span::styled(
                &row.matched,
                Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            ),
            Span::raw(&row.after),
        ]))
        .style(style),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::search::Operation;
    use ratatui::{Terminal, backend::TestBackend};

    /// Search action colors and alignment come from the same regions used for clicks.
    #[test]
    fn search_action_colors_and_right_edge() {
        let mut app = AppState::new();
        app.search.open = true;
        app.search.replace = true;
        app.search.query.literal = "apple".into();
        let area = Rect::new(0, 0, 80, 24);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let layout = LayoutSnapshot::for_app(area, &app, 1);
        let target = |layout: &LayoutSnapshot, index| {
            layout
                .targets
                .iter()
                .find(|target| target.control == Control::Search(index))
                .unwrap()
                .area
        };
        let find = target(&layout, SUBMIT);
        let next = target(&layout, 7);
        let all = target(&layout, 8);
        assert_eq!(find.right(), all.right());
        assert_eq!(next.y, target(&layout, 1).y + 1);
        assert_eq!(all.y, next.y);
        assert!(target(&layout, OPTIONS).y > all.y);
        terminal
            .draw(|frame| super::super::render(frame, &app, &layout))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(find.x, find.y)].fg, Color::Green);
        assert_eq!(buffer[(next.x, next.y)].fg, Color::Cyan);
        assert_eq!(buffer[(all.x, all.y)].fg, Color::Yellow);

        app.modal = Some(Modal::ReplaceAll(app.search.request(Operation::ReplaceAll)));
        app.focus = Some(Control::Search(16));
        let layout = LayoutSnapshot::for_app(area, &app, 2);
        let confirm = target(&layout, 15);
        terminal
            .draw(|frame| super::super::render(frame, &app, &layout))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer()[(confirm.x, confirm.y)].fg,
            Color::Yellow
        );
    }
}
