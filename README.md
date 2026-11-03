# ede

A Rust terminal text editor for macOS and Linux, built in reviewed stages.
Current version: **0.0.1-dev.1**.

Stage 01 provides the terminal shell: keyboard/mouse controls, help, focus,
resize handling, suspend/resume, and terminal restoration. Opening/editing files
and piped input arrive in later stages. See the [MVP overview](specs/001_initial_mvp/overview.md).

## Build and run

Install Rust through rustup. The repository pins Rust 1.99.1 and installs its
formatting/linting components through `rust-toolchain.toml`.

```sh
cargo build --locked
cargo run --locked
```

Run inside a terminal with a usable `TERM`, ideally at least 40 columns by 8 rows.
Smaller windows show a resize prompt; Ctrl+Q still quits. Informational flags work
without a terminal:

```sh
cargo run --locked -- --help
cargo run --locked -- --version
```

## Controls

| Action | Control |
| --- | --- |
| Help | F1 or Help button |
| Quit | Ctrl+Q or Quit button |
| Close help | Escape or Close button |
| Move focus | Tab / Shift+Tab |
| Activate focused control | Enter |
| Suspend / resume | Help → Suspend, then the shell's `fg` command |
| Save feedback | Ctrl+S reports “No document open” |
| Copy feedback | Ctrl+C reports “No selection” |

Click buttons to activate them. Clicking the empty content area focuses it;
dragging/wheeling there demonstrates input delivery without editing or scrolling.
Help is modal. Characters and paste are discarded while no document exists.
Cmd shortcuts and enhanced keyboard protocols are not required.

Inside the application, raw input lets Ctrl+S reach Save and Ctrl+C reach Copy.
The terminal's original settings return on quit or suspension. OS, terminal,
tmux, and connection software may intercept shortcuts before they reach ede;
ede does not change those applications' settings.

SIGINT, SIGTERM, and SIGHUP request cleanup; their exit code is `128 + signal`.
Suspension requires foreground shell job control. SIGKILL, external SIGSTOP, and
broken connections can prevent cleanup. Scratch recovery is a later stage.

## Structure

| Module | Responsibility |
| --- | --- |
| `src/bin/ede.rs` / `lib` | CLI, entry point, deferred error/panic reporting |
| `terminal` | Acquired modes, restoration, signal flags, job control |
| `runtime` | One event reader, frame scheduling, side effects |
| `input` / `command` | Event normalization and shared command identities/bindings |
| `app` | State and command reduction, without terminal I/O |
| `ui` | Geometry shared by rendering, focus order, and hit-testing |
| `status` | Bounded/coalesced notifications from future background services |

Ratatui and Crossterm provide rendering/input; signal-hook and nix handle Unix
signals/job control. No async runtime is needed. `vt100` is a test-only dependency
for inspecting rendered terminal output. `Cargo.lock` fixes the resolved versions.

## Development checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Tests include isolated Unix pseudo-terminals and an interactive shell to exercise
real bytes, kernel terminal settings, resize, signals, and suspend/resume. The
GitHub Actions workflow runs these checks on Linux and macOS. Pseudo-terminal
tests do not establish compatibility with every terminal app or remote transport.
See the [Stage 01 validation record](specs/001_initial_mvp/01_terminal_foundation.md#validation-record) for
tested environments and agreed deferrals. Future stage specifications are written
and reviewed one at a time, before separately authorized implementation.
