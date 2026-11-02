# 01 — Terminal foundation

Status: **Complete — ready for review in [PR #1](https://github.com/rapidclock/ede/pull/1).**

The user approved this design and explicitly authorized Stage 01 implementation
on 2026-11-01. Readability, cohesive modules, and idiomatic Rust are required; use
*Rust for Rustaceans* as a design guide. See the [validation record](01_validation.md).

Parent: [MVP overview](overview.md). Depends on: none.
Branch: `stage/terminal_foundation`. PR target: `feat/initial_mvp`.

## Outcome and scope

Deliver a small, buildable terminal application with reliable input and cleanup.
It displays an empty editor surface, help/actions, and status messages. Both keys
and mouse controls invoke named commands. It does not open or edit documents yet.

This stage establishes FR-01 and the routing/layout foundation of FR-03/FR-09.
Storage, text selection, clipboard access, search, highlighting, stream ingestion,
persistent configuration, and user remapping remain in their planned stages.
Plain character input and paste must not create a pretend editing buffer.

## Approved technical choices

The user approved these choices with the stage. Compatible patch updates and
small implementation dependencies are recorded in the validation record.

| Item | Choice and purpose |
| --- | --- |
| Package | One Cargo package, `ede`, version `0.0.1-dev.1`; binary plus testable library |
| Toolchain | Rust 1.99.1, edition 2024; pin in `rust-toolchain.toml` with rustfmt/clippy and minimal profile |
| UI | Ratatui 0.30.2 with its Crossterm backend |
| Terminal/input | Crossterm 0.29.0; one reader using `poll`/`read`, not `EventStream` |
| Unix signals | signal-hook 0.4.6 (compatible patch update from 0.4.5); handlers set flags, application loop performs cleanup |
| Job control | nix 0.31.3 for safe terminal/process-group queries |
| Test screen parser | vt100 0.18.1, test-only dependency |
| Execution | Standard-library event loop, no Tokio or other async runtime |

Rust 1.99.1 is installed on the development host. This is an initial toolchain
choice, not a promise to support older compilers. Commit `Cargo.lock` for the
application. Enable only required dependency features, retain upstream licenses,
and avoid duplicate Crossterm versions with incompatible event/backend types.
Do not add storage or highlighting dependencies in this stage.

## User-visible behavior

- `ede` starts the shell. `--help`/`-h` and `--version`/`-V` print ordinary text and
  exit successfully without initializing terminal modes. Unknown arguments,
  including filenames at this stage, return a clear error and exit code 2.
- Interactive startup requires terminal stdin and stdout and a usable `TERM`
  value. Reject missing/`dumb` TERM, piped stdin, or redirected stdout with a clear
  error before terminal mutation. Return code 1; emit no escape sequences or
  document output to redirected stdout. Stage 08 will introduce piped input through
  a separate data source; do not build assumptions that stdin is always UI input.
- At 40 columns by 8 rows or larger, show a one-row header, remaining content area,
  and one-row status bar. Header shows `ede`, its version, and Help/Quit controls.
  The content area says no document is open. Status shows a short message and
  essential shortcuts. Use text labels and basic colors; color is not the only
  means of conveying focus or errors.
- Below that size, show a clipped resize message, keep quit/cancel keys working,
  and omit controls that cannot fit. Zero-sized areas must not underflow or panic.
- Help opens an overlay listing available commands and shortcuts. Its Quit,
  Suspend, and Close actions work with the mouse and with Tab/Shift+Tab and Enter.
  Escape closes the overlay. Clicks outside it must not activate underlying UI.
- Clicking the empty content area focuses it. Mouse press/release, drag, and wheel
  events can produce a short status description to demonstrate delivery; there is
  no text selection or document scrolling yet. Never print pasted text to a log.
- Closing help returns focus to its opener, or the content area if that control
  no longer exists. Resize preserves a valid focus target and dismisses stale
  pointer capture.

### Commands and initial bindings

| Command identity | Trigger | Stage 01 result |
| --- | --- | --- |
| `app.quit` | Ctrl+Q; Quit control | Clean exit, code 0 |
| `app.help` | F1; Help control | Open help; focus its first action |
| `ui.cancel` | Escape; Close control | Close overlay; otherwise no operation |
| `focus.next` / `focus.previous` | Tab / Shift+Tab | Move between visible enabled controls in visual order |
| `ui.activate` | Enter on a focused control; click | Invoke that control's command |
| `document.save` | Ctrl+S | Status: "No document open"; never report a successful save |
| `selection.copy` | Ctrl+C | Status: "No selection"; do not exit or send an interrupt |
| `app.suspend` | Suspend action in help | Restore terminal, stop process, then resume safely |

Do not use bare `q` to quit or Ctrl+Z to suspend: future editing needs ordinary
characters and Ctrl+Z undo. Other editing shortcuts become active with their
features. Bindings are data mapping input/context to command identities, not
conditionals scattered through widgets; user configuration is added in stage 09.

Normalize Ctrl+letter events consistently, retain other modifiers, and avoid
mapping an Alt-modified event to a plain Ctrl shortcut. Ignore key-release events.
Repeat events may move focus but must not repeatedly activate discrete actions.
Treat paste as data, never as shortcut sequences; with no document, discard its
payload and show "No document open". Optional focus/paste reporting may be absent
on a connection; essential controls must still work.

## Structure and contracts

Use a small module structure; introduce more modules only when behavior needs them.
The table describes implemented module ownership. `runtime.rs` owns event-loop
scheduling and `status.rs` owns the bounded background mailbox.

| Module | Responsibility |
| --- | --- |
| `bin/ede.rs` | CLI, process exit status, outer error/panic boundary |
| `lib.rs` | Testable application entry points; no terminal side effects on import |
| `terminal.rs` | Terminal acquisition, lifecycle, restoration, signal flags |
| `input.rs` | Crossterm-to-application event normalization and contextual binding lookup |
| `command.rs` | Command identities, labels, default bindings, availability metadata |
| `app.rs` | Application state and command/event reduction; no direct terminal I/O |
| `ui.rs` | Layout, rendering, focus order, hit-testing and pointer capture |

Conceptual types may be enums/structs; keep them private unless another module
needs them. Detailed document types belong to stage 02.

| Concept | Minimum information and invariant |
| --- | --- |
| `AppState` | Open overlay, valid focus target, latest status, running/quit intent, dirty-layout/render flag |
| `AppEvent` | Normalized key, pointer, paste, focus, resize, signal intent, or background status |
| `CommandId` | Stable named action; independent of its keyboard or mouse trigger |
| `LayoutSnapshot` | Terminal dimensions, visible rectangles, control IDs/actions, z-order, focus order, generation |
| `PointerCapture` | Optional control owning a drag/release; reset on resize, focus loss, or overlay change |
| `StatusMessage` | Severity and bounded display text; never raw document/paste content |
| `TerminalSession` | Saved terminal state and successfully enabled modes; the sole owner of restoration |

Flow: terminal event → normalized event → layout/context resolution → command →
state update → layout/render when dirty. Both Enter and clicking a given button
must reach the same command handler. Document-only commands remain unavailable
without a document; status feedback must not perform document work.

Use the same layout snapshot for drawing and hit-testing. Rectangles have an
inclusive top/left and exclusive bottom/right. Overlay controls take precedence;
hidden or zero-sized controls cannot be hit. After resize, compute and draw a new
snapshot before routing further pointer events. Coordinates are terminal cells,
not text offsets; Unicode document positioning is introduced in stage 03.

Background services will publish messages to the application loop rather than
draw or mutate UI state. Provide a bounded status-message entry point, initially
64 messages, with coalescing for repeated progress and explicit backpressure for
completion/error messages. Input, signals, and quit must not depend on queue capacity.
Do not introduce a worker pool or synthetic background job UI just to demonstrate
this contract. Document revision checks are added with stage 02.

## Terminal lifecycle and scheduling

### Entry and restoration

1. Parse CLI and validate terminal prerequisites before making changes.
2. Register signal flags and arrange unwind cleanup. Construct one terminal owner.
3. Enable raw input, enter alternate screen, hide cursor, and request mouse,
   bracketed-paste, and focus reporting. Track each successful setup step.
4. Draw the first frame and start the event loop.
5. On exit or setup failure, attempt all applicable cleanup steps: release input
   reporting, show cursor, leave alternate screen, restore original terminal
   attributes. One cleanup error must not prevent remaining attempts.

Raw input must disable software flow control and terminal-generated Ctrl+C/Ctrl+Z
signals. Verify this in a real terminal: Ctrl+S reaches `document.save`, and Ctrl+C
reaches `selection.copy`. Restore the captured state, not an assumed "sane" mode.
Do not mix Ratatui automatic initialization with a second independent raw-mode or
cleanup owner. Use explicit backend setup for this lifecycle.

Cleanup is idempotent. Partial setup, draw/read errors, ordinary shutdown, and
panic unwinding must all pass through it. Send human-readable diagnostics after
leaving raw/alternate-screen mode where possible; do not call `process::exit`
inside the guarded application scope. Preserve the original error if cleanup also
fails, while reporting both. An unwinding panic exits with code 101; ordinary
runtime failures use 1. Do not select a panic-abort build profile.

### Signals and suspend/resume

- Signal handlers only set flags. They must not draw, allocate application data,
  or acquire terminal/UI locks. Check flags from normal loop execution.
- External SIGINT, SIGTERM, or SIGHUP requests cleanup and exit with `128 + signal`.
  A Ctrl+C key event is distinct from an externally delivered SIGINT.
- For `app.suspend` or external SIGTSTP, restore the terminal first, then suspend
  using a vetted signal primitive. One workable approach is raising SIGSTOP from
  normal application execution after cleanup, avoiding re-entry into the SIGTSTP
  handler. Resume on SIGCONT, reacquire terminal settings/modes, get the current
  size, invalidate pointer state, and force a full redraw. Re-entry failures exit
  cleanly. In a session with no usable job control, report that suspension is
  unavailable rather than leave the terminal half-configured.
- SIGKILL and an externally delivered SIGSTOP cannot be cleaned up in advance.
  A broken connection can also prevent successful output restoration. State these
  limits; do not claim crash-proof terminal restoration or SSH reconnection.

### Event loop

The main thread owns input and rendering. Use one Crossterm reader; do not mix
`poll`/`read` with another reader or `EventStream`. Check signals and bounded
background messages, then poll input with at most a 20 ms wait. Process bounded
input batches so mouse/repeat floods cannot starve signals or drawing.

Redraw when state changes, not on every poll. Coalesce resize/motion/progress
updates and cap routine drawing around 60 frames/second. Never coalesce away key
presses or pointer button transitions. Keep timers monotonic. No animations,
busy-wait loop, or indefinite worker joins are needed in this stage. Future I/O
must not be performed inline in a command handler on this thread.

## Compatibility baseline

Start with basic colors and conventional key/mouse sequences; do not require
enhanced keyboard protocols or blocking capability queries. Cmd shortcuts are
optional and must not be advertised as working without verification. Respect
upstream OS/terminal/tmux bindings; do not modify those applications' settings.

Initial test configurations:

| Configuration | Stage 01 check |
| --- | --- |
| macOS Terminal.app, directly and inside tmux | Full shell acceptance below |
| Linux xterm or GNOME Terminal, directly and inside tmux | Full shell acceptance below |
| SSH to Linux, then tmux | Input, mouse, resize, quit, cleanup, detach/reattach |
| Eternal Terminal or mosh, optionally with tmux | Early smoke checks when available; full matrix remains stage 10 |

Record actual OS/terminal/connection/tmux versions and required configuration with
results. Terminal apps above are proposed reference environments, not a ban on
others. Unavailable environments must be listed as unverified, not claimed to
pass; agree any deferral before marking stage acceptance complete.

The user approved manual-check deferrals on 2026-11-01. The validation record
distinguishes executable PTY/tmux checks from unverified graphical frontends and
remote transports.

## Implementation sequence

1. Create the Cargo package/toolchain and a testable library. Add dependencies and
   module ownership above; do not create document storage yet.
2. Implement CLI validation and the terminal lifecycle with failure-injection tests.
3. Implement the command reducer, bindings, layout/focus, and shared hit-testing.
4. Connect input/rendering; implement help, status feedback, and signal handling.
5. Add automated checks and run terminal smoke scenarios. Update README with stage
   usage/limitations and record the environment matrix results.
6. Remove the CI no-manifest guard. Run formatting, Clippy, and tests on both
   `ubuntu-latest` and `macos-latest`, using the pinned toolchain and locked
   dependency resolution. Keep workflow permissions read-only; no publishing.

CI commands: `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets --locked -- -D warnings`, and
`cargo test --workspace --locked`. Do not replace this stage with documentation-only
tests once implementation starts. The Rust application must compile and execute.

## Acceptance checks

| ID | Scenario and required result | Evidence |
| --- | --- | --- |
| A01 | Help/version work with redirected output, contain no terminal control sequences, and return 0; bad arguments return 2 | Automated CLI checks |
| A02 | Non-TTY stdin/stdout or unusable TERM fails before mutation, returns 1, and does not consume piped content | Automated process checks |
| A03 | Normal quit, setup failure at each step, read/draw error, and unwind all attempt restoration of acquired resources; cleanup can be repeated | Lifecycle tests with an injected terminal driver; real-terminal smoke |
| A04 | Ctrl+Q quits; F1 opens help; Escape dismisses it; Ctrl+S reports no document without freezing output; Ctrl+C reports no selection without interrupting | Binding/reducer tests and real-terminal smoke |
| A05 | Click and keyboard activation of the same control produce the same action; overlays block underlying controls; focus cycles only through visible controls | Layout/routing tests and mouse smoke |
| A06 | Resize, zero/tiny dimensions, drag outside bounds, and focus loss do not panic or leave stale hit targets; expanding restores the normal shell | Test backend/layout checks and terminal resize smoke |
| A07 | Bracketed paste is treated as data; printable characters and paste cannot trigger quit/save or create a document | Input/reducer tests and paste smoke |
| A08 | Suspend returns an ordinary usable shell; resume redraws at the new size; external termination cleans up where the connection permits | Real terminal/job-control checks, including tmux |
| A09 | With no changes, no repeated draw calls occur; a continuous motion stream cannot starve quit or resize | Event-loop tests using controllable time/input |
| A10 | Linux/macOS CI passes fmt, Clippy, and tests, with a committed lockfile and no no-manifest bypass | CI results |
| A11 | Proposed local and SSH/tmux baseline works, with unsupported optional features and missing environments explicitly recorded | Manual compatibility record |

For real-terminal checks, compare `stty -g` before and after normal exit, injected
failure, and suspend/resume; also confirm cursor visibility and mouse capture are
restored. Inject failures through test seams, not permanent user-facing crash
commands. No numeric input-latency result may be claimed from subjective smoke
checks; measure it separately as the UI becomes functional.

## Review and handoff

The design and implementation were authorized together on 2026-11-01. Deliver
this stage through a PR into `feat/initial_mvp`, with automated checks and the
validation record. Later stages still require their own detailed design review
and implementation authorization; no later stage is detailed or implemented here.

## References

- [Ratatui API and initialization choices](https://docs.rs/ratatui/0.30.2/ratatui/)
- [Ratatui backend compatibility](https://ratatui.rs/concepts/backends/)
- [Crossterm events and single-reader rule](https://docs.rs/crossterm/0.29.0/crossterm/event/index.html)
- [Crossterm raw mode](https://docs.rs/crossterm/0.29.0/crossterm/terminal/index.html#raw-mode)
- [signal-hook signal safety](https://docs.rs/signal-hook/0.4.6/signal_hook/)
