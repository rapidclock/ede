# ede

A Rust terminal text editor for macOS and Linux, built in reviewed stages.
Current version: **0.0.1-dev.2**.

Stage 01 provides the terminal shell: keyboard/mouse controls, help, focus,
resize handling, suspend/resume, and terminal restoration. Opening/editing files
and piped input arrive in later stages. Stage 02 adds a separately testable storage
library; it is not connected to the terminal shell yet.
See the [MVP overview](specs/001_initial_mvp/overview.md).

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
| `document` | Private paged text, revisions, transactions, undo/redo, line indexes and guarded saves |

Ratatui and Crossterm provide rendering/input; signal-hook and nix handle Unix
signals/job control. No async runtime is needed. `vt100` is a test-only dependency
for inspecting rendered terminal output. `Cargo.lock` fixes the resolved versions.
The storage core uses `tempfile` for private working/save files and `memchr` for
bounded text scanning.

## Storage core

`ede::document::Document` offers blocking open, disk-staged edits, undo/redo,
snapshots, compaction and safe saves. It supports UTF-8 and preserves BOM/newline
bytes; binary and unsupported encodings fail explicitly. `Snapshot` owns a bounded
immutable revision lease and returns scalar-aligned windows rather than whole
lines. Release leases before compaction.

Use `StorageService` from an interactive client: submission, cancellation, close
and completion polling do not wait on filesystem work. One worker serializes
mutations; another serves immutable snapshot reads. The direct document API and
`shutdown().wait()` belong outside the terminal event loop.

Run the reproducible storage benchmark with a real allocated text fixture:

```sh
cargo run --release --locked --example storage_bench -- ordinary 1024 copy
```

Corpus choices are `ordinary`, `dense`, and `long`; size is MiB. `clone` attempts a
filesystem clone with a copy fallback; output reports whether cloning was used.
An optional fourth argument, up to `1000`, measures repeated edits with default
undo history (for example `ordinary 1024 copy 1000`). Fixtures live in an isolated
temporary directory and are removed afterwards.
Results identify warm generated-file conditions; they do not claim cold-cache or
end-to-end UI latency. See the [Stage 02 validation record](specs/001_initial_mvp/02_document_storage.md#validation-record).

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
