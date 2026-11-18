//! Single-reader event loop with monotonic frame scheduling and bounded background work.

use std::{
    io,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    editor::{Action, After, ClipboardTransfer, Editor},
    ingress::Ingress,
};
use ratatui::{Terminal, TerminalOptions, Viewport, backend::CrosstermBackend, layout::Rect};
use std::path::PathBuf;

use crate::{
    Exit,
    app::{AppState, Intent},
    input::Input,
    status::{Severity, StatusInbox, StatusMessage},
    terminal::{ClipboardWrite, Signals, TerminalOutput, TerminalSession, UnixTerminal},
    ui::{self, LayoutSnapshot},
};

/// Routine frames are limited to approximately 60 Hz; idle input waits never exceed 20 ms.
const FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667);
/// Upper bound on signal latency caused by waiting for a terminal event.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Runtime effects and time, separated for deterministic loop tests.
trait Host {
    /// Monotonic time since this session began.
    fn now(&self) -> Duration;
    /// Poll/read from the sole event source; unknown events may yield None.
    fn input(&mut self, timeout: Duration) -> io::Result<Option<Input>>;
    /// Draw the supplied immutable geometry and state.
    fn draw(&mut self, app: &AppState, layout: &LayoutSnapshot) -> io::Result<()>;
    /// Obtain current cell dimensions, including after resume.
    fn area(&self) -> io::Result<Rect>;
    /// Take an external termination request, if present.
    fn termination(&self) -> Option<u8>;
    /// Take a request to suspend from an external signal.
    fn suspend_requested(&self) -> bool;
    /// Take a SIGCONT notification requiring a fresh frame.
    fn continued(&self) -> bool;
    /// Take a paste cancellation that arrived outside the terminal byte stream.
    fn cancel_paste_requested(&self) -> bool {
        false
    }
    /// Restore/suspend/reacquire, returning false when job control is unavailable.
    fn suspend(&mut self) -> io::Result<bool>;
    /// Clear backend state so the next frame fully redraws after continuation.
    fn clear(&mut self) -> io::Result<()>;
    /// Acquire an enhanced keyboard mode after asynchronous capability detection.
    fn keyboard(&mut self) -> io::Result<()> {
        Ok(())
    }
    /// Toggle mouse reporting without altering the user's terminal settings.
    fn mouse(&mut self, _enabled: bool) -> io::Result<()> {
        Ok(())
    }
    /// Send a prepared terminal clipboard frame beside normal drawing.
    fn clipboard(&mut self, _transfer: &ClipboardTransfer) -> ClipboardWrite {
        ClipboardWrite::Unavailable
    }
}

/// Concrete session adapter; backend rendering and lifecycle ownership remain separate.
struct TerminalHost<'a> {
    terminal: Terminal<CrosstermBackend<TerminalOutput>>,
    session: &'a mut TerminalSession<UnixTerminal>,
    signals: &'a Signals,
    started: Instant,
    ingress: Ingress,
}

impl Host for TerminalHost<'_> {
    /// Use an elapsed monotonic clock rather than wall time.
    fn now(&self) -> Duration {
        self.started.elapsed()
    }
    /// Receive normalized events from the sole bounded terminal reader.
    fn input(&mut self, timeout: Duration) -> io::Result<Option<Input>> {
        self.ingress.input(timeout)
    }
    /// Render the same geometry later used for mouse routing.
    fn draw(&mut self, app: &AppState, layout: &LayoutSnapshot) -> io::Result<()> {
        if self.terminal.get_frame().area() != layout.area {
            self.terminal.resize(layout.area)?;
        }
        self.terminal.draw(|frame| ui::render(frame, app, layout))?;
        Ok(())
    }
    /// Read the backend dimensions without capability queries.
    fn area(&self) -> io::Result<Rect> {
        let (width, height) = self.session.driver().size()?;
        Ok(Rect::new(0, 0, width, height))
    }
    /// Forward signal flags without signal-handler I/O.
    fn termination(&self) -> Option<u8> {
        self.signals.termination()
    }
    /// Forward a pending job-control request.
    fn suspend_requested(&self) -> bool {
        self.signals.suspend_requested()
    }
    /// Forward a continuation notification.
    fn continued(&self) -> bool {
        self.signals.continued()
    }
    fn cancel_paste_requested(&self) -> bool {
        self.signals.cancel_paste_requested()
    }
    /// Restore and stop outside the reducer and signal handler.
    fn suspend(&mut self) -> io::Result<bool> {
        self.session.suspend()
    }
    /// Enhanced input state belongs to the same lifecycle guard as raw/mouse modes.
    fn keyboard(&mut self) -> io::Result<()> {
        self.session.set_keyboard()
    }
    /// Preserve capture choice across suspension and restore it on exit.
    fn mouse(&mut self, enabled: bool) -> io::Result<()> {
        self.session.set_mouse(enabled)
    }
    fn clipboard(&mut self, transfer: &ClipboardTransfer) -> ClipboardWrite {
        self.session
            .clipboard(transfer, || self.signals.output_interrupted())
    }
    /// Invalidate Ratatui's previous buffer following re-entry.
    fn clear(&mut self) -> io::Result<()> {
        // Terminal::clear() queries the cursor position in Ratatui 0.30.2. A fullscreen
        // resize invalidates both buffers without a terminal round trip, even at the same size.
        let area = self.area()?;
        self.terminal.resize(area)
    }
}

