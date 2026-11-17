//! Bounded recovery-list state and keyboard/mouse commands.

use super::*;
use crate::editor::RecoveryPage;

/// One catalog page with stable identity and a request generation for stale-reply rejection.
#[derive(Clone, Debug)]
pub(crate) struct Dialog {
    pub(crate) generation: u64,
    pub(crate) revision: u64,
    pub(crate) page: Arc<RecoveryPage>,
    pub(crate) highlight: usize,
    pub(crate) loading: bool,
}

impl AppState {
    pub(super) fn open_recovery(&mut self) -> Option<Intent> {
        self.picker_generation = self.picker_generation.wrapping_add(1);
        let generation = self.picker_generation;
        self.modal = Some(Modal::Recovery(Box::new(Dialog {
            generation,
            revision: 0,
            page: Arc::new(RecoveryPage::default()),
            highlight: 0,
            loading: true,
        })));
        self.focus = Some(Control::RecoveryList);
        self.capture = None;
        self.dirty = true;
        Some(Intent::Editor(Action::RecoveryPage {
            generation,
            cursor: None,
            previous: false,
        }))
    }

    pub(super) fn recovery_command(
        &mut self,
        command: Command,
        layout: &LayoutSnapshot,
    ) -> Option<Intent> {
        layout.overlay?;
        if let Some(Modal::DiscardRecovery(dialog, scratch)) = &self.modal {
            match command {
                Command::Tab | Command::FocusNext | Command::FocusPrevious => {
                    self.focus = layout.next(self.focus, command == Command::FocusPrevious);
                    self.dirty = true;
                }
                Command::Activate | Command::Newline => {
                    let (dialog, scratch) = (dialog.clone(), *scratch);
                    if self.focus == Some(Control::ConfirmDiscard) {
                        let generation = dialog.generation;
                        self.modal = Some(Modal::Recovery(dialog));
                        self.focus = Some(Control::RecoveryList);
                        self.dirty = true;
                        return Some(Intent::Editor(Action::DiscardRetained {
                            generation,
                            scratch,
                        }));
                    }
                    self.modal = Some(Modal::Recovery(dialog));
                    self.focus = Some(Control::RecoveryList);
                    self.dirty = true;
                }
                _ => {}
            }
            return None;
        }
        let Some(Modal::Recovery(dialog)) = &mut self.modal else {
            return None;
        };
        let requested = match command {
            Command::RecoveryPrevious => Some(Control::RecoveryPrevious),
            Command::RecoveryNext => Some(Control::RecoveryNext),
            Command::RecoveryOpen | Command::SelectRow => Some(Control::RecoveryList),
            Command::RecoveryDiscard => Some(Control::RecoveryDiscard),
            Command::RecoveryRetry => Some(Control::RecoveryRetry),
            _ => None,
        };
        if requested.is_some_and(|target| {
            !layout.contains(target)
                || matches!(command, Command::RecoveryDiscard | Command::RecoveryRetry)
                    && self.focus != Some(target)
        }) {
            return None;
        }
        match command {
            Command::Tab | Command::FocusNext | Command::FocusPrevious => {
                self.focus = layout.next(self.focus, command == Command::FocusPrevious);
                self.dirty = true;
            }
            Command::Move(movement, _) if !dialog.loading => {
                let rows = layout.list_rows().max(1) as isize;
                let delta = match movement {
                    Movement::Up => -1,
                    Movement::Down => 1,
                    Movement::PageUp => -rows,
                    Movement::PageDown => rows,
                    _ => return None,
                };
                dialog.highlight = dialog
                    .highlight
                    .saturating_add_signed(delta)
                    .min(dialog.page.rows.len().saturating_sub(1));
                dialog.revision = dialog.revision.wrapping_add(1);
                self.focus = Some(Control::RecoveryList);
                self.dirty = true;
            }
            Command::Activate
            | Command::Newline
            | Command::SelectRow
            | Command::RecoveryPrevious
            | Command::RecoveryNext
            | Command::RecoveryOpen
            | Command::RecoveryDiscard
            | Command::RecoveryRetry => {
                if requested.or(self.focus) == Some(Control::PathCancel) {
                    return self.command(Command::Cancel, layout);
                }
                if dialog.loading {
                    return None;
                }
                match requested.or(self.focus) {
                    Some(Control::RecoveryPrevious | Control::RecoveryNext) => {
                        let previous = requested.or(self.focus) == Some(Control::RecoveryPrevious);
                        if previous && !dialog.page.previous || !previous && !dialog.page.next {
                            return None;
                        }
                        let cursor = if previous {
                            dialog.page.before
                        } else {
                            dialog.page.after
                        };
                        self.picker_generation = self.picker_generation.wrapping_add(1);
                        dialog.generation = self.picker_generation;
                        dialog.revision = dialog.revision.wrapping_add(1);
                        dialog.loading = true;
                        dialog.highlight = 0;
                        self.capture = None;
                        self.dirty = true;
                        return Some(Intent::Editor(Action::RecoveryPage {
                            generation: dialog.generation,
                            cursor,
                            previous,
                        }));
                    }
                    Some(
                        Control::RecoveryList | Control::RecoveryOpen | Control::RecoveryRetry,
                    ) => {
                        let scratch = dialog.page.rows.get(dialog.highlight)?.id;
                        let generation = dialog.generation;
                        self.modal = None;
                        return self.tab_change(Action::OpenRetained {
                            generation,
                            scratch,
                        });
                    }
                    Some(Control::RecoveryDiscard) => {
                        let scratch = dialog.page.rows.get(dialog.highlight)?.id;
                        self.modal = Some(Modal::DiscardRecovery(dialog.clone(), scratch));
                        self.focus = Some(Control::ConfirmCancel);
                        self.capture = None;
                        self.dirty = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::RecoveryRow;
    use ratatui::layout::Rect;

    /// A failure dismissing Close also revokes the coordinator's removal intention.
    #[test]
    fn unrelated_failure_revokes_pending_close() {
        let mut app = AppState::new();
        app.expect_guard(17);
        assert!(matches!(
            app.event(Event::Failure("view I/O failed".into())),
            Some(Intent::Editor(Action::CancelGuard))
        ));
        assert!(
            app.event(Event::Proceed(17, After::Close(TabId(1))))
                .is_none()
        );
    }

    /// A late list reply cannot change another page or reopen a dismissed dialog.
    #[test]
    fn stale_recovery_reply_never_replaces_current_page() {
        let mut app = AppState::new();
        app.open_recovery();
        let generation = app.picker_generation;
        let page = Arc::new(RecoveryPage {
            rows: vec![RecoveryRow {
                id: crate::recovery::Id::fresh().unwrap(),
                label: "retained fixture".into(),
            }]
            .into(),
            ..RecoveryPage::default()
        });
        app.event(Event::RecoveryPage(generation + 1, page.clone()));
        assert!(matches!(&app.modal, Some(Modal::Recovery(dialog)) if dialog.page.rows.is_empty()));
        app.event(Event::RecoveryPage(generation, page.clone()));
        assert!(matches!(&app.modal, Some(Modal::Recovery(dialog)) if dialog.page.rows.len() == 1));
        let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 40, 8), &app, 1);
        app.command(Command::Cancel, &layout);
        app.event(Event::RecoveryPage(generation, page));
        assert!(app.modal.is_none());
    }

    /// Cancel is the safe default, and a resize cannot make a hidden Discard actionable.
    #[test]
    fn recovery_discard_defaults_cancel_and_requires_visible_controls() {
        let mut app = AppState::new();
        app.open_recovery();
        let generation = app.picker_generation;
        app.event(Event::RecoveryPage(
            generation,
            Arc::new(RecoveryPage {
                rows: vec![RecoveryRow {
                    id: crate::recovery::Id::fresh().unwrap(),
                    label: "fixture".into(),
                }]
                .into(),
                ..RecoveryPage::default()
            }),
        ));
        app.focus = Some(Control::RecoveryDiscard);
        let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 40, 8), &app, 1);
        app.command(Command::Activate, &layout);
        assert_eq!(app.focus, Some(Control::ConfirmCancel));
        let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 40, 8), &app, 2);
        assert!(app.command(Command::Activate, &layout).is_none());
        assert!(matches!(app.modal, Some(Modal::Recovery(_))));
        app.focus = Some(Control::RecoveryDiscard);
        let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 40, 8), &app, 3);
        app.command(Command::Activate, &layout);
        app.focus = Some(Control::ConfirmDiscard);
        let small = LayoutSnapshot::for_app(Rect::new(0, 0, 20, 4), &app, 4);
        assert!(app.command(Command::Activate, &small).is_none());
        assert!(matches!(app.modal, Some(Modal::DiscardRecovery(..))));
    }

    /// Retrying a failed Close must not turn it into a session Quit.
    #[test]
    fn failed_close_retry_preserves_requested_operation() {
        let mut app = AppState::new();
        app.event(Event::PersistenceFailed(
            TabId(2),
            After::Close(TabId(2)),
            "removal failed".into(),
        ));
        app.focus = Some(Control::RecoveryErrorRetry);
        let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 40, 8), &app, 1);
        assert!(matches!(
            app.command(Command::Activate, &layout),
            Some(Intent::Editor(Action::Guard(After::Close(TabId(2)))))
        ));
    }

    /// Saving after an inactive scratch's persistence failure targets that scratch, not the active tab.
    #[test]
    fn persistence_failure_export_retains_its_original_tab() {
        let mut app = AppState::new();
        app.screen.tab = Some(TabId(1));
        app.help = true;
        app.event(Event::PersistenceFailed(
            TabId(2),
            After::Quit,
            "disk unavailable".into(),
        ));
        assert!(!app.help, "a late persistence error owns the entire dialog");
        assert_eq!(app.focus, Some(Control::RecoveryErrorCancel));
        let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 40, 8), &app, 1);
        app.command(Command::SaveAs, &layout);
        assert!(matches!(app.modal, Some(Modal::Path(PathKind::Save, _))));
        assert_eq!(app.path_tab, Some(TabId(2)));
    }

    /// Retry refresh is reachable by keyboard from details at minimum geometry.
    #[test]
    fn details_retry_uses_the_named_tab() {
        let mut app = AppState::new();
        app.screen.tab = Some(TabId(7));
        app.screen.path = Some(PathBuf::from("/fixture"));
        app.modal = Some(Modal::Details("fixture".into()));
        app.focus = Some(Control::DetailsClose);
        let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 40, 8), &app, 1);
        app.command(Command::Tab, &layout);
        assert_eq!(app.focus, Some(Control::RetryRefresh));
        assert!(matches!(
            app.command(Command::Activate, &layout),
            Some(Intent::Editor(Action::RetryRefresh(TabId(7))))
        ));
    }
}
