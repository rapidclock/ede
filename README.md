# ede

A Rust terminal text editor for macOS and Linux, built in reviewed stages.
Current version: **0.0.1-dev.5**. Stage 5 adds persistent scratches and automatic file refresh.
See the [MVP overview](specs/001_initial_mvp/overview.md) and the
[Stage 5 validation record](specs/001_initial_mvp/05_scratch_recovery.md#validation-record).

## Build and run

Install Rust through rustup. `rust-toolchain.toml` pins the toolchain and its
formatting/linting components.

```sh
cargo build --locked
cargo run --locked
cargo run --locked -- path/to/file.txt
cargo run --locked -- first.txt second.txt
cargo run --locked -- -- -filename.txt
```

Run in a terminal with a usable `TERM`, at least 40 columns by 8 rows. Smaller
windows preserve work and show a resize prompt; return to a supported size to
answer unsaved-change prompts. `--help` and `--version` work without a terminal.
Pipes, search, and highlighting belong to later stages.

Opening validates a private UTF-8 snapshot in the background. Its initial preview
is read-only; Escape cancels preparation. Once ready, files remain editable
regardless of total size. Lines do not wrap; horizontal scrolling follows the
caret. Enter preserves the document's suggested LF/CRLF style, Tab inserts a
literal tab, and existing bytes are not normalized. Unsupported encodings and
binary data report errors.

## Controls

| Action | Portable shortcut | Additional shortcut when Cmd is delivered |
| --- | --- | --- |
| New / Open | Ctrl+N / Ctrl+O | Cmd+N / Cmd+O |
| Previous / next tab | F7 / F8 | Same |
| Close tab | Ctrl+W | Cmd+W |
| Save / Save As | Ctrl+S / F4 | Cmd+S / Cmd+Shift+S |
| Quit | Ctrl+Q | Cmd+Q |
| Select all | Ctrl+A | Cmd+A |
| Cut / Copy / internal paste | Ctrl+X / Ctrl+C / Ctrl+V | Cmd+X / Cmd+C / Cmd+V |
| Undo / Redo | Ctrl+Z / Ctrl+Y | Cmd+Z / Cmd+Shift+Z |
| Move / extend selection | Arrows, Home, End, Page Up/Down; Shift extends | Same |
| Document start / end | Ctrl+Home / Ctrl+End | Same |
| Go to line or line:cell | Ctrl+G; numbers start at 1 | Same |
| Help / actions | F1 / F2 | Same |
| Leave text / previous control | F6 / Shift+F6 | Same |
| Activate / cancel | Enter / Escape | Same |

Buttons and the action menu provide alternatives for shortcuts intercepted by a
terminal. Tab cycles controls in dialogs/help and inserts text in the editor.
Help scrolls with Up/Down or the wheel; the action menu also scrolls with the wheel. Suspend is available through Help or Actions; resume
with the shell's `fg` command.

New and Open add tabs, and switching restores each tab's caret, selection,
scrolling, and undo history. Use the Tabs button or Actions → Tabs to select or
close a tab outside the strip. Actions → Tab details shows the active file's
path. The strip marks active (`>`), modified (`*`), waiting (`…`), loading
(`↓`), queued save (`⇡`), saving (`↑`), scratch (`~`), and attention (`!`).

Open starts in the active file's parent directory or the launch directory.
Use arrows or the wheel to select an entry and Enter to open it. Up and the
page buttons browse directories. Tab to Path to enter a literal absolute or
relative path. The listing includes hidden entries.

Click to place the caret, Shift-click to extend, or drag to select. Dragging beyond
the text area autoscrolls while the terminal continues reporting pointer events.
The wheel scrolls the view without moving the caret. Actions → Toggle mouse
capture pauses capture for terminal-native selection; F2 gives a keyboard route
to turn it back on. Native-selection modifiers depend on the terminal; use tmux's
copy mode inside tmux.

Copy/cut use **ede's private internal clipboard**, retained until process exit.
Ctrl+V, or a directly delivered Cmd+V key, inserts that clipboard. A terminal's
Paste command sends its external clipboard text through bracketed paste; this is
a different input path. Both support large contents using disk staging, with no
arbitrary paste-size cutoff. A paste commits as one undoable edit or leaves the
document unchanged on validation, staging, space, or pre-commit cancellation
failure. Bytes within bracketed paste are always data, including Escape. While
the status shows `Receiving paste`, run the displayed `kill -USR1 PID` command
from another shell on the editor host to cancel reception. The reader discards
the rest of that frame through its closing marker. Escape can cancel an admitted
paste before it commits once the terminal has sent the closing marker.

Save As creates a new destination; it does not overwrite an existing path. Normal
Save checks the previously acknowledged destination for external changes. Editing
can continue during saving; a save of older content leaves newer edits unsaved.
Close protects every scratch, including empty ones, with Save / Discard / Cancel
and Cancel selected initially. Quit automatically retains scratches and checks
modified named tabs in creation order. Canceling a later prompt keeps the session.
An uncertain save explicitly reports that the destination may have changed.

Scratches checkpoint every five seconds and flush their final contents on normal
Quit. Relaunch restores text, tab order, active scratch, caret, selection, and
viewport with fresh undo history. Each running instance owns a separate scratch
set; a new instance claims scratches from exited or crashed instances. Excess
entries beyond 64 open tabs remain in Actions → Recovery, with Open, Retry,
Discard, and paging. A crash restores the last complete checkpoint; unsaved
named-file edits are outside this recovery scope.

Recovery state lives in `$HOME/Library/Application Support/ede` on macOS, or
`$XDG_STATE_HOME/ede` on Linux (falling back to `$HOME/.local/state/ede`). Set an
absolute `EDE_STATE_DIR` to use another profile. Unusable state or a corrupt
catalog stops startup with a diagnostic. Failed objects remain listed with their
location and error so they can be repaired and retried. Persistence errors offer
Retry, Save As, and Cancel; failed Close retries Close. Tab details keeps the
checkpoint status and unresolved error visible.

Clean named tabs refresh automatically after external saves, including inactive
tabs and rename saves. Local edits and paste reception prevent replacement;
guarded Save still checks for conflicts. Refresh preserves the latest view and
adds one undo step within the 256 MiB history budget. If the previous text exceeds
that budget, refresh resets history and reports it. Missing or invalid sources
leave the last good text intact and retry with backoff. Actions → Retry refresh
and Tab details provide an immediate retry.

## Cmd delivery

On a terminal that reports enhanced keyboard support, ede requests disambiguated
key events and accepts Cmd as Super. Detection does not block startup; unanswered
or late capability replies retain portable operation. Acquired keyboard/mouse
modes are restored on exit and suspension.

A terminal can still consume individual Cmd shortcuts. Terminal capability alone
is insufficient evidence that a particular key reaches ede. A manually configured
mapping such as Cmd+S → byte `0x13` invokes portable Save instead. ede never changes
terminal, macOS, or tmux settings automatically. Terminal-specific mapping recipes
and direct Cmd compatibility are **pending real-terminal validation**; synthetic
PTY coverage does not establish a tested setup. See the Stage 3 validation record.

For an **unverified candidate iTerm2 mapping**, use a separate test profile and
add Cmd+S under Settings → Profiles → Keys → Key Mappings, with **Send Hex Code**
set to `0x13`. The resulting Ctrl+S input is covered by ede's protocol tests;
physical delivery through iTerm2 still needs a manual check. See iTerm2's
[key mapping documentation](https://iterm2.com/documentation-preferences-profiles-keys.html).
iTerm2 recommends the [Kitty protocol over its older CSI-u mode](https://iterm2.com/documentation-csiu.html),
which matches ede's negotiation. Do not infer all Cmd aliases from one mapping:
record each shortcut, terminal version, profile and direct/tmux/remote path.

The agreed compatibility scope includes macOS and Linux, direct terminals, tmux,
and the agreed remote transports. The user's iTerm2 setup is one target within
that matrix. Local iTerm2 3.7.6 was identified, but this environment blocks GUI
control of iTerm2, so its real key delivery remains unverified.

## Storage and architecture

| Module | Responsibility |
| --- | --- |
| `src/bin/ede.rs` / `lib` | CLI, lifecycle boundary, deferred diagnostics |
| `terminal` | Terminal modes, signals, suspend/resume |
| `ingress` / `spool` | Sole stdin reader, bounded VT parsing, streamed paste files |
| `runtime` | Frame scheduling, worker polling, effects |
| `input` / `command` | Text/modifier normalization and shared bindings |
| `app` / `ui` | Modal routing, focus, controls, prepared-frame rendering/hits |
| `picker` | Bounded native directory pages and cancellable background I/O |
| `editor` / `text` | Ordered storage coordination, clipboard/history positions, chunked Unicode layout |
| `document` | Paged text, immutable snapshots, edits, undo/redo, guarded saves |
| `recovery` | Owner locks, immutable scratch objects, atomic catalog, bounded persistence worker |
| `status` | Bounded notifications |

`ede::document::Document` is a blocking storage API. Interactive clients use
`StorageService`: bounded submit/poll/cancel operations, a document owner,
separate workers for immutable reads and file preparation, and at most one
independent save worker. Prepared files are installed and saves reconcile on
the owner before acknowledgement. Public snapshot leases block compaction;
internal background readers pin older immutable generations and allow foreground
compaction. Refresh builds a complete private generation and maps its view before
a short guarded owner swap. Terminal restoration precedes worker shutdown waits.
SIGINT/SIGTERM/SIGHUP use best-effort cleanup and exit `128 + signal`; abnormal
termination retains the last complete scratch checkpoint.

Ratatui/Crossterm provide rendering and mode commands. A dedicated bounded VT
reader avoids Crossterm's whole-string paste allocation. The Unicode segmentation
and width dependencies match Ratatui's resolved versions. Temporary files are
private and removed when their owners exit. The design uses deliberate disk
storage rather than depending on OS swap.

## Development and benchmarks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo run --release --locked --example storage_bench -- ordinary 1024 copy
EDE_BENCH_MIB=1024 EDE_BENCH_CORPUS=ordinary cargo test --release --locked \
  --test terminal large_file_interactive_benchmark -- --ignored --exact --nocapture
EDE_PASTE_MIB=64 cargo test --release --locked --test terminal \
  large_paste_memory_benchmark -- --ignored --exact --nocapture
EDE_RESOURCE_MIB=256 cargo test --release --locked --lib \
  interactive_resource_and_incorporation_measurements -- --ignored --nocapture
EDE_BENCH_MIB=1024 EDE_PASTE_MIB=64 cargo test --release --locked --test terminal \
  large_recovery_and_refresh_benchmark -- --ignored --exact --nocapture
EDE_BENCH_MIB=1024 cargo test --release --locked --lib \
  document::refresh::tests::large_refresh_storage_measurements -- --ignored --exact --nocapture
cargo test --locked --test terminal isolated_tmux_editing_and_suspend_smoke \
  -- --ignored --exact --nocapture
```

The opt-in interactive benchmark generates an allocated file, opens it in a real
PTY, measures first displayed text separately from editable Ready, samples
ordinary event-to-visible latency and editor RSS, navigates/edits beginning,
middle and end, and checks saving during edits. Corpus choices are `ordinary`,
`dense`, and `long`; sizes are MiB. Fixtures are isolated and removed afterwards.
Generated files are warm, and a PTY observer does not measure a graphical terminal
or remote transport. Linux CI exercises storage, editing, scratch recovery, and
refresh with 256 MiB and swap disabled. Native and CI measurements are recorded
in the Stage 5 validation record.

The resource probe runs the coordinator and UI against ordinary, dense, long-line
and mixed Unicode files. It records managed allocation high waters, working-file
sizes/allocated blocks, and reducer/update/render CPU time separately from terminal
transport. These counters do not represent total process memory or physical disk
ownership of shared filesystem-clone blocks.
For the executable benchmarks, `EDE_BENCH_PEAK_RSS=1` also reports the kernel's
lifetime peak RSS using `/usr/bin/time` (`-l` on macOS, `-v` on Linux), separately
from live samples. That tool is needed only for this opt-in measurement.

The storage benchmark also accepts `clone` with a copy fallback and optional
churn edits (up to 1000). Earlier evidence and agreed deferrals remain in the
[Stage 1](specs/001_initial_mvp/01_terminal_foundation.md#validation-record) and
[Stage 2](specs/001_initial_mvp/02_document_storage.md#validation-record) records.