/// Resources and preferences captured at the validated launch boundary.
pub(crate) struct Launch {
    pub directory: PathBuf,
    pub documents: crate::stream::Startup,
    pub recovery: crate::recovery::Store,
    pub syntax_program: Option<PathBuf>,
    pub desktop_clipboard: Option<Arc<crate::clipboard::Backend>>,
    pub config: crate::settings::Location,
}

/// Run with explicitly acquired modes; Ratatui initialization helpers are not used.
pub(crate) fn run(
    session: &mut TerminalSession<UnixTerminal>,
    signals: &Signals,
    inbox: StatusInbox,
    launch: Launch,
) -> io::Result<Exit> {
    let (width, height) = session.driver().size()?;
    // The event loop owns resize/layout changes. A fixed viewport prevents Ratatui
    // independently resizing between snapshot construction, drawing, and hit-testing.
    let terminal = Terminal::with_options(
        CrosstermBackend::new(session.driver().output()?),
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, width, height)),
        },
    )?;
    let picker = crate::picker::Picker::new()?;
    let ingress = Ingress::new(session.driver().input()?)?;
    let mut host = TerminalHost {
        terminal,
        session,
        signals,
        started: Instant::now(),
        ingress,
    };
    let mut editor = Editor::new(
        launch.directory.clone(),
        launch.documents,
        launch.recovery,
        launch.syntax_program,
        launch
            .config
            .user_directory()
            .map(std::path::Path::to_owned),
    )?;
    let highlighting = editor.highlighting_process();
    let mut app = AppState::new();
    app.set_launch_directory(launch.directory);
    app.desktop_clipboard = launch.desktop_clipboard;
    app.config_location = launch.config;
    app.settings.startup(&app.config_location);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        event_loop_impl(&mut host, inbox, Some(&mut editor), Some(&picker), &mut app)
    }));
    app.revoke_clipboard();
    let settings_exit = app.settings.exit(&app.config_location);
    host.ingress.stop();
    let streamed = editor.streamed();
    let worker = editor.shutdown();
    let picker_worker = picker.shutdown();
    let restoration = host.session.restore();
    let output_lost = host.session.output_lost();
    let bounded_cleanup = streamed || output_lost;
    highlighting.pause();
    if !bounded_cleanup {
        host.ingress.join();
    }
    drop(host);
    let deadline = Instant::now() + Duration::from_millis(250);
    for worker in [worker, picker_worker].into_iter().flatten() {
        if bounded_cleanup {
            while !worker.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        if !bounded_cleanup || worker.is_finished() {
            let _ = worker.join();
        }
    }
    if let Some(message) = settings_exit.or_else(|| app.settings.exit(&app.config_location))
        && !output_lost
    {
        eprintln!("ede: {message}");
    }
    match result {
        Ok(result) => {
            restoration?;
            result
        }
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

/// Own UI state, checking signals between individual events even during an input flood.
fn event_loop_impl(
    host: &mut impl Host,
    inbox: StatusInbox,
    mut editor: Option<&mut Editor>,
    picker: Option<&crate::picker::Picker>,
    app: &mut AppState,
) -> io::Result<Exit> {
    let mut area = host.area()?;
    let mut layout = LayoutSnapshot::new(area, app.help, 0);
    let mut rendered_generation = None;
    let mut last_draw = None;
    let mut requested_geometry = None;
    let mut drag_scroll = false;

    loop {
        if let Some(signal) = host.termination() {
            return Ok(Exit { code: 128 + signal });
        }
        if host.cancel_paste_requested()
            && !app.cancel_receiving_paste()
            && let Some(bridge) = editor.as_deref()
        {
            bridge.cancel_paste();
        }
        if let Some(bridge) = editor.as_deref() {
            bridge.receiving(app.receiving_paste_tab());
        }
        if let Some(bridge) = editor.as_deref_mut() {
            if bridge.failed() {
                return Err(io::Error::other(
                    "Editor worker stopped unexpectedly; terminal restored on exit",
                ));
            }
            if let Some(screen) = bridge.screen() {
                let pending = bridge.pending(screen.acknowledged);
                app.update(screen, pending);
                if drag_scroll && !pending {
                    drag_scroll = false;
                    if let Some(intent) = app.drag_hit(&layout)
                        && effect(intent, host, app, Some(bridge), picker)?
                    {
                        return Ok(Exit { code: 0 });
                    }
                }
            }
            for _ in 0..16 {
                let Ok(event) = bridge.events.try_recv() else {
                    break;
                };
                let had_surface = app.modal.is_some() || app.help;
                let intent = app.event(event);
                if !had_surface && (app.modal.is_some() || app.help) {
                    bridge.revoke_search_navigation();
                    app.revoke_clipboard_target();
                }
                if let Some(intent) = intent
                    && effect(intent, host, app, Some(bridge), picker)?
                {
                    return Ok(Exit { code: 0 });
                }
            }
        }
        if let Some(reply) = picker.and_then(crate::picker::Picker::poll)
            && let Some(intent) = app.picker_reply(reply)
            && effect(intent, host, app, editor.as_deref_mut(), picker)?
        {
            return Ok(Exit { code: 0 });
        }
        app.poll_settings(editor.as_deref_mut());
        if let Some(signal) = host.termination() {
            return Ok(Exit { code: 128 + signal });
        }
        if host.suspend_requested() {
            app.request_suspend();
        }
        if app.suspend_ready() {
            suspend(host, app, editor.as_deref())?;
            area = host.area()?;
            rendered_generation = None;
        }
        if host.continued() {
            app.revoke_clipboard();
            host.clear()?;
            area = host.area()?;
            app.input(Input::Resize(area.width, area.height), &layout);
            rendered_generation = None;
        }
        // A producer refilling the queue cannot starve input or signals.
        for _ in 0..64 {
            let Some(message) = inbox.pop() else {
                break;
            };
            app.notify(message);
        }
        if let Some(bridge) = editor.as_deref()
            && let Some(transfer) = bridge.take_clipboard(app.screen.acknowledged)
            && deliver_clipboard(host, app, transfer) == ClipboardWrite::Lost
        {
            return Ok(Exit {
                code: host.termination().map_or(1, |signal| 128 + signal),
            });
        }
        reconcile_layout(area, app, &mut layout);
        if let Some(intent) = app.tick(host.now(), &layout) {
            drag_scroll = true;
            if effect(intent, host, app, editor.as_deref_mut(), picker)? {
                return Ok(Exit { code: 0 });
            }
        }
        // A timer can submit a pasted path and close its dialog. Reconcile that
        // change before drawing or reading another key, just like input effects.
        reconcile_layout(area, app, &mut layout);
        let geometry = (
            area.width,
            layout.content_height(),
            layout.document_generation,
        );
        if requested_geometry != Some(geometry) {
            if let Some(bridge) = editor.as_deref_mut() {
                if bridge.submit(Action::Size(geometry.0, geometry.1, geometry.2)) {
                    requested_geometry = Some(geometry);
                    app.pending = true;
                }
            } else {
                requested_geometry = Some(geometry);
            }
        }
        let now = host.now();
        let elapsed = last_draw.map(|last| now.saturating_sub(last));
        if rendered_generation.is_none()
            || (app.dirty && elapsed.is_none_or(|elapsed| elapsed >= FRAME_INTERVAL))
        {
            draw(host, app, &layout, &mut rendered_generation, &mut last_draw)?;
        }
        let timeout = if app.dirty {
            FRAME_INTERVAL
                .saturating_sub(host.now().saturating_sub(last_draw.unwrap_or_default()))
                .min(POLL_INTERVAL)
        } else {
            POLL_INTERVAL
        };
        // A pending interaction may finish just after the previous poll. Check its
        // result promptly; idle sessions retain the longer low-overhead wait.
        let timeout = if app.pending {
            timeout.min(Duration::from_millis(2))
        } else {
            timeout
        };
        let Some(input) = host.input(timeout)? else {
            continue;
        };
        if matches!(input, Input::KeyboardSupported) {
            if host.now() <= Duration::from_millis(500) {
                host.keyboard()?;
                app.enhanced_keyboard = true;
                app.dirty = true;
            }
            continue;
        }
        // Coalesced resize/overlay changes must reach the screen before pointer hit-testing.
        if matches!(input, Input::Pointer(_))
            && (app.dirty || rendered_generation != Some(layout.generation))
        {
            draw(host, app, &layout, &mut rendered_generation, &mut last_draw)?;
        }
        if let Input::Resize(width, height) = input {
            area = Rect::new(0, 0, width, height);
        }
        let had_surface = app.modal.is_some() || app.help;
        let intent = app.input(input, &layout);
        if !had_surface
            && (app.modal.is_some() || app.help)
            && let Some(bridge) = editor.as_deref()
        {
            bridge.revoke_search_navigation();
        }
        if let Some(intent) = intent
            && effect(intent, host, app, editor.as_deref_mut(), picker)?
        {
            return Ok(Exit { code: 0 });
        }
        // Admit a completed paste before releasing its reception guard.
        if let Some(bridge) = editor.as_deref() {
            bridge.receiving(app.receiving_paste_tab());
        }
    }
}

/// Validate dispatch authority at the tty and report what reached the endpoint.
fn deliver_clipboard(
    host: &mut impl Host,
    app: &mut AppState,
    transfer: ClipboardTransfer,
) -> ClipboardWrite {
    if app.system_clipboard_epoch() != Some(transfer.epoch) {
        if Some(transfer.sequence) == app.screen.clipboard_sequence {
            let message = if transfer.cut {
                "Cut internally | system request revoked"
            } else {
                "Copied internally | system revoked"
            };
            app.report_clipboard(transfer.sequence, message);
        }
        return ClipboardWrite::Unavailable;
    }
    let outcome = host.clipboard(&transfer);
    let message = match outcome {
        ClipboardWrite::Sent => "Terminal copy sent | unconfirmed",
        ClipboardWrite::Unavailable if transfer.cut => "Cut internally | system write failed",
        ClipboardWrite::Unavailable => "Copied internally | system write failed",
        ClipboardWrite::Lost => return outcome,
    };
    app.report_clipboard(transfer.sequence, message);
    outcome
}

/// Keep displayed controls and focus consistent after asynchronous or timer-driven transitions.
fn reconcile_layout(area: Rect, app: &mut AppState, layout: &mut LayoutSnapshot) {
    if layout.area != area || layout.signature != LayoutSnapshot::signature(app) {
        let mut next = LayoutSnapshot::for_app(area, app, layout.generation.wrapping_add(1));
        if layout.area == area
            && layout.content_height() == next.content_height()
            && layout.overlay.is_some() == next.overlay.is_some()
        {
            next.document_generation = layout.document_generation;
        }
        *layout = next;
    }
    app.sync_layout(layout);
}

/// Apply effects outside the reducer. Queue exhaustion is explicit and never drops accepted text.
fn effect(
    intent: Intent,
    host: &mut impl Host,
    app: &mut AppState,
    editor: Option<&mut Editor>,
    picker: Option<&crate::picker::Picker>,
) -> io::Result<bool> {
    match intent {
        Intent::Quit => {
            if let Some(editor) = editor {
                if editor.submit(Action::FinishQuit) {
                    app.expect_guard(editor.sequence());
                }
            } else {
                return Ok(true);
            }
        }
        Intent::QuitConfirmed => return Ok(true),
        Intent::Suspend => app.request_suspend(),
        Intent::MouseCapture(enabled) => host.mouse(enabled)?,
        Intent::Picker(request) => {
            if let Some(picker) = picker {
                picker.request(request);
            }
        }
        Intent::CancelPicker => {
            if let Some(picker) = picker {
                picker.cancel();
            }
        }
        Intent::CancelWork => {
            if let Some(editor) = editor {
                editor.cancel();
            }
        }
        Intent::Editor(action) => {
            if let Some(editor) = editor {
                if action.invalidates_clipboard_target() {
                    app.revoke_clipboard_target();
                }
                let action = match (app.screen.tab, action) {
                    (
                        Some(tab),
                        action @ (Action::Command(_)
                        | Action::Clipboard { .. }
                        | Action::NativeCopy { .. }
                        | Action::NativePaste { .. }
                        | Action::Insert(_)
                        | Action::Paste(_)
                        | Action::Save(_)
                        | Action::Scroll(..)
                        | Action::Hit { .. }
                        | Action::ClearSelection
                        | Action::GoTo { .. }),
                    ) => Action::ForTab(tab, Box::new(action)),
                    (_, action) => action,
                };
                let save_dialog = matches!(action, Action::Save(None))
                    || matches!(&action, Action::ForTab(_, inner) if matches!(**inner, Action::Save(None)));
                let guard = match action {
                    Action::Guard(after)
                    | Action::Discard(after, _)
                    | Action::SaveGuard(after, _) => Some(after),
                    _ => None,
                };
                let switching = matches!(
                    action,
                    Action::New
                        | Action::Open(_)
                        | Action::Follow(_)
                        | Action::Switch(_)
                        | Action::CycleTab(_)
                        | Action::CancelOpen(_)
                        | Action::CancelActiveOpen(_)
                ) || matches!(action, Action::RemoveTab(tab) if Some(tab) == app.screen.tab);
                if editor.submit(action) {
                    if let Some(after) = guard {
                        app.expect_guard_for(editor.sequence(), after);
                    }
                    if switching {
                        app.switching = Some(editor.sequence());
                    }
                    if save_dialog {
                        app.expect_save_dialog(editor.sequence());
                    }
                    app.pending = true;
                    app.dirty = true;
                } else {
                    let message = "Busy: input rejected; retry when ready";
                    // Keep rejection visible even when a pending worker immediately
                    // replaces the status row with its next ordinary completion.
                    app.event(crate::editor::Event::Failure(message.into()));
                    app.notify(StatusMessage::new(Severity::Warning, message));
                }
            } else if matches!(action, Action::Guard(After::Quit)) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
/// Exercise the scheduler independently of real storage or worker timing.
fn event_loop(host: &mut impl Host, inbox: StatusInbox) -> io::Result<Exit> {
    event_loop_impl(host, inbox, None, None, &mut AppState::new())
}

/// Draw and update scheduler bookkeeping in one place, including forced pointer-layout frames.
fn draw(
    host: &mut impl Host,
    app: &mut AppState,
    layout: &LayoutSnapshot,
    rendered: &mut Option<u64>,
    last_draw: &mut Option<Duration>,
) -> io::Result<()> {
    host.draw(app, layout)?;
    app.dirty = false;
    *rendered = Some(layout.generation);
    *last_draw = Some(host.now());
    Ok(())
}

/// Report unavailable job control without leaving active modes partially restored.
fn suspend(host: &mut impl Host, app: &mut AppState, editor: Option<&Editor>) -> io::Result<()> {
    app.revoke_clipboard();
    if let Some(editor) = editor {
        app.syntax_epoch = editor.pause_highlighting();
        app.screen.syntax_projection = None;
        if app.screen.syntax_status == crate::syntax::Status::Ready {
            app.screen.syntax_status = crate::syntax::Status::Preparing;
        }
    }
    let result = host.suspend();
    if let Some(editor) = editor
        && result.is_ok()
    {
        editor.resume_highlighting();
    }
    if result? {
        host.clear()?;
        app.input(
            Input::FocusLost,
            &LayoutSnapshot::new(host.area()?, app.help, 0),
        );
        app.dirty = true;
    } else {
        app.notify(StatusMessage::new(
            Severity::Warning,
            "Suspension unavailable in this session",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{command::Command, status};
    use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
    use std::{cell::Cell, collections::VecDeque};

    /// Settings and suspension can overtake a Copy held inside a real storage read.
    #[test]
    fn held_copy_respects_settings_and_suspension_at_delivery() {
        crate::provide_descriptor_headroom();
        use crate::document::{FaultInjector, IoPoint, OpenOptions};
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
            mpsc,
        };

        #[derive(Debug)]
        struct HoldRead {
            armed: AtomicBool,
            entered: mpsc::SyncSender<()>,
            release: Mutex<mpsc::Receiver<()>>,
        }
        impl FaultInjector for HoldRead {
            fn check(&self, point: IoPoint) -> io::Result<()> {
                if point == IoPoint::Read && self.armed.swap(false, Ordering::AcqRel) {
                    self.entered.send(()).unwrap();
                    self.release
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .map_err(io::Error::other)?;
                }
                Ok(())
            }
        }
        let acknowledged = |editor: &Editor| {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut last = None;
            loop {
                if let Some(screen) = editor.screen() {
                    if screen.acknowledged == editor.sequence() {
                        return screen;
                    }
                    last = Some((screen.acknowledged, screen.message));
                }
                assert!(
                    Instant::now() < deadline,
                    "worker did not acknowledge command {}, last {last:?}, stopped {}, events {:?}",
                    editor.sequence(),
                    editor.failed(),
                    editor.events.try_iter().collect::<Vec<_>>()
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        for (enabled, suspending) in [(false, false), (true, false), (true, true)] {
            let root = tempfile::tempdir().unwrap();
            let (entered, reached) = mpsc::sync_channel(1);
            let (release, held) = mpsc::sync_channel(1);
            let gate = Arc::new(HoldRead {
                armed: AtomicBool::new(false),
                entered,
                release: Mutex::new(held),
            });
            let store = crate::recovery::Store::open(crate::recovery::Options {
                root: root.path().join("state"),
                reserve_bytes: 0,
                ..Default::default()
            })
            .unwrap();
            let mut editor = Editor::with_options(
                root.path().to_owned(),
                crate::stream::Startup {
                    paths: Vec::new(),
                    source: None,
                },
                store,
                None,
                None,
                OpenOptions {
                    faults: Some(gate.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
            let mut app = AppState::new();
            app.config_location = crate::settings::Location::isolated(root.path().to_owned());
            let apply = |app: &mut AppState, editor: &mut Editor, enabled| {
                std::fs::write(
                    root.path().join("config.toml"),
                    format!("[clipboard]\ncopy_to_system = {enabled}\n"),
                )
                .unwrap();
                let previous = app.settings_generation;
                app.settings.reload();
                let deadline = Instant::now() + Duration::from_secs(5);
                while app.settings_generation == previous {
                    app.poll_settings(Some(editor));
                    assert!(Instant::now() < deadline, "settings did not apply");
                    std::thread::yield_now();
                }
            };
            apply(&mut app, &mut editor, enabled);
            assert!(editor.submit(Action::New));
            acknowledged(&editor);
            assert!(editor.submit(Action::Insert("held copy text".into())));
            assert!(editor.submit(Action::Command(Command::SelectAll)));
            app.update(acknowledged(&editor), false);
            let layout = LayoutSnapshot::for_app(Rect::new(0, 0, 80, 24), &app, 1);
            let Some(Intent::Editor(copy)) = app.input(Input::Command(Command::Copy), &layout)
            else {
                panic!("Copy did not dispatch");
            };
            gate.armed.store(true, Ordering::Release);
            assert!(editor.submit(copy));
            reached.recv_timeout(Duration::from_secs(5)).unwrap();
            // The test retains status, not a rendered frame. Multiple editor fixtures
            // share the process-wide two-layout budget while tests run in parallel.
            app.screen.view = None;
            let mut host = Fake::new(Vec::new());
            if suspending {
                suspend(&mut host, &mut app, Some(&editor)).unwrap();
            } else {
                if enabled {
                    apply(&mut app, &mut editor, false);
                }
                apply(&mut app, &mut editor, true);
            }
            release.send(()).unwrap();
            let copied = acknowledged(&editor);
            let transfer = editor.take_clipboard(copied.acknowledged);
            app.update(copied, false);
            if enabled {
                assert_eq!(
                    deliver_clipboard(&mut host, &mut app, transfer.unwrap()),
                    ClipboardWrite::Unavailable
                );
            } else {
                assert!(
                    transfer.is_none(),
                    "enabling revived an internal-only command"
                );
            }
            assert!(host.clipboard_frames.is_empty());
            app.screen.view = None;
            assert!(editor.submit(Action::Insert(String::new())));
            assert!(editor.submit(Action::Command(Command::Paste)));
            acknowledged(&editor);
            let destination = root.path().join("round-trip.txt");
            assert!(editor.submit(Action::Save(Some(destination.clone()))));
            let deadline = Instant::now() + Duration::from_secs(5);
            while !std::fs::read_to_string(&destination).is_ok_and(|text| text == "held copy text")
            {
                assert!(
                    Instant::now() < deadline,
                    "internal copy did not survive revocation"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            editor.shutdown().unwrap().join().unwrap();
        }
    }

    /// A later successful internal copy retires a frame before its first byte.
    #[test]
    fn newer_oversized_copy_revokes_taken_transfer() {
        crate::provide_descriptor_headroom();
        let root = tempfile::tempdir().unwrap();
        let store = crate::recovery::Store::open(crate::recovery::Options {
            root: root.path().join("state"),
            reserve_bytes: 0,
            ..Default::default()
        })
        .unwrap();
        let mut editor = Editor::new(
            root.path().to_owned(),
            crate::stream::Startup {
                paths: Vec::new(),
                source: None,
            },
            store,
            None,
            None,
        )
        .unwrap();
        let ack = |editor: &Editor| {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut last = None;
            loop {
                if let Some(screen) = editor.screen() {
                    if screen.acknowledged == editor.sequence() {
                        return screen;
                    }
                    last = Some((screen.acknowledged, screen.message));
                }
                assert!(
                    Instant::now() < deadline,
                    "worker did not acknowledge command {}, last {last:?}, stopped {}, events {:?}",
                    editor.sequence(),
                    editor.failed(),
                    editor.events.try_iter().collect::<Vec<_>>()
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        let mut host = Fake::new(Vec::new());
        let mut app = AppState::new();
        app.settings.applied.values.clipboard_override =
            Some(crate::settings::ClipboardMode::Automatic);
        let epoch = app.system_clipboard_epoch();

        assert!(editor.submit(Action::New));
        ack(&editor);
        assert!(editor.submit(Action::Insert("old".into())));
        assert!(editor.submit(Action::Command(Command::SelectAll)));
        assert!(editor.submit(Action::Clipboard {
            cut: false,
            system: epoch
        }));
        let old_screen = ack(&editor);
        let old = editor.take_clipboard(old_screen.acknowledged).unwrap();
        app.update(old_screen, false);
        app.screen.view = None;

        // An unsuccessful nonblocking attempt does not reserve output authority.
        let first = old.write_first(|_| Err(io::ErrorKind::WouldBlock.into()));
        assert_eq!(first.unwrap_err().kind(), io::ErrorKind::WouldBlock);

        // The worker can finish these during a readiness wait before the first
        // nonblocking tty write. No copy has reached the fake tty yet.
        assert!(editor.submit(Action::Insert("n".repeat(8193))));
        assert!(editor.submit(Action::Command(Command::SelectAll)));
        assert!(editor.submit(Action::Clipboard {
            cut: false,
            system: epoch
        }));
        let new_screen = ack(&editor);
        assert!(new_screen.message.contains("system limit 8 KiB"));
        assert!(editor.take_clipboard(new_screen.acknowledged).is_none());

        assert_eq!(
            deliver_clipboard(&mut host, &mut app, old),
            ClipboardWrite::Unavailable
        );
        assert!(host.clipboard_frames.is_empty());
    }

    /// A size acknowledgement can coalesce away the older copy acknowledgement.
    #[test]
    fn clipboard_feedback_survives_newer_layout_ack() {
        let mut host = Fake::new(Vec::new());
        let mut app = AppState::new();
        app.settings.applied.values.clipboard_override =
            Some(crate::settings::ClipboardMode::Automatic);
        let mut copied = app.screen.clone();
        copied.message = crate::editor::COPIED_INTERNALLY.into();
        copied.acknowledged = 12;
        copied.clipboard_sequence = Some(11);
        app.update(copied, false);
        let transfer = ClipboardTransfer {
            epoch: app.system_clipboard_epoch().unwrap(),
            cut: false,
            sequence: 11,
            frame: b"test frame".to_vec(),
            generation: 0,
            authority: std::sync::Arc::default(),
        };
        assert_eq!(
            deliver_clipboard(&mut host, &mut app, transfer),
            ClipboardWrite::Sent
        );
        assert_eq!(host.clipboard_frames, [b"test frame".to_vec()]);
        assert_eq!(app.status.text(), "Terminal copy sent | unconfirmed");
        let mut resized = app.screen.clone();
        resized.acknowledged += 1;
        app.update(resized.clone(), false);
        assert_eq!(app.status.text(), "Terminal copy sent | unconfirmed");
        resized.clipboard_sequence = Some(12);
        app.update(resized, false);
        app.report_clipboard(11, "Terminal copy sent | unconfirmed");
        assert_eq!(app.status.text(), crate::editor::COPIED_INTERNALLY);
    }

    /// A prepared write overtaken by disable/re-enable cannot enter terminal output.
    #[test]
    fn clipboard_delivery_skips_a_revoked_write() {
        let mut host = Fake::new(Vec::new());
        let mut app = AppState::new();
        let mut copied = app.screen.clone();
        copied.message = crate::editor::COPIED_INTERNALLY.into();
        copied.clipboard_sequence = Some(0);
        app.update(copied.clone(), false);
        app.settings.applied.values.clipboard_override =
            Some(crate::settings::ClipboardMode::Automatic);
        let epoch = app.system_clipboard_epoch().unwrap();
        let transfer = ClipboardTransfer {
            epoch,
            cut: false,
            sequence: 0,
            frame: b"pending".to_vec(),
            generation: 0,
            authority: std::sync::Arc::default(),
        };
        app.settings.applied.values.clipboard_override =
            Some(crate::settings::ClipboardMode::Internal);
        app.revoke_clipboard();
        app.settings.applied.values.clipboard_override =
            Some(crate::settings::ClipboardMode::Automatic);
        assert_eq!(
            deliver_clipboard(&mut host, &mut app, transfer),
            ClipboardWrite::Unavailable
        );
        assert!(
            host.clipboard_frames.is_empty(),
            "revoked clipboard request reached terminal output"
        );
        assert!(app.status.text().contains("system revoked"));
        let transfer = ClipboardTransfer {
            epoch: app.system_clipboard_epoch().unwrap(),
            cut: false,
            sequence: 0,
            frame: b"new".to_vec(),
            generation: 0,
            authority: std::sync::Arc::default(),
        };
        assert_eq!(
            deliver_clipboard(&mut host, &mut app, transfer),
            ClipboardWrite::Sent
        );
        assert_eq!(host.clipboard_frames, [b"new".to_vec()]);
        app.update(app.screen.clone(), false);
        assert!(app.reports_clipboard());
        let mut saved = app.screen.clone();
        saved.message = "Saved newer file state".into();
        app.update(saved, false);
        assert_eq!(app.status.text(), "Saved newer file state");
        assert!(!app.reports_clipboard());

        app.update(copied, false);
        app.notify(StatusMessage::new(
            Severity::Info,
            "Saved before clipboard delivery",
        ));
        let transfer = ClipboardTransfer {
            epoch: app.system_clipboard_epoch().unwrap(),
            cut: false,
            sequence: 0,
            frame: b"still delivered".to_vec(),
            generation: 0,
            authority: std::sync::Arc::default(),
        };
        assert_eq!(
            deliver_clipboard(&mut host, &mut app, transfer),
            ClipboardWrite::Sent
        );
        assert_eq!(app.status.text(), "Saved before clipboard delivery");
    }

    /// Deferred field submission must replace dialog geometry/focus before the next key.
    #[test]
    fn deferred_path_submission_reconciles_focus_and_geometry() {
        use crate::{
            app::{Modal, PathKind},
            editor::Event,
            spool::Spool,
            ui::Control,
        };
        use std::sync::Arc;
        let area = Rect::new(0, 0, 80, 24);
        let mut app = AppState::new();
        app.screen.loading = false;
        app.screen.tab = Some(crate::editor::TabId(1));
        app.screen.document = Some(crate::document::DocumentId(1));
        app.modal = Some(Modal::Path(PathKind::Save, String::new()));
        app.focus = Some(Control::PathField);
        let mut layout = LayoutSnapshot::for_app(area, &app, 1);
        let mut spool = Spool::new().unwrap();
        spool.append(b"destination").unwrap();
        assert!(matches!(
            app.input(Input::Paste(Arc::new(spool)), &layout),
            Some(Intent::Editor(Action::FieldPaste(_, 0)))
        ));
        app.input(Input::Command(Command::Newline), &layout);
        app.event(Event::FieldPaste(0, Ok("destination".into())));
        reconcile_layout(area, &mut app, &mut layout);
        assert!(matches!(
            app.tick(Duration::ZERO, &layout),
            Some(Intent::Editor(Action::Save(Some(_))))
        ));
        reconcile_layout(area, &mut app, &mut layout);
        assert_eq!(layout.signature, LayoutSnapshot::signature(&app));
        assert!(layout.overlay.is_none());
        assert_eq!(app.focus, Some(Control::Content));
        assert!(
            matches!(app.input(Input::Text("x".into()), &layout), Some(Intent::Editor(Action::Insert(text))) if text == "x")
        );
    }

    /// Scripted runtime advances virtual time during polls; no sleeps or real terminal needed.
    struct Fake {
        now: Cell<Duration>,
        events: VecDeque<Input>,
        frames: Vec<(Duration, u64, Rect)>,
        fail_draw: bool,
        keyboards: usize,
        clipboard_frames: Vec<Vec<u8>>,
        terminate_at: Duration,
    }

    impl Fake {
        /// Set a finite input script; the final quit or virtual termination ends the loop.
        fn new(events: Vec<Input>) -> Self {
            Self {
                now: Cell::new(Duration::ZERO),
                events: events.into(),
                frames: Vec::new(),
                fail_draw: false,
                keyboards: 0,
                clipboard_frames: Vec::new(),
                terminate_at: Duration::from_millis(100),
            }
        }
    }

    impl Host for Fake {
        fn clipboard(&mut self, transfer: &ClipboardTransfer) -> ClipboardWrite {
            match transfer.write_first(|frame| {
                self.clipboard_frames.push(frame.to_vec());
                Ok(frame.len())
            }) {
                Ok(_) => ClipboardWrite::Sent,
                Err(_) => ClipboardWrite::Unavailable,
            }
        }
        /// Read virtual monotonic time.
        fn now(&self) -> Duration {
            self.now.get()
        }
        /// Deliver queued input at 0.1 ms intervals, otherwise advance by the poll timeout.
        fn input(&mut self, timeout: Duration) -> io::Result<Option<Input>> {
            let event = self.events.pop_front();
            self.now.set(
                self.now.get()
                    + if event.is_some() {
                        Duration::from_micros(100)
                    } else {
                        timeout
                    },
            );
            Ok(event)
        }
        /// Save frame metadata or inject a backend write failure.
        fn draw(&mut self, _app: &AppState, layout: &LayoutSnapshot) -> io::Result<()> {
            if self.fail_draw {
                return Err(io::Error::other("draw failed"));
            }
            self.frames
                .push((self.now(), layout.generation, layout.area));
            Ok(())
        }
        /// Starting dimensions for deterministic geometry.
        fn area(&self) -> io::Result<Rect> {
            Ok(Rect::new(0, 0, 80, 24))
        }
        /// Stop idle runs, also guarding against starvation during a flood.
        fn termination(&self) -> Option<u8> {
            (self.now() >= self.terminate_at).then_some(15)
        }
        /// No synthetic signals beyond virtual termination.
        fn suspend_requested(&self) -> bool {
            false
        }
        /// No continuation in the scheduler scenarios.
        fn continued(&self) -> bool {
            false
        }
        /// No actual process stopping in deterministic tests.
        fn suspend(&mut self) -> io::Result<bool> {
            Ok(false)
        }
        /// Observe capability acceptance without acquiring a real terminal mode.
        fn keyboard(&mut self) -> io::Result<()> {
            self.keyboards += 1;
            Ok(())
        }
        /// Nothing to clear on the fake renderer.
        fn clear(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// An unchanged idle screen is drawn once, while bounded polls keep termination responsive.
    #[test]
    fn idle_does_not_redraw() {
        let mut host = Fake::new(vec![]);
        let (_, inbox) = status::mailbox();
        assert_eq!(event_loop(&mut host, inbox).unwrap().code, 143);
        assert_eq!(host.frames.len(), 1);
        assert_eq!(host.now(), Duration::from_millis(100));
    }

    /// Motion floods cannot starve quit, signals, or a fresh layout before the next pointer.
    #[test]
    fn flood_preserves_quit_and_resize() {
        let motion = Input::Pointer(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        });
        let mut events = vec![motion.clone(); 500];
        events.extend([
            Input::Resize(40, 8),
            motion.clone(),
            Input::Command(Command::Quit),
        ]);
        let mut host = Fake::new(events);
        let (_, inbox) = status::mailbox();
        assert_eq!(event_loop(&mut host, inbox).unwrap().code, 0);
        assert_eq!(host.frames.last().unwrap().2, Rect::new(0, 0, 40, 8));
        assert!(host.frames.len() <= 3);
        // An open but hidden help overlay at tiny sizes must not regenerate layout on every event.
        let mut events = vec![Input::Command(Command::Help), Input::Resize(20, 4)];
        events.extend(vec![motion.clone(); 500]);
        events.push(Input::Command(Command::Quit));
        let mut host = Fake::new(events);
        let (_, inbox) = status::mailbox();
        assert_eq!(event_loop(&mut host, inbox).unwrap().code, 0);
        assert!(host.frames.len() <= 3);
        let mut host = Fake::new(vec![motion; 10_000]);
        let (_, inbox) = status::mailbox();
        assert_eq!(event_loop(&mut host, inbox).unwrap().code, 143);
        assert!(!host.events.is_empty());
    }

    /// Detection is bounded; late/absent replies preserve portable quit and never become text.
    #[test]
    fn keyboard_detection_timeout_and_fallback() {
        for late in [false, true] {
            let mut host = Fake::new(vec![
                Input::KeyboardSupported,
                Input::Command(Command::Quit),
            ]);
            if late {
                host.now.set(Duration::from_millis(501));
                host.terminate_at = Duration::from_secs(1);
            }
            let (_, inbox) = status::mailbox();
            assert_eq!(event_loop(&mut host, inbox).unwrap().code, 0);
            assert_eq!(host.keyboards, usize::from(!late));
        }
        let mut host = Fake::new(vec![Input::Command(Command::Quit)]);
        let (_, inbox) = status::mailbox();
        assert_eq!(event_loop(&mut host, inbox).unwrap().code, 0);
        assert_eq!(host.keyboards, 0);
    }

    /// Backend failures propagate out of the event loop to the terminal cleanup boundary.
    #[test]
    fn draw_error_is_not_swallowed() {
        let mut host = Fake::new(vec![]);
        host.fail_draw = true;
        let (_, inbox) = status::mailbox();
        assert!(event_loop(&mut host, inbox).is_err());
    }
}
