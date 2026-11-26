//! Geometry and rendering for the floating Find and Find & Replace windows.
use super::*;
use crate::{
    app::search::{
        CLOSE, FORM_DOWN, FORM_UP, MINIMIZE, OPTIONS, RESTORE, SHOW_RESULTS, SUBMIT, display,
    },
    editor::search::{Summary, results::Row},
    settings::Scalar,
};
use ratatui::text::{Line, Span};

const MAX_WINDOW_WIDTH: u16 = 60;
/// Keep the two window actions visually and physically apart.
const TITLE_ACTION_GAP: u16 = 2;

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
}

impl Signature {
    pub(super) fn new(app: &AppState) -> Self {
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
                .filter(|rows| rows.count != 0)
                .map(|rows| (rows.generation, rows.start, rows.page.len(), rows.count)),
        }
    }
}

impl LayoutSnapshot {
    /// Add hit regions above the ordinary editor without resizing its document view.
    pub(super) fn add_search_window(&mut self, app: &AppState) {
        let width = self.area.width.min(MAX_WINDOW_WIDTH);
        let groups = app.search.groups(app.screen.search.busy);
        let form_rows = groups.len() as u16;
        let rows = app.search.current_rows(&app.screen.search.rows);
        let limit = app.settings.applied.values.get(Scalar::FindMatchRows);
        let desired_results = rows.map_or(0, |rows| rows.count.min(limit) as u16);
        // Keep three document rows visible, even at the minimum terminal size.
        let body_limit = self.content_height() - 5;
        let compact = form_rows + u16::from(desired_results > 0) > body_limit;
        let list_only = compact && app.search.list_open && desired_results > 0;
        let shown_form = if list_only {
            0
        } else {
            form_rows.min(body_limit)
        };
        let list_gap = u16::from(!compact && desired_results > 0 && body_limit > shown_form + 1);
        let shown_results = desired_results.min(body_limit - shown_form - list_gap);
        let spare = if compact {
            0
        } else {
            body_limit - shown_form - shown_results
        };
        let options_gap = u16::from(spare > list_gap);
        let navigation_gap = u16::from(spare > list_gap + options_gap);
        let height = 2 + shown_form + shown_results + list_gap + options_gap + navigation_gap;
        let window = Rect::new(
            self.area.right() - width,
            self.area.bottom() - 1 - height,
            width,
            height,
        );
        self.search_window = Some(window);
        self.add(Control::SearchSurface, window);
        let mut right = self.add_search_title_actions(app, window, MINIMIZE) - 1;
        let mut header = Vec::new();
        if compact {
            if desired_results > 0 {
                header.push(SHOW_RESULTS);
            }
            if !list_only && shown_form < form_rows {
                header.extend([FORM_DOWN, FORM_UP]);
            }
        }
        for index in header {
            let width = button_width(app, index);
            right -= width;
            self.add(Control::Search(index), Rect::new(right, window.y, width, 1));
            right -= 1;
        }
        let inner_x = window.x + 1;
        let inner_width = width - 2;
        let results = Rect::new(inner_x, window.y + 1, inner_width, shown_results);
        if let Some(rows) = rows.filter(|_| shown_results > 0) {
            self.search_list = Some(results);
            self.add(Control::SearchList, results);
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
        let capacity = usize::from(shown_form);
        let start = app.search.row.min(groups.len().saturating_sub(capacity));
        let mut y = results.bottom() + list_gap;
        for group in groups.iter().skip(start).take(capacity) {
            y += match group[0] {
                OPTIONS => options_gap,
                5 => navigation_gap,
                _ => 0,
            };
            self.add_search_group(app, group, Rect::new(inner_x, y, inner_width, 1));
            y += 1;
        }
    }

    /// Keep the expanded window's width and reserve one editor row for its title.
    pub(super) fn add_search_bar(&mut self, app: &AppState) {
        let width = self.area.width.min(MAX_WINDOW_WIDTH);
        let bar = Rect::new(self.area.right() - width, self.area.bottom() - 2, width, 1);
        if let Some(content) = self
            .targets
            .iter_mut()
            .find(|target| target.control == Control::Content)
        {
            content.area.height -= 1;
        }
        self.search_bar = Some(bar);
        self.add(Control::SearchSurface, bar);
        self.add_search_title_actions(app, bar, RESTORE);
    }

    fn add_search_title_actions(&mut self, app: &AppState, area: Rect, primary: u8) -> u16 {
        let close = button_width(app, CLOSE);
        let primary_width = button_width(app, primary);
        let close_x = area.right() - 1 - close;
        let primary_x = close_x - TITLE_ACTION_GAP - primary_width;
        self.add(Control::Search(CLOSE), Rect::new(close_x, area.y, close, 1));
        self.add(
            Control::Search(primary),
            Rect::new(primary_x, area.y, primary_width, 1),
        );
        primary_x
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
                let mut x = area.x + u16::from(matches!(group[0], 2 | 4 | 11)) * 2;
                if group[0] == 11 {
                    x += 7;
                }
                for &index in group {
                    let width = button_width(app, index);
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

fn divider(width: u16) -> Line<'static> {
    Line::styled(
        "─".repeat(usize::from(width)),
        Style::default().add_modifier(Modifier::DIM),
    )
}

/// Summarize the minimized search in the editor's existing status row.
pub(super) fn minimized_status(app: &AppState) -> Option<String> {
    let status = &app.screen.search;
    let rows = app.search.current_rows(&status.rows);
    let message = if status.busy {
        "Searching…".into()
    } else if status.message.starts_with("Cancelled") {
        "Cancelled".into()
    } else if status.message.contains("stale") || status.message.starts_with("Current tab changed")
    {
        "Stale results".into()
    } else if status.message.starts_with("Stopped")
        || rows.is_some_and(|rows| rows.error.is_some() || rows.feedback_incomplete)
    {
        "Incomplete".into()
    } else if status.outcomes.is_empty() || rows.is_none() {
        return None;
    } else {
        let count = status.rows.live;
        format!("{count} {}", if count == 1 { "match" } else { "matches" })
    };
    Some(message)
}

fn search_title(app: &AppState) -> &'static str {
    if app.search.replace {
        " Find & Replace "
    } else {
        " Find "
    }
}

/// Draw the window's title and its two actions on the reserved editor row.
pub(super) fn render_bar(
    frame: &mut Frame<'_>,
    app: &AppState,
    layout: &LayoutSnapshot,
    bar: Rect,
) {
    frame.render_widget(Clear, bar);
    frame.render_widget(
        Block::default()
            .title(search_title(app))
            .borders(Borders::TOP | Borders::LEFT | Borders::RIGHT),
        bar,
    );
    for target in &layout.targets {
        if let Control::Search(index @ (RESTORE | CLOSE)) = target.control {
            render_title_action(frame, app, target.area, index);
        }
    }
}

fn render_title_action(frame: &mut Frame<'_>, app: &AppState, area: Rect, index: u8) {
    if matches!(index, MINIMIZE | RESTORE) {
        frame.render_widget(Clear, Rect::new(area.right(), area.y, TITLE_ACTION_GAP, 1));
    }
    let label = app.search.label(index);
    let focused = app.focus == Some(Control::Search(index));
    let text = if focused {
        format!(">{label}<")
    } else {
        format!("[{label}]")
    };
    let color = match index {
        CLOSE => Some(Color::Red),
        MINIMIZE | RESTORE => Some(Color::LightBlue),
        _ => None,
    };
    frame.render_widget(
        Paragraph::new(text).style(button_style(color, focused)),
        area,
    );
}

/// Escape individual values and add genuine terminal rows for sections.
pub(super) fn details(report: &Summary, width: u16) -> Paragraph<'static> {
    let heading = Style::default().add_modifier(Modifier::BOLD);
    let mut lines = vec![Line::styled(safe(&report.headline), heading)];
    for (label, rows) in [
        ("Tabs", &report.tabs),
        ("Replacement", &report.replacements),
        ("Notices", &report.notices),
    ] {
        if !rows.is_empty() {
            lines.push(divider(width));
            lines.push(Line::styled(label, heading));
            lines.extend(rows.iter().map(|row| Line::from(safe(row))));
        }
    }
    Paragraph::new(lines).wrap(Wrap { trim: false })
}

/// Paint the shared geometry in terminal cells; the same regions receive pointer events.
pub(super) fn render(frame: &mut Frame<'_>, app: &AppState, layout: &LayoutSnapshot, window: Rect) {
    let status = &app.screen.search;
    let rows = app.search.current_rows(&status.rows);
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
    frame.render_widget(
        Block::default()
            .title(search_title(app))
            .borders(Borders::ALL),
        window,
    );
    if let Some(list) = layout.search_list
        && layout
            .targets
            .iter()
            .any(|target| target.control == Control::Search(0) && target.area.y > list.bottom())
    {
        frame.render_widget(
            Paragraph::new(divider(list.width)),
            Rect::new(list.x, list.bottom(), list.width, 1),
        );
    }
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
            Control::Search(index @ (MINIMIZE | CLOSE)) => {
                render_title_action(frame, app, target.area, index);
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
                    && label.width() + usize::from(focused) > target.area.width as usize
                {
                    format!(
                        "› Options {} C:{} W:{}",
                        app.search.scope_label(),
                        if app.search.query.match_case {
                            "on"
                        } else {
                            "off"
                        },
                        if app.search.query.whole_word {
                            "on"
                        } else {
                            "off"
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

    /// Long reports scroll to their last wrapped row and return to the headline.
    #[test]
    fn search_details_scroll_to_both_ends_and_escape_values() {
        use crate::{command::Movement, input::Input};
        let mut app = AppState::new();
        let mut tabs = vec!["tab\nname · 1 match · Complete".into()];
        tabs.extend((2..=24).map(|i| format!("tab {i} · 1 match · Complete")));
        app.modal = Some(Modal::SearchDetails(Summary {
            headline: "24 matches".into(),
            tabs,
            replacements: vec![],
            notices: vec![],
        }));
        app.focus = Some(Control::DetailsClose);
        let mut terminal = Terminal::new(TestBackend::new(65, 14)).unwrap();
        let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 65, 14), &app, 1);
        let screen = |terminal: &mut Terminal<TestBackend>, app: &AppState| {
            terminal
                .draw(|frame| super::super::render(frame, app, &layout))
                .unwrap();
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        let start = screen(&mut terminal, &app);
        assert!(start.contains("24 matches") && start.contains("tab<U+000A>name"));

        app.input(
            Input::Command(Command::Move(Movement::PageDown, false)),
            &layout,
        );
        assert_eq!(app.help_scroll, layout.overlay.unwrap().height - 3);
        app.input(Input::Command(Command::Move(Movement::End, false)), &layout);
        app.input(
            Input::Command(Command::Move(Movement::Down, false)),
            &layout,
        );
        assert_eq!(app.help_scroll, layout.search_details_scroll_limit);
        assert!(screen(&mut terminal, &app).contains("tab 24 · 1 match · Complete"));
        app.input(
            Input::Command(Command::Move(Movement::Home, false)),
            &layout,
        );
        assert!(screen(&mut terminal, &app).contains("24 matches"));
    }

    /// An empty-page refresh cannot cancel a press on an unchanged form control.
    #[test]
    fn only_result_pages_with_rows_change_the_hit_signature() {
        let mut app = AppState::new();
        app.search.open = true;
        let empty = Signature::new(&app);
        std::sync::Arc::make_mut(&mut app.screen.search)
            .rows
            .generation += 1;
        assert_eq!(Signature::new(&app), empty);

        std::sync::Arc::make_mut(&mut app.screen.search).rows.count = 1;
        let visible = Signature::new(&app);
        assert_ne!(visible, empty);
        std::sync::Arc::make_mut(&mut app.screen.search)
            .rows
            .generation += 1;
        assert_ne!(Signature::new(&app), visible);
    }

    /// Collapsing keeps the window edge, title, and controls on a single row.
    #[test]
    fn minimized_search_matches_the_expanded_title_row() {
        for (width, height) in [(80, 24), (40, 8)] {
            let area = Rect::new(0, 0, width, height);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            for (replace, title) in [(false, "Find"), (true, "Find & Replace")] {
                let mut app = AppState::new();
                app.notify(crate::status::StatusMessage::new(
                    crate::status::Severity::Info,
                    "Ready",
                ));
                app.search.open = true;
                app.search.replace = replace;
                let expanded = LayoutSnapshot::for_app(area, &app, 1);
                let window = expanded.search_bounds().unwrap();
                let column = |layout: &LayoutSnapshot, index| {
                    layout
                        .targets
                        .iter()
                        .find(|t| t.control == Control::Search(index))
                        .unwrap()
                        .area
                        .x
                };
                app.minimize_search();
                std::sync::Arc::make_mut(&mut app.screen.search).busy = true;
                let collapsed = LayoutSnapshot::for_app(area, &app, 2);
                let bar = collapsed.search_bar.unwrap();
                assert_eq!(bar, Rect::new(window.x, area.bottom() - 2, window.width, 1));
                assert_eq!(column(&expanded, MINIMIZE), column(&collapsed, RESTORE));
                assert_eq!(column(&expanded, CLOSE), column(&collapsed, CLOSE));
                terminal
                    .draw(|frame| super::super::render(frame, &app, &collapsed))
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let row: String = (bar.x..bar.right())
                    .map(|x| buffer[(x, bar.y)].symbol())
                    .collect();
                assert!(row.starts_with(&format!("┌ {title} ─")), "{row:?}");
                assert!(row.ends_with("[+]  [×]┐"), "{row:?}");
                assert!(!row.contains("Searching"));
                let status: String = (0..width)
                    .map(|x| buffer[(x, height - 1)].symbol())
                    .collect();
                assert!(status.contains("Searching…"), "{status:?}");
                let expand_x = column(&collapsed, RESTORE);
                assert_eq!(buffer[(expand_x, bar.y)].fg, Color::LightBlue);
                assert_eq!(buffer[(column(&collapsed, CLOSE), bar.y)].fg, Color::Red);
                for x in expand_x + 3..column(&collapsed, CLOSE) {
                    assert_eq!(collapsed.hit(x, bar.y), Some(Control::SearchSurface));
                }
            }
        }
    }

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
        let minimize = target(&layout, MINIMIZE);
        let close = target(&layout, CLOSE);
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
        assert_eq!(buffer[(minimize.x, minimize.y)].fg, Color::LightBlue);
        assert_eq!(buffer[(close.x, close.y)].fg, Color::Red);
        assert_eq!(close.x - minimize.right(), TITLE_ACTION_GAP);
        for x in minimize.right()..close.x {
            assert_eq!(buffer[(x, close.y)].symbol(), " ");
            assert_eq!(layout.hit(x, close.y), Some(Control::SearchSurface));
        }
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
