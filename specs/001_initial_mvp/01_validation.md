# Stage 01 validation record

Date: 2026-11-01. Branch: `stage/terminal_foundation`.
Specification: [Terminal foundation](01_terminal_foundation.md).

## Automated coverage

The implementation has 23 automated tests: 15 library tests, 3 CLI process tests,
and 5 executable pseudo-terminal tests. Formatting and Clippy pass locally with
the pinned toolchain. Native CI uses the same commands on Linux and macOS; its
results must be checked on the stage PR before declaring the stage complete.

| Acceptance | Evidence |
| --- | --- |
| A01–A02 | Help/version/argument statuses; piped bytes remain unread; redirected stdout and unusable TERM fail without terminal escapes or mutation |
| A03 | Injected failure at every acquisition step; partial output rollback; cleanup continues after errors and retries failed resources; I/O failure/unwind restoration; primary and cleanup diagnostics retained |
| A04 | Exact modifier/release/repeat tests; executable Ctrl+S/Ctrl+C/Ctrl+Q checks; IXON disabled while active |
| A05–A07 | Shared command handlers; modal help; capture cancellation; exclusive geometry; rendering at zero/tiny/normal sizes; actual mouse, resize, Escape, and bracketed-paste sequences |
| A08 | Interactive shell Suspend/fg cycle, resized resume, external SIGTSTP/fg, SIGINT/SIGTERM/SIGHUP exit codes, exact captured termios restoration and output cleanup requests |
| A09 | Virtual time/input: idle frame drawn once, input floods preserve quit/resize/signal handling; background queue saturation and progress coalescing tested |
| A10 | Lockfile included; Linux/macOS workflow runs fmt, Clippy, tests without a manifest guard; results recorded in PR checks |
| A11 | Local PTY and tmux checks below; unavailable manual environments explicitly deferred |

The pseudo-terminal suite uses `vt100` to inspect the displayed screen rather than
assuming raw output is plain text. The test launcher keeps a parent shell alive
until terminal attributes are captured: macOS revokes a controlling PTY when its
session leader exits. Tests never add user-facing crash/failure switches.

## Local environment and smoke results

Host: macOS 27.2 (26B123), arm64; rustc 1.99.1
`58cf8f9c3 2026-10-13`; Cargo 1.99.1; tmux 3.8.

| Environment | Result and scope |
| --- | --- |
| Unix PTY, `TERM=xterm-256color`, `/bin/sh -i` | Pass: executable shortcuts, paste, mouse, modal routing, tiny/normal resize, suspension/resumption, external signals, exact kernel attributes and cursor/reporting/alternate-screen cleanup requests |
| Isolated tmux 3.8 server, default config (`-f /dev/null`), local PTY clients | Pass: Ctrl+S/Ctrl+C/Ctrl+Q, F1/Escape, Suspend/fg, shrink/expand, two client attach/detach cycles, normal quit, terminal-settings comparison |
| Terminal.app and Linux graphical terminal frontends | Not manually verified; PTY/protocol checks do not establish frontend behavior |
| SSH to Linux, with/without tmux | Not available; manual check deferred |
| Eternal Terminal / mosh, with/without tmux | Not available; manual check deferred |

The tmux smoke used a private temporary socket/server and did not change user
configuration. `stty -g` snapshots match after suspension/quit apart from Darwin's
transient `PENDIN` input-reprocessing flag, which the comparison masks. The
executable PTY tests compare the exact inherited/restored `Termios` values.

On 2026-11-01, the user approved deferring unavailable manual compatibility checks
while recording them as unverified. These checks remain part of the later
compatibility matrix; no SSH/reconnection, terminal-app, or numeric latency result
is inferred from this stage's local tests. Cursor visibility and disabled mouse
capture are checked as emitted control sequences, without a human visual check.

## Implementation decisions and limits

- Toolchain/UI choices were approved with the stage. signal-hook resolved to
  0.4.6, a compatible patch release; nix 0.31.3 adds safe controlling-terminal and
  process-group queries needed for job-control validation. `vt100` 0.18.1 is used
  only by tests. Crossterm resolves to one version, 0.29.0. Its internal
  signal-hook 0.3 dependency coexists with the application's 0.4 dependency; signal
  registrations share signal-hook-registry and executable signal tests cover their
  interaction.
- `runtime.rs` and `status.rs` separate scheduling and bounded background delivery
  from entry points/state/lifecycle. There are no document types, artificial jobs,
  worker pools, storage/highlighting dependencies, or later-stage implementations.
- Ratatui 0.30.2's `Terminal::clear()` requests the cursor position. Re-entry uses
  fullscreen `resize()` to invalidate/redraw without that round trip. The PTY
  suite does not answer cursor queries and rejects emitted query sequences.
- Signal handlers set flags only. Main-loop checks happen between individual
  events, with at most a 20 ms idle poll and at most 64 background messages per
  iteration. Routine redraws are limited to about 60 Hz; fresh pointer geometry
  can force an earlier draw after resize/overlay changes.
- This stage establishes a shell. File editing, durable scratch recovery, clipboard
  integration, highlighting, stream input, and user key remapping remain future
  work. Existing process/global terminal restrictions and upstream shortcut
  interception are documented in the README.
