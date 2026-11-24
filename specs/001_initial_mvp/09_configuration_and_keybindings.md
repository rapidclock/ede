# 09 — Configuration and keybinding UI

Status — Implemented and verified locally on macOS and Ubuntu. The stage is in a draft PR. Requirements come from the accepted [MVP overview](overview.md#fr-09--configuration-and-keybinding-remapping).

Branch — `stage/configuration_and_keybindings`. PR target — `feat/initial_mvp`.
Baseline — Stage 08 merged at `a258b85`.

## Outcome and scope

- Remap every user-facing command through a local file or the keyboard- and
  mouse-operable Settings UI. Persist UI edits to the same file, expose the
  effective bindings where users invoke them, and give users a way to recover
  from an unusable map. Preserve command availability and safety checks for
  editable documents, read-only streams, and modal confirmations.
- Configure the Stage 05 periodic scratch checkpoint interval and all three
  Stage 07 highlighting thresholds. Keep the Stage 07 theme, association, and
  query format and reload command. The persistent theme preference is still
  `default_theme` in `highlighting.toml`. The Syntax Theme picker applies a
  session override.
- Ship portable default bindings, document keys that depend on terminal delivery,
  and preserve the terminal lifecycle and clipboard behavior from Stages 01–03.
  Stage 10 owns the full terminal and remote-transport compatibility matrix.
- Terminal/OS shortcut editing, multi-chord sequences, macros, and synchronized
  profiles are outside this stage. The Actions menu provides command discovery.
  Settings captures and shows one delivered shortcut at a time.

## Settings file and startup

Resolve the machine-local config directory once per process using the Stage 07
rule. Use `$XDG_CONFIG_HOME/ede` if XDG_CONFIG_HOME is nonempty and absolute,
otherwise `$HOME/.config/ede` with an absolute HOME. If neither base is usable,
run with defaults, explain why persistence is unavailable, and leave scratch
state discovery to Stage 05. The general file is `config.toml` in that directory.
Existing `highlighting.toml` and `queries/` remain independent and require no
migration. A remote session uses the environment of the host running ede.

Use UTF-8 TOML. Every table and entry is optional. This example shows default
scalar values, an overridden binding list, an unbound optional command, and a
picker-specific binding.

```toml
[recovery]
checkpoint_interval_seconds = 5

[highlighting]
max_document_bytes = 20971520
max_line_bytes = 65536
parse_timeout_ms = 250

[keybindings.global]
save = ["Ctrl+S", "F9", "Cmd+S"]
reload_settings = []

[keybindings.picker]
activate = ["Enter", "Ctrl+O"]
```

| Setting | Default | Accepted integer range |
| --- | ---: | ---: |
| `recovery.checkpoint_interval_seconds` | 5 | 1–3,600 |
| `highlighting.max_document_bytes` | 20,971,520 | 1–20,971,520 |
| `highlighting.max_line_bytes` | 65,536 | 1–65,536 |
| `highlighting.parse_timeout_ms` | 250 | 1–250 |

The proposed upper limits for highlighting retain the Stage 07 resource
envelope. `[editor] default_line_ending` accepts `"lf"` or `"crlf"` and
defaults to `"lf"`. **Settings → Editor → Default line ending** exposes the
choice. Saving applies it immediately to new and open documents with no LF
or CRLF. Reject unknown fields, wrong types, out-of-range integers, unknown
contexts and command IDs, and malformed or conflicting bindings. Limit
`config.toml` to 64 KiB and each command entry to eight shortcuts. A missing file
means bundled defaults. Read only a regular file through a bounded read, with
nonblocking open and descriptor type check. Treat a symlink at `config.toml`,
including a dangling one, as an error. Parent directory symlinks may be resolved
using the selected config directory. Render path and diagnostic text safely as
terminal text. Report the path, offending setting or binding, and the reason,
with a line and column when available.

Read, parse, validate, and write off the interactive thread. Treat a candidate as
one settings revision. A failed initial read or validation publishes defaults
with a persistent diagnostic in Settings. A failed subsequent reload retains
the last applied revision. An optional file removed on disk returns to defaults
on successful reload. Read on startup and on explicit **Reload Settings** from
Actions or Settings. Keep at most one running file operation and one latest
pending reload. An obsolete read cannot publish a settings revision or replace
the diagnostic for a newer operation.

Help, version, and existing CLI usage errors perform no configuration I/O. On
an interactive launch, drawing and document/source startup can proceed during
configuration loading. Until the first general settings result arrives, show
**Use defaults for this session** with a mouse target and Escape as its keyboard
equivalent. Ignore action and text input during this brief startup state, with
normal terminal resize, signal, and teardown processing. The fallback revokes
the pending general load, starts with defaults, and leaves the file alone.
These defaults have no verified save base. Enable saving only after a later
explicit reload records a valid file or confirms its absence.

### Safe startup

Accept `--safe-config` as an optional leading flag to each existing interactive
invocation, including `ede --safe-config --follow FILE` and
`producer | ede --safe-config -- FILE`. Preserve `--` as the filename separator
and the existing stdin/tty rules and tab counts. The flag by itself opens the
ordinary interactive session with default settings. Document the flag in
`--help`. In safe mode use bundled settings and bundled highlighting throughout
the session. Skip reads and writes of user configuration, including query files,
and visibly disable configuration save and reload. Normal editing, guarding
and scratch recovery remain available. F1 Help shows the expected configuration
location when available so the user can fix or move a broken file externally and
relaunch. Safe startup and the failed-load default path must work without mouse
capture or enhanced keys.

## Commands and key resolution

Use a single command catalog for stable lowercase snake_case IDs, display labels,
allowed contexts, repeat behavior, and defaults. Existing named actions retain
their behavior and pass through the same command handlers whether invoked by
key, menu, button, or pointer. Register bindable movement and selection
variants, focus and activation, field editing, cancellation, document/file/tab
actions, search and replacement, syntax, recovery, stream actions, and Settings
commands. Include actions with no shipped shortcut, including existing controls
that implement actions inline, such as search option toggles, insertion of
literal Tab/LF/CRLF into a query, picker parent/page navigation, and modal
Save, Discard and Retry. Protected decisions require their existing live
prompt, enabled target and visible geometry. A direct binding to a protected
decision requires focus on its corresponding control, just as Activate does.
For parameterized actions such as selecting a row, bind the action on the
current row. Document every ID with its permitted contexts and default keys.
Use IDs independent of display labels and Rust enum names. Mouse gestures,
committed text, signals, and terminal protocol events remain separate inputs.

| Context | Active surface and command scope |
| --- | --- |
| `editor` | Document content owns focus. Include caret/selection movement, copy/cut/paste, undo/redo, insertion/deletion, and moving focus out of content. |
| `controls` | Workspace buttons and tab controls own focus, with no overlay. Navigation, activation and cancellation act on those controls. |
| `search` | Search panel focus, including its text fields and controls. Local commands edit the focused field or navigate/activate panel controls. Search commands available from workspace remain available here. A search panel visible with focus in content uses `editor`. Replace All confirmation uses `dialog`. |
| `picker` | File-picker modal. Local commands include list and page movement, focus, activation, path-field editing and cancellation. |
| `dialog` | Help, Actions, tab list, confirmations, recovery, syntax selectors, and other dialogs. Navigation and activation act on the top surface. Commands never bypass a pending confirmation. |
| `settings` | Settings and its own confirmation/diagnostic views. Local commands include navigation, activation, cancellation, and the Settings actions described below. |
| `global` | The fallback in `editor`, `controls` and `search` for app-level actions such as Open, Help, Actions, Settings, Quit, Find and tab switching. Preserve the command availability rules of those surfaces. Under `picker`, `dialog` or `settings`, the modal owns input and suppresses this fallback. |

Resolve decoded keys at event dispatch against the current applied revision and
the current top surface. Try the active context first, then eligible global
bindings. A locally bound key is consumed even when its command is unavailable
at the focused control. A modal miss cannot dispatch into the covered surface.
Keyboard commands respect the same focus, capability, busy and confirmation
checks as button activation. Unbound printable input goes to the active
editable field or document normally. Block printable keys with only Shift or no
modifier as configurable shortcuts, including Space, to preserve text entry.
An unbound control key does nothing. Resolve context on the UI thread after
decoding, so queued undecided keystrokes cannot carry old command mappings.
Modal context changes, resize, and settings publication reconcile focus and
existing pointer capture before accepting input for a new surface. Input already
dispatched retains its captured target and authority.

Each list replaces that command's defaults in that context. An absent entry
inherits defaults. An empty list unbinds a command in that context. Removing an
override restores inheritance. Resolve collisions on the complete effective map
after applying all overrides. A normalized chord can have one owner per context.
Reject conflicts between a local and an eligible global binding, with both command
IDs and the context in the error, so a user must explicitly move or clear the
other binding. Use the catalog's static eligibility for these checks across
all contexts. Current focus, a tab loading, or a disabled action cannot make
a conflict acceptable. Reuse across disjoint contexts and with suppressed
global bindings is valid. Reject duplicates within a list after normalization.

### Chord spelling and terminal delivery

Use plus-separated modifier names and one key, for example `Ctrl+Shift+S`,
`Cmd+S` or `Shift+Tab`. Accept ASCII case-insensitive names, with canonical
modifier order Ctrl, Alt, Shift, Cmd. `Super` is an alias for `Cmd`; comparisons
retain all modifier bits. Support modified ASCII letters/digits, modified
`Space`, and named keys `Escape`, `Enter`, `Tab`, `Backspace`, `Delete`,
`Left`, `Right`, `Up`, `Down`, `Home`, `End`, `PageUp`, `PageDown`,
and `F1`–`F12`, with or without modifiers for named keys. Reject repeated or
unknown modifiers, unknown keys, and sequences. Normalize delivered ASCII
letter case and legacy BackTab to Shift+Tab, without dropping Ctrl, Alt, Shift
or Cmd. Show the canonical spelling in every UI. Reject unsupported captures
with an explanation. Continue to insert decoded printable Unicode normally.

Use the Stage 03 negotiated input protocol and legacy decoding. Key releases
only affect capture bookkeeping. Protocol-reported repeats can drive movement,
text entry/deletion and focus navigation as allowed today. Opening dialogs,
applying settings, file actions, confirmation, and other one-shot actions
consume presses only. Legacy repeat bytes are indistinguishable from
individual presses. Preserve existing confirmation guards and the capture
transition rules below. Keep bracketed paste as data through the complete
frame, preserve SIGUSR1 cancellation during
reception and existing pre-commit cancellation, and keep stdin source bytes
separate from tty input. Prioritize the paste decoder and terminal ownership
before key capture or command resolution.

Document that displayed chords describe decoded events. A legacy terminal can
send the same byte for Ctrl+I and Tab, Ctrl+M and Enter, Ctrl+J and Enter, or a
keyboard Backspace configured to send Ctrl+H. Decode an ambiguous byte using the
existing mapping and warn for bindings that need enhanced input. In particular
`0x08` means Ctrl+H (Replace) and DEL means Backspace. Ctrl+Shift+letter, Cmd,
and some modified/function keys may be translated or intercepted. Show
delivered aliases conditionally in help text and keep Actions/buttons and
portable remapping available. Do not alter upstream settings. Retain Stage 01's
raw mode, disabled software flow control, and exact terminal restoration on
suspend and exit. The default Ctrl+C copies the selection to ede's private
clipboard and is harmless without a selection. Default Escape cancels the
innermost cancellable work or dismisses its surface and never dispatches Quit.

### Default map

Preserve these shipped bindings from Stage 08. Display Cmd aliases as delivered
shortcuts. Commands without listed shortcuts are unbound by default and
available through Actions, an existing focused control, or Settings.

| Commands | Portable defaults | Delivered Cmd aliases |
| --- | --- | --- |
| New, Open, Close Tab | Ctrl+N, Ctrl+O, Ctrl+W | Cmd+N, Cmd+O, Cmd+W |
| Save, Save As, Quit | Ctrl+S, F4, Ctrl+Q | Cmd+S, Cmd+Shift+S, Cmd+Q |
| Select All, Copy, Cut, Internal Paste | Ctrl+A, Ctrl+C, Ctrl+X, Ctrl+V | Cmd+A, Cmd+C, Cmd+X, Cmd+V |
| Undo, Redo, Go To | Ctrl+Z, Ctrl+Y, Ctrl+G | Cmd+Z, Cmd+Shift+Z |
| Find, Replace, Next, Previous | Ctrl+F, Ctrl+H, F3, Shift+F3 | Cmd+F, Cmd+Shift+H |
| Help, Actions, Previous Tab, Next Tab | F1, F2, F7, F8 | |
| Move, extend selection | Arrows, Home/End, PageUp/PageDown; Shift plus movement extends selection in content | |
| Document Start/End, extend to document bounds | Ctrl+Home/End; Shift adds selection | |
| Newline, insert Tab, backward/forward delete | Enter and Tab in document content; Backspace and Delete where supported | |
| Next/previous control, activate, cancel | F6/Shift+F6; Tab/Shift+Tab in controls; Enter outside document content; Escape | |

Context determines which action owns each key. For example, Enter inserts a
newline in content, activates the focused control elsewhere, and submits the
current field where the existing UI supports submission. Tab inserts only in
document content. Shift+Tab navigates to the previous control and does not
insert a tab or unindent. Preserve existing picker movement, search navigation
and field editing in the corresponding contexts. Use distinct remappable IDs for
insertion and focus/activation, such as `insert_tab` and `focus_next`, and bind
Tab to the appropriate ID in each context. In `search`, F6/Shift+F6 bind
`focus_editor`, while Tab/Shift+Tab bind next/previous panel control. Keep the
editor F6 route into the visible search panel and the editor Cancel route to
dismiss or stop search when content owns focus. In `dialog`, bind F4 and
Cmd+Shift+S locally to Save As for the save and retention error surfaces.
Retry actions from error and details surfaces use those controls' existing
eligibility rules. Settings is available through Actions by default, has a
stable `settings` ID, and has no direct default chord. Use the picker and dialog
navigation and focus defaults in Settings.

Require an effective portable keyboard route from workspace to Actions and
through focus, activation and cancellation in every surface, including with
mouse capture off. Settings and guarded Quit remain Actions entries. Require
a direct portable Quit binding in the workspace so Quit can still be requested
below 40×8, when Actions is hidden. Validate that required steps retain at least
one legacy-decodable binding in their applicable contexts. An allowed portable
chord for this check is an unmodified named key,
Shift+Tab, or Ctrl+A–Z except Ctrl+H/I/J/M. This check ensures a route in the
default decoder. Actual delivery still depends on the terminal path. A user
can replace every command's defaults while retaining the route. Diagnostics
identify the step and context if a candidate removes the route.

## Settings UI and persistence

- Open **Settings** from Actions or its configurable shortcut. Show the
  effective config path, state (defaults, active user settings, or safe mode),
  and outstanding load/save diagnostic. List commands by context, including
  unbound ones. Show each effective chord and whether the entry inherits
  defaults or overrides them. Expose the four scalar settings with units and
  valid ranges.
- Provide **Add shortcut**, **Replace shortcut**, **Remove shortcut**, and
  **Use defaults** for the selected row. Start a capture explicitly. Consume one
  normalized supported press and show its canonical chord in a preview with
  Discard focused. Capture never dispatches the recorded key, including Enter
  and Escape. Accepting the preview updates the draft only. Also accept a typed
  chord in a field with the same validation. This provides a keyboard route to
  record the key that opened capture or one the terminal cannot deliver.
  Ignore repeats and releases as candidates. Suppress further presses of the
  opening and captured key, across the capture and preview transitions, until
  a distinct press or an enhanced release for that key. This handles held keys
  represented as presses by legacy terminals. A mouse Cancel control exits
  capture. A keyboard user can end capture by pressing a different key and
  discarding the candidate. Preserve the draft on resize and consume keys
  while capture is obscured. Ignore paste and signal/control events as capture
  candidates. Pasting settings text is allowed in focused input fields under
  normal field input rules.
- Edit a draft while the previous map stays active. Revalidate the entire draft
  and show conflicts before **Save Settings**. Support swapping and clearing
  chords in one draft. **Restore defaults** drafts removal of the general
  overrides, subject to the same explicit save. **Cancel** closes a clean
  draft. When discarding a modified draft or reloading over one, confirm first
  with Keep Editing focused. While a write is pending, show its progress and
  avoid submitting a duplicate. On failure retain the draft for correction or
  retry. On success show the newly applied values and derive help, menu
  shortcuts, control hints and small-terminal status text from the active
  revision. Usable text/scroll controls and visible keyboard focus are required
  at 40×8. Below that size show the effective Quit or Cancel shortcut for
  the current surface as space permits, invalidate pointer capture and
  preserve any pending confirmation without allowing approval. In a Settings
  capture obscured by resize, Cancel leaves capture without accepting a key.
  Return focus to the opener on exit where it still exists.
- TUI saves modify only the changed overrides and preserve existing comments and
  unrelated entries. Select a TOML editing approach at implementation that can
  do this and pass round-trip tests. Validate the actual serialized bytes and
  the 64 KiB limit before writing. A draft inherits the base file identity,
  mode, and bytes, or expected absence, from the applied revision's successful
  load or save. Opening Settings cannot silently rebase a draft onto newer
  disk bytes. After a successful reload or save use the new accepted base for
  the next draft. If the file started malformed, unreadable, or unsupported,
  offer external repair and explicit reload.
  Never overwrite that file from a draft based on fallback defaults.
- On save, use a nonblocking advisory lock shared by ede writers in the same
  config directory. Report Busy on contention. Open the resolved directory
  and anchor file operations there. Compare the current file identity, mode,
  and bytes with the draft base. Report a changed or newly created file,
  present **Reload Settings**, and preserve the draft until the user chooses
  what to discard. Only create the config directory and file for explicit save.
  Stage the replacement in a private 0700 directory inside the selected
  configuration directory. Preserve existing regular-file permission bits and
  use mode 0600 for a new file. Reject final symlinks,
  nonregular files, and multiply-linked files for UI writes. Stage, sync and
  publish the complete file atomically, using no-clobber publication when the
  file was absent. Remove unpublished temporary files on failure.
- After obtaining the publication grant below, recheck the expected
  destination's identity, mode, type, link count, and contents immediately
  before replacement. Check that the selected config directory still resolves
  to the staged directory. Concurrent ede writers serialize. Abort on any
  external change detected at the recheck. For an existing file, the final
  check and atomic rename are separate operations. An external writer can race
  that window by writing without the lock. Document this limit and the
  workflow for reloading after external edits before saving in Settings.
  If a post-publication error prevents confirming the result, report that the
  file may have changed and require reload before
  another save. Startup reads without a write lock and must handle a torn or
  invalid external write according to the validation rules.
- A failed save before publication leaves the active revision and prior file
  intact. Confirmed publication applies the complete candidate as one runtime
  revision. Serialize a save's grant to enter the final check and publication
  with cancel and shutdown. Before the grant, **Cancel Save** revokes the
  attempt and returns control immediately with the draft retained. If the grant
  wins, the operation finishes under its existing authority. Show **Finishing
  save** and allow the Settings surface to close while the worker finishes.
  A reload requested during save reads after the write completes. Accept and
  report a definitive result if it
  arrives before exit. Keep save/shutdown coordination independent of existing
  file/scratch Close and Quit confirmations. A stalled config worker must not
  delay tty restoration or process exit. If a worker remains stuck after the
  grant, the file may contain the old or complete new revision on relaunch.
  Process exit while filesystem I/O is stalled can leave an unpublished private
  staging directory.

## Applying a settings revision

- Apply the keymap, checkpoint interval, and highlighting limits together at
  the runtime boundary before dispatching another key. Tag asynchronous work
  with the applicable settings generation. The main settings revision and
  Stage 07 custom highlighting revision are loaded independently; changing one
  cannot discard the other's last good values. On any bad general config,
  preserve all previous general settings, including the cadence and limits.
- Changing the periodic scratch interval updates the running scheduler for all
  scratches, including later stdin-to-scratch promotions. Recalculate the next
  periodic due time from the time the current schedule started using the new
  interval. If already due, make the work eligible immediately. Let an in-flight
  sweep finish, then schedule the next sweep with the new interval. An
  unrelated reload cannot postpone a pending checkpoint. Preserve retry timing,
  completed acknowledgements, dirty revisions, and the final normal-Quit
  checkpoint and user-visible failure path from Stage 05.
- Apply byte limits to the entire exact logical text revision, including
  inactive and streamed tabs, with the Stage 07 BOM, LF/CRLF and equality rules.
  Apply the configured deadline to analysis and later projections in both
  editor and worker. When an applicable limit changes, revoke stale eligibility
  decisions, projections and outstanding jobs, then check the latest extent.
  A stale worker cannot publish over a tighter limit or latch a timeout after
  supersession. An increased limit within the allowed range can make an
  affected tab eligible again. Preserve manual language and theme choices,
  content, history, and editor responsiveness. Leave Stage 07 worker resource
  ceilings, Stage 08 spooling, and large-line navigation policy independent of
  these settings.

## Acceptance criteria

Use reducer and service tests for deterministic ordering and real files and
PTYs for publication, input delivery and terminal behavior. Write or extend
focused tests before implementation. Use explicit barriers for races and
verify changed guards with deliberate mutations in isolated copies per
[AGENTS.md](../../AGENTS.md).

| ID | Observable evidence required |
| --- | --- |
| K01 | Exercise config path precedence with absolute, empty, relative and unavailable bases. Verify missing config and existing Stage 07 customization need no migration. Check all numeric boundaries, syntax/schema errors, normalized duplicates, nonregular/symlink inputs, oversized reads and safe rendering of diagnostics. Gate startup I/O, show first content and use the default bypass, and prove late startup results cannot publish. Informational/invalid CLI invocations perform no settings I/O. |
| K02 | Audit Stage 08 commands and control outcomes against the documented catalog and the runtime, Settings and Help dispatch. Remap save, Quit, Settings, both search focus actions, field editing, movement and extend-selection. Verify Save As from save/retention errors and Cancel in content with search visible. Exercise absence, explicit unbinding, inherited-default conflicts with a temporarily disabled command, same-chord reuse in disjoint surfaces, and rejection of a broken recovery route. Test field text, disabled commands, clicks and actual guarded operations. A modal and a read-only stream must reject actions that they could reject before remapping, including stale activation after resize or a keymap swap. |
| K03 | With real PTYs, cover portable defaults and translated and enhanced key delivery, exact Shift/Cmd modifiers, legacy ambiguous bytes, repeats, releases, and missing capability replies. Run Ctrl+S with IXON initially enabled, no-selection Ctrl+C, Escape cancellation, paste containing remapped chords, and split pipe/tty startup. Verify suspend, resume and exit restore terminal state and retain the applied map. Sample a default tmux and remote SSH path and record which keys actually arrive. |
| K04 | At 40×8, use keyboard and mouse independently to discover bindings, remap via capture or a typed chord, edit scalars, reset, and save. Prove captured Enter/Escape, held legacy and enhanced keys, and pasted data cannot invoke commands or commit. Apply defaults/reset in one draft and verify live hints and reopened settings. Resize during capture, a dirty-draft discard confirmation, and guarded Quit. Verify Cancel and Quit reachability, preservation of drafts/work, and rejection of hidden approvals. Rerun picker, search, scratch and stream interaction regressions. |
| K05 | Reopen after a TUI save and obtain the same settings via file parsing. Verify preserved comments/unrelated overrides and permission handling. Force failure before and after atomic publish. Force external edits before opening the draft, then test create/replace, same-inode edits, chmod, new hard links, and directory replacement before the final guard. Control cancellation and shutdown before and after the publication grant, contention between two writers, and reload during save. Check file bytes, active revision, retained draft/diagnostic and cleanup. Deliberately break the destination, grant, and stale-result guards in isolated copies and verify the intended assertions fail. |
| K06 | Load a broken keymap and exercise guarded keyboard Quit with default bindings and no mouse. Launch safe mode with normal filenames, literal dash-prefixed paths, stdin and `--follow`. Verify no user settings or syntax query files are read or written, no hidden reset occurs, and scratch persistence and stage-specific CLI constraints work. |
| K07 | With a controlled clock and held checkpoint worker, shorten/lengthen the interval, issue unrelated reloads, and race a new cadence against a sweep, stdin promotion and normal Quit. Assert the next eligible time, exact recovered bytes and failed-Quit protection. Test both highlighting byte thresholds at equality and beyond, long offscreen lines, and configured deadlines with held workers. Exercise stale completions, relaxed bounds, invalid reload, and independent Stage 07 theme/rule reload. Verify typing, search and editing remain responsive with slow settings I/O. |
| K08 | Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, and `cargo test --workspace --locked` on macOS and Linux. Document config paths, schema, supported chord strings, contexts and command IDs, safe recovery, and legacy key delivery in the README. Record actual environments, outcomes and remaining Stage 10 terminal coverage here. |

## Validation record

- Independent design review against `a258b85` covered command ownership, capture
  transitions, tiny-terminal recovery, and publication. Separate implementation
  reviews against `c4bfcb0` covered input and Settings, filesystem publication,
  and runtime application. Follow-up checks found no material issues after fixes
  to focused confirmation, native path display, staging, and shutdown ordering.
- **K01 — Loading.** Unit and reducer checks cover XDG/HOME precedence, strict
  ranges and schema, printable diagnostics, missing files, FIFOs, symlinks, UTF-8
  and the exact file-size boundary. A held startup read cannot publish either a
  stale revision or a stale diagnostic after bypass. First text, the bypass
  control and paste cancellation were checked in the renderer/reducer. CLI tests
  cover flags and early errors. The Stage 07 config reload PTY passed.
- **K02 — Keymaps.** The README's command IDs, contexts, and defaults are checked
  against the actual catalog. Tests cover normalized collisions, inherited and
  empty lists, swaps, disjoint contexts, and portable recovery. Real PTYs remap
  Save, Quit, Settings, both search focus actions, field Backspace, movement,
  and selection. Reducer checks guard focused and visible Discard, error
  recovery Save As, and Actions and Help suppressing covered document commands.
- **K03 — Terminal.** Real PTY regressions cover portable keys, translated and
  enhanced delivery, repeats and releases, absent capability replies, IXON,
  clipboard and bracketed paste, split input, and exact tty restoration. Job
  control preserves a remapped Quit after suspend and resume. Isolated tmux 3.8
  passed on macOS. An SSH PTY into Ubuntu delivered Ctrl+S, F9, the CSI-u sequence
  for Cmd+Shift+S, Escape and remapped Ctrl+B. Saved bytes were checked after
  exit. Stage 10 owns the full physical keyboard, emulator, and transport matrix.
- **K04 — Settings.** Real 40×8 PTYs exercise keyboard scalar edit, save and
  relaunch, restoration of defaults, mouse entry, captured Enter, typed
  replacement via bracketed paste, resize during capture, and dirty confirmation.
  Reducer checks cover captured Escape and release, held presses, and preservation
  of a typed field across asynchronous reload. Picker, search, scratch and stream
  regressions passed.
- [macOS CI run 103](https://github.com/rapidclock/ede/actions/runs/45306751499)
  exposed a race between the Settings PTY test's standalone Escape and its
  immediate resize. PTY tests now wait for visible resize and confirmation
  states. Reducer checks dispatch the hidden Settings and Quit keys with an
  explicit small layout. On macOS arm64, all three affected PTY tests passed
  32 repetitions each, and the full suite passed with the CI descriptor limit.
- **K05 — Publication.** Real files and barriers cover stale base, same-inode
  edits, replacement, chmod, new hard links, symlinks, replaced directories,
  creation during publication, contention, cancellation and shutdown on both
  sides of the grant, and reload ordered after save. Tests verify private
  staging, late source changes, comments, modes, serialized round-trip, and
  uncertainty after a simulated directory-sync failure. Isolated mutations of
  the final destination check, grant, obsolete-read check, no-clobber publish,
  and staged-source guard failed their intended assertions. Independent review
  also verified a deliberate break in the focused Discard guard.
- **K06 — Safe startup.** Real PTYs cover rejected initial settings, last-good
  reload fallback, guarded keyboard Quit with mouse capture paused, bundled
  syntax and no user reload, and safe startup with an ordinary file, literal
  dash-prefixed file, pipe, and Follow. Config and source bytes were checked.
  Exposing the user config root in safe mode in an isolated copy failed the
  read-root assertion.
- **K07 — Runtime updates.** Controlled scheduler and checkpoint-worker tests
  cover a shorter and longer schedule, unchanged deadlines, an in-flight sweep
  and the exact final Quit checkpoint. A pipe PTY reloads the cadence during
  stdin promotion. Syntax tests cover whole-document and offscreen CRLF line
  equality, limits above the boundary, bad general reload, independent custom
  theme reload, worker deadlines, and stale timeout rejection. The settings
  envelope also passes during a held caret read fault.
- **K08 — Local verification.** Rust 1.99.1 on macOS 27.2 arm64 and Ubuntu
  26.10 aarch64 passed `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets --locked -- -D warnings`, and
  `cargo test --workspace --locked` under a 256-descriptor soft limit. The
  default suite passed 313 tests on macOS and 315 on Ubuntu, with 10 opt-in
  tests skipped on each. The macOS tmux smoke test ran separately. The Ubuntu
  suite ran in tmux pane `%0` from a checksum-verified isolated source snapshot.
