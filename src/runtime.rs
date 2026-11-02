//! Single-reader event loop with monotonic frame scheduling and bounded background work.

use std::{
    io,
    time::{Duration, Instant},
};

use crossterm::event;
use ratatui::{Terminal, TerminalOptions, Viewport, backend::CrosstermBackend, layout::Rect};

use crate::{
    Exit,
    app::{AppState, Intent},
    input::{self, Input},
    status::{Severity, StatusInbox, StatusMessage},
    terminal::{Signals, TerminalSession, UnixTerminal},
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
    /// Restore/suspend/reacquire, returning false when job control is unavailable.
    fn suspend(&mut self) -> io::Result<bool>;
    /// Clear backend state so the next frame fully redraws after continuation.
    fn clear(&mut self) -> io::Result<()>;
}

/// Concrete session adapter; backend rendering and lifecycle ownership remain separate.
struct TerminalHost<'a> {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    session: &'a mut TerminalSession<UnixTerminal>,
    signals: &'a Signals,
    started: Instant,
}

impl Host for TerminalHost<'_> {
    /// Use an elapsed monotonic clock rather than wall time.
    fn now(&self) -> Duration {
        self.started.elapsed()
    }
    /// Crossterm poll/read are called on this thread only.
    fn input(&mut self, timeout: Duration) -> io::Result<Option<Input>> {
        if event::poll(timeout)? {
            Ok(input::normalize(event::read()?))
        } else {
            Ok(None)
        }
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
        let size = self.terminal.size()?;
        Ok(Rect::new(0, 0, size.width, size.height))
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
    /// Restore and stop outside the reducer and signal handler.
    fn suspend(&mut self) -> io::Result<bool> {
        self.session.suspend()
    }
    /// Invalidate Ratatui's previous buffer following re-entry.
    fn clear(&mut self) -> io::Result<()> {
        // Terminal::clear() queries the cursor position in Ratatui 0.30.2. A fullscreen
        // resize invalidates both buffers without a terminal round trip, even at the same size.
        let area = self.area()?;
        self.terminal.resize(area)
    }
}

/// Run with explicitly acquired modes; Ratatui initialization helpers are not used.
pub(crate) fn run(
    session: &mut TerminalSession<UnixTerminal>,
    signals: &Signals,
    inbox: StatusInbox,
) -> io::Result<Exit> {
    let (width, height) = crossterm::terminal::size()?;
    // The event loop owns resize/layout changes. A fixed viewport prevents Ratatui
    // independently resizing between snapshot construction, drawing, and hit-testing.
    let terminal = Terminal::with_options(
        CrosstermBackend::new(io::stdout()),
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, width, height)),
        },
    )?;
    let mut host = TerminalHost {
        terminal,
        session,
        signals,
        started: Instant::now(),
    };
    event_loop(&mut host, inbox)
}

/// Own UI state, checking signals between individual events even during an input flood.
fn event_loop(host: &mut impl Host, inbox: StatusInbox) -> io::Result<Exit> {
    let mut app = AppState::new();
    let mut area = host.area()?;
    let mut layout = LayoutSnapshot::new(area, app.help, 0);
    let mut rendered_generation = None;
    let mut last_draw = None;

    loop {
        if let Some(signal) = host.termination() {
            return Ok(Exit { code: 128 + signal });
        }
        if host.suspend_requested() {
            suspend(host, &mut app)?;
            area = host.area()?;
            rendered_generation = None;
        }
        if host.continued() {
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
        if layout.area != area || layout.help_open != app.help {
            layout = LayoutSnapshot::new(area, app.help, layout.generation.wrapping_add(1));
        }
        app.sync_layout(&layout);
        let now = host.now();
        let elapsed = last_draw.map(|last| now.saturating_sub(last));
        if rendered_generation.is_none()
            || (app.dirty && elapsed.is_none_or(|elapsed| elapsed >= FRAME_INTERVAL))
        {
            draw(
                host,
                &mut app,
                &layout,
                &mut rendered_generation,
                &mut last_draw,
            )?;
        }
        let timeout = if app.dirty {
            FRAME_INTERVAL
                .saturating_sub(host.now().saturating_sub(last_draw.unwrap_or_default()))
                .min(POLL_INTERVAL)
        } else {
            POLL_INTERVAL
        };
        let Some(input) = host.input(timeout)? else {
            continue;
        };
        // Coalesced resize/overlay changes must reach the screen before pointer hit-testing.
        if matches!(input, Input::Pointer(_)) && rendered_generation != Some(layout.generation) {
            draw(
                host,
                &mut app,
                &layout,
                &mut rendered_generation,
                &mut last_draw,
            )?;
        }
        if let Input::Resize(width, height) = input {
            area = Rect::new(0, 0, width, height);
        }
        match app.input(input, &layout) {
            Some(Intent::Quit) => return Ok(Exit { code: 0 }),
            Some(Intent::Suspend) => {
                suspend(host, &mut app)?;
                area = host.area()?;
                rendered_generation = None;
            }
            None => {}
        }
    }
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
fn suspend(host: &mut impl Host, app: &mut AppState) -> io::Result<()> {
    if host.suspend()? {
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

    /// Scripted runtime advances virtual time during polls; no sleeps or real terminal needed.
    struct Fake {
        now: Cell<Duration>,
        events: VecDeque<Input>,
        frames: Vec<(Duration, u64, Rect)>,
        fail_draw: bool,
    }

    impl Fake {
        /// Set a finite input script; the final quit or virtual termination ends the loop.
        fn new(events: Vec<Input>) -> Self {
            Self {
                now: Cell::new(Duration::ZERO),
                events: events.into(),
                frames: Vec::new(),
                fail_draw: false,
            }
        }
    }

    impl Host for Fake {
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
            (self.now() >= Duration::from_millis(100)).then_some(15)
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
        let mut events = vec![motion; 500];
        events.extend([Input::Resize(40, 8), motion, Input::Command(Command::Quit)]);
        let mut host = Fake::new(events);
        let (_, inbox) = status::mailbox();
        assert_eq!(event_loop(&mut host, inbox).unwrap().code, 0);
        assert_eq!(host.frames.last().unwrap().2, Rect::new(0, 0, 40, 8));
        assert!(host.frames.len() <= 3);
        // An open but hidden help overlay at tiny sizes must not regenerate layout on every event.
        let mut events = vec![Input::Command(Command::Help), Input::Resize(20, 4)];
        events.extend(vec![motion; 500]);
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

    /// Backend failures propagate out of the event loop to the terminal cleanup boundary.
    #[test]
    fn draw_error_is_not_swallowed() {
        let mut host = Fake::new(vec![]);
        host.fail_draw = true;
        let (_, inbox) = status::mailbox();
        assert!(event_loop(&mut host, inbox).is_err());
    }
}
