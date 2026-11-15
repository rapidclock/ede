# 001 — Initial MVP overview

Status: **MVP scope and roadmap accepted — Stages 01–06 merged. Stage 07 design draft.**

Project: **ede**, a predominantly Rust terminal text editor for macOS and Linux.

Project branch: `feat/initial_mvp`.

This document captures the product scope and current design direction. It is not
authorization to implement the editor. Requirements will evolve through discussion
and implementation experience; proposals below remain proposals until agreed.

## 1. Purpose and intended experience

Build a lightweight terminal editor that is comfortable for everyday text editing
and useful for inspecting large files and live command output. Mouse interaction
is a first-class part of the interface. The application must also remain fully
usable through the keyboard and in remote terminal sessions.

The target experience combines familiar tabbed editing and search/replace with
pager-like navigation and log following. Use non-modal editing with familiar
Ctrl-based shortcuts and optional Cmd equivalents where the terminal delivers
them. The complete default keymap remains to be designed under FR-09.

Users initially compile and run the application themselves. Packaging, installers,
automatic updates, and release distribution are not MVP requirements.

## 2. Scope and requirement status

- **Required:** the functional requirements in section 3, including both viewing
  and editing large files and compatibility with remote terminal workflows.
- **Agreed starting defaults:** the highlighting heuristics, scratch checkpoint
  interval, and responsiveness goals below, subject to benchmark validation.
- **Proposed:** the architecture in section 4 and verification details not yet
  agreed in section 5.
- **Unresolved:** the decisions in section 6. These are not implied commitments.
- **Deferred/optional:** the capabilities in section 7.

Markdown means source editing and syntax highlighting, including YAML frontmatter
and fenced code blocks. A rendered Markdown preview is not required.

This iteration opens individual text files, not folders or project workspaces.
The file picker may navigate directories to locate a file, but opening a directory
as a workspace and recursively loading its files are outside this iteration.

The initial MVP package version is `0.0.1`. Development versions may use SemVer
prerelease identifiers such as `0.0.1-dev.1`, `0.0.1-dev.2`, and so on, preceding
`0.0.1`. This is the Cargo package version, separate from the Rust compiler version
and edition. No Cargo project is created during specification work.

## 3. Functional requirements

### FR-01 — Platforms and terminal lifecycle

- Run natively on macOS and Linux; Windows support is outside the initial scope.
- Run correctly in local terminals and through SSH, Eternal Terminal, or mosh,
  including inside local or remote tmux sessions. Test keyboard input, mouse
  interaction, paste, rendering, and resizing across supported configurations.
- Keep core editing and navigation available through a portable keyboard baseline.
  Enhanced terminal features must have usable fallbacks.
- Respond to terminal resizing, including small terminals, without losing document
  content or making essential controls inaccessible.
- Restore terminal modes and mouse capture on normal exit and recoverable error
  paths. Specify suspend/resume and interrupted-session behavior in later detail.
- Treat network latency as an operating condition: avoid unnecessary output and
  input handling that waits on unrelated background work.

Compatibility depends on the complete terminal path. The editor cannot receive
keys or mouse events intercepted upstream. Document supported configurations and
test them; do not claim universal support for every protocol/version combination.

### FR-02 — Basic editing and document integrity

- Create a document, open an existing file, insert and delete text, select ranges,
  cut/copy/paste, and navigate by character, line, and page.
- Support undo/redo and save/save-as. Mark modified documents and protect unsaved
  changes when closing tabs or exiting. Preserve scratch documents across normal
  application exits without requiring an explicit save (FR-10).
- Keep cursor placement, selection, and text rendering consistent for tabs,
  combining characters, and wide Unicode characters.
- Preserve supported encoding and newline conventions according to an explicit
  policy. Do not silently replace undecodable bytes and save corrupted content.
- Binary formats are unsupported. Reject detected binary files with a clear
  message such as "Cannot open binary file" and leave the file unchanged. Define
  detection and unsupported-text-encoding behavior separately so an encoding error
  does not automatically imply binary content. No binary viewer or hex editor is
  required.
- Report open/save failures clearly and preserve unsaved work on failure.
- Detect changes saved by another process and automatically refresh clean named
  editing tabs, including inactive tabs. Refresh requires no command, tab
  switch, or terminal focus change. Preserve the caret and viewport where
  possible and show brief feedback when the refresh completes.
- Preserve unsaved edits if the file changes on disk or while a refresh is in
  progress. Keep a visible on-disk change warning while the conflict remains
  and retain the guarded save policy. Automatic refresh of clean tabs and
  protection of modified tabs are required for MVP release.
- Define safe saving behavior for permissions, symlinks, hard links, and file
  replacement before implementation of file persistence.

### FR-03 — First-class mouse interaction

- Click to position the cursor and drag to select text, including selection across
  lines and autoscrolling when dragging beyond the viewport.
- Support wheel scrolling and mouse access to visible navigation controls.
- Make tabs, file selection, menus, dialogs, search options, and settings operable
  with the mouse. All essential actions must also be keyboard accessible.
- Keep selection and controls accurate after scrolling, resizing, and switching
  tabs, including with wide characters and tab expansion.
- Define how application selection, terminal-native selection, tmux copy mode,
  and clipboard integration coexist. Internal copy/paste must remain available
  when local system clipboard integration is unavailable.

Double-click word selection, triple-click line selection, and draggable scrollbars
are proposed interaction details, subject to review.

### FR-04 — Syntax highlighting and language expansion

- Target initial support for Markdown, TOML, YAML, JSON, JavaScript, TypeScript,
  Python, Rust, INI, XML, HTML, and CSS. Discuss and agree any language or embedded
  syntax deferrals with the user if their complexity is disproportionate to the
  MVP; do not silently reduce this target list.
- External highlighting libraries, grammar packages, or tools are acceptable if
  lightweight, performant, and well maintained. Select the approach together with
  the user, including its installation/build needs and maintenance tradeoffs.
  Bundle the agreed syntax assets or document agreed external prerequisites;
  routine highlighting must work offline without a language server.
- For Markdown, highlight source structure, YAML frontmatter, inline code, and
  fenced code. Use the fence language when a corresponding bundled language is
  available; use a readable fallback for unknown languages.
- Define the Markdown dialect explicitly. CommonMark plus GitHub-style tables,
  task lists, and strikethrough is the current proposal. “Full Markdown” does not
  mean every incompatible extension or a rendered preview.
- Account for embedded languages such as JavaScript/CSS inside HTML.
- Separate file detection, syntax definitions/queries, embedded-language rules,
  and theme styling so adding a language does not require rewriting the UI.
- Allow a manual language override for ambiguous filenames and piped content.
- Degrade or disable highlighting when necessary to preserve responsiveness;
  explain the change visibly. Highlighting limits must not make a file uneditable.

Exact grammars, dialects, theme defaults, and extension-loading mechanisms remain
open. Adding a native grammar may initially require recompilation.

Agreed initial highlighting heuristics (configurable and subject to benchmarks):

| Trigger | Behavior |
| --- | --- |
| Document exceeds 20 MiB | Disable highlighting for the document |
| Any document line exceeds 64 KiB of text | Disable highlighting for the document |
| A background parsing operation exceeds 250 ms | Cancel parsing and fall back to plain text |

These are highlighting limits, not file-opening or editing limits. Use a whole
document plain-text fallback rather than a column cutoff that loses syntax
context. Show the reason in the UI and avoid repeatedly retrying an expensive
parser automatically. Keep parsing off the interactive UI path and discard stale
results after edits. Library-specific cancellation behavior needs validation.

### FR-05 — Tabs and interactive file selection

- Open multiple documents in tabs. Show the active document and modified state.
- Switch and close tabs using the mouse and keyboard, preserving each document's
  content and its cursor/viewport state.
- Provide an in-application file picker with directory navigation, file selection,
  and a way to enter a file path directly. Directory navigation is only for
  locating files; it does not open a folder as a workspace. Handle permission,
  missing-file, and directory-as-file errors clearly.
- Make ordinary files, large files, stream documents, and persistent scratch
  documents distinguishable without requiring users to learn unrelated navigation
  interfaces.

Folder/workspace opening, a permanent project tree, fuzzy project search, and
split panes are outside this iteration.

### FR-06 — Large-file viewing and editing

- Allow both viewing and editing large files, including files larger than available
  RAM. A mandatory read-only cutoff is not an acceptable final MVP solution.
- Bound resident text caches and background queues. Account separately for index
  metadata, edit history, syntax trees, search results, and temporary disk usage.
- Display initial content without waiting for a full-file scan or complete line
  count. Show indexing/progress state where an operation needs unexplored data.
- Render only needed content and handle extremely long lines without allocating
  or laying out the entire line for every frame.
- For exceptionally long lines, default to horizontal scrolling rather than soft
  wrapping, initially using the 64 KiB line threshold from FR-04. Keep all text
  accessible and editable; disabling wrapping must not truncate the document.
- Keep UI input responsive while indexing, searching, replacing, and saving.
  Long operations need progress and cancellation where interruption is safe.
- Search across storage boundaries and preserve correct selections and edits even
  when the relevant text is not resident in memory.
- Support undo/redo and saving for large-file edits with explicit resource policies.
- Allow reduced highlighting and other costly derived features independently of
  editing. Set measurable limits after representative benchmarks.

Fast editing does not imply instantaneous full-file operations. A full search must
inspect data, and saving a changed file may require writing the complete result.
Disk-backed storage trades RAM demand for disk I/O, metadata, and temporary space.

### FR-07 — Piped input, pager use, and live following

- Accept piped stdin, including finite command output and a producer that continues
  writing, for example `tail -f app.log | ede`.
- Keep interactive terminal input separate from stdin document data. Show content
  as it arrives rather than waiting for end-of-input.
- Provide pager-style navigation, selection, search, and an explicit follow mode.
- Allow pausing/resuming following without confusing it with pausing ingestion.
  Scrolling away from the bottom must not repeatedly force the viewport back down.
- Handle partial lines and multibyte characters split across reads.
- Define retained history and spill-to-disk limits. When storage is exhausted,
  report and apply an explicit policy rather than silently dropping content.
- Define behavior on producer EOF/error, quitting while a producer is active, and
  loss of a controlling terminal.
- Specify direct following of a file as well as piped input, including the policy
  for truncation and log rotation.
- Keep terminal control output out of redirected document output. Define
  noninteractive behavior before claiming compatibility as a `cat` replacement.
- Keep piped content read-only while stdin is still active. Navigation, selection,
  search, and follow/pause remain available; pausing follow does not enable edits.
- Once stdin reaches EOF, make the received text available as an editable unnamed
  document that can be saved to a chosen file. Changing this document does not
  modify the producer's source files.
- Concurrent ingestion and editing, and editable snapshots taken before EOF, are
  outside this MVP. An indefinitely running producer remains a read-only stream.

The intent is a lightweight stand-in for common `less`/`more`/`cat` workflows, not
complete command-line or flag compatibility with those programs.

### FR-08 — Search and replacement

- Search the current document or all open documents, including unsaved edits.
- Provide literal/exact-text search and a case-sensitivity option, next/previous
  navigation, and visible match feedback.
- Distinguish literal matching from whole-word matching; clarify the latter's MVP
  status before detailed design.
- Replace the next match or all matches in the chosen scope. Show affected counts
  and make document changes undoable.
- Include large files in search and replacement without requiring a full in-memory
  copy. Bound result storage and make long searches cancellable.
- Search retained stream history with an explicit scope. Disable replacement for
  active streams; enable it for piped text after EOF, when the document becomes
  editable. Multi-document replacement must identify and skip read-only streams.
- Specify partial failures, cancellation, and per-document undo for multi-document
  replacement before implementation. Do not imply one atomic cross-file disk save.

Regular expressions and searching unopened project files are optional extensions,
not initial requirements.

### FR-09 — Configuration and keybinding remapping

- Make every user-facing command remappable, including navigation, editing,
  search, file actions, and settings access.
- Support remapping through a configuration file and through the TUI. Persist UI
  changes so the two mechanisms describe the same effective configuration.
- Define context/precedence rules so editor, file-picker, and dialog bindings do
  not conflict unexpectedly. Validate conflicts and report malformed settings.
- Keep a recovery route for an unusable configuration or keymap.
- Provide portable defaults and usable alternatives when terminal protocols cannot
  distinguish a requested shortcut. Do not depend on macOS Command-key delivery.
- Use familiar non-modal defaults, including Ctrl+S for save, Ctrl+F for find, and
  Ctrl+Z for undo. Offer Cmd equivalents where supported without depending on them
  for essential commands or changing OS/terminal shortcut settings automatically.
- Ctrl+C copies selected text. Escape cancels the current cancellable operation
  or dismisses its UI; quitting uses a separate command and shortcut. Specify
  behavior when nothing is selected and stream cancellation during detailed design.
- While the editor owns the terminal, configure raw input and disable software
  flow control so Ctrl+S saves instead of suspending terminal output. Restore the
  previous settings on exit and suspend; reapply editor settings on resume.
- Avoid known OS, terminal, and default tmux shortcut conflicts. Upstream shortcuts
  may consume events before they reach the editor; document this limitation,
  provide alternate bindings, and support user remapping rather than claiming
  compatibility with every custom configuration.
- Expose current commands and their effective bindings through discoverable UI.

TOML configuration, a command palette, and an input recorder for diagnosing remote
key delivery are current proposals.

### FR-10 — Persistent scratch documents

- Create and edit scratch documents without choosing a filename or save location.
  Support multiple scratch documents in tabs with the usual editing and search
  capabilities.
- Automatically persist scratch contents in application-managed storage. Quitting
  normally must preserve all current contents without requiring explicit saving,
  naming the document, or answering a save prompt for each scratch document.
- Schedule a checkpoint every **5 seconds while modified**, and finish persisting
  current contents on normal exit. The interval is configurable. Keep checkpoint
  work off the interactive UI path and avoid rewriting unchanged content.
- After an application crash, restore the last successfully completed checkpoint.
  Changes not included in that checkpoint may be lost. An interrupted checkpoint
  must leave the previous complete checkpoint usable. This does not promise a
  five-second maximum loss window when storage is slow or failing, or guarantee
  survival of power loss or storage failure.
- Restore retained scratch documents and their contents on the next launch,
  including their tab order, active tab, and cursor/viewport positions.
- Allow saving a scratch document as a named file and explicitly discarding one.
  Quitting the application must not be treated as discarding scratch documents.
- Report persistence failures and offer a way to preserve the content before exit;
  do not silently quit after failing to retain it.
- Keep persistence scoped to the user on the machine running the editor. In a
  remote session, scratch storage lives on the remote machine; cross-machine
  synchronization is not required.

[Stage 05](05_scratch_recovery.md) defines machine-local storage, independent live
owners, restoration of retired owners, and fresh undo history on recovery.
Normal-exit retention and restoration of the last complete checkpoint after an
application crash are required; power-loss durability is not promised.

## 4. Proposed architectural direction

Use Rust for the editor core, terminal integration, and tests. Keep auxiliary
scripts small; use Python with `uv` only where appropriate.

Separate terminal input/rendering, interaction state, document operations/storage,
and background services. Route keyboard and mouse actions through the same named
commands. Background work should use document revisions so stale results cannot
overwrite current state. Changes should be expressed as undoable transactions.

Stage 01 selects Ratatui with Crossterm; its implementation is merged and dependency
versions are recorded in the stage design. The [Stage 07 design](07_syntax_highlighting.md)
proposes Tree-sitter with bundled grammars and compares it with Syntect. Engine
and dependency selection remains open for user approval after feasibility checks.

Prefer reuse of ripgrep's Rust libraries where they fit editor search. Evaluate
`grep-searcher`, `grep-matcher`, and `grep-regex` before writing a custom engine.
Adapt search to the current document revision, including unsaved edits, scratch
documents, and paged storage, rather than searching only the underlying saved file.
Validate chunk-boundary matches, case handling, long-line memory use, cancellation,
and match offsets used for replacement. Reusing these libraries does not require
the external `rg` executable or add regex search to the user-facing MVP scope.

Evaluate a paged, disk-backed piece tree for large-file editing: reference stable
original ranges and append-only added text, caching only needed blocks. A piece
tree does not automatically solve source stability, bounded metadata, or saving.
Compare one shared editing model against an ordinary-file backend such as Ropey
plus a large-file backend before committing to either approach.

Keep live ingestion distinct from editing a stable revision. Define a snapshot or
conflict strategy before referencing mutable external file content as document
storage. Avoid assuming that an open file descriptor or memory map freezes data.

## 5. Quality and verification expectations

- Use representative source/configuration files and Markdown fixtures, including
  frontmatter and mixed-language fences, to verify bundled language behavior.
- Exercise Unicode placement/selection, newline preservation, undo, save failures,
  external changes, and replacement across chunk boundaries.
- Verify binary-file rejection and directory-as-file errors without modifying the
  source or disrupting existing documents.
- Verify scratch contents survive quitting without saving and are restored on
  relaunch. Cover multiple scratch tabs, save-as, explicit discard, and persistence
  failures without silent content loss.
- Verify application-crash recovery from the last complete scratch checkpoint,
  including interruption during a subsequent checkpoint write.
- Verify active pipes remain read-only and become editable after EOF, without
  losing received text. Pausing follow must not enable editing or replacement.
- Benchmark ordinary files, multi-gigabyte files, millions of short lines, very
  long lines, and sustained streams. Specify hardware and cache conditions when
  defining memory, CPU, first-display, and interaction latency targets.
- The agreed initial large-file benchmark is viewing and occasional edits to a
  **1 GiB text file on a machine with 8 GiB RAM**. This is a validation target, not
  a hard file-size limit or permission to consume all available RAM. Record CPU,
  storage, file shape, and warm/cold cache conditions with the measurements.
- First-display latency measures launch/open until the first usable text appears;
  input latency measures an available key/mouse event until its visible result.
  The agreed initial goals are **first usable text within 1 second locally** and
  **ordinary input response within 50 ms**, to validate on the benchmark baseline.
  These are engineering goals, not measured results or universal guarantees.
  Separate editor processing time from additional remote connection latency.
  Application memory budgets and measurement details remain to be designed.
- Verify whole-document highlighting fallback for the size, line-length, and
  parsing-time heuristics while retaining editing and search functionality.
- Verify local and remote terminal combinations for mouse, keys, paste, resizing,
  latency, detach/reattach, and reconnect behavior. Terminal compatibility needs
  integration/manual evidence beyond ordinary unit tests.
- Native Linux and macOS GitHub Actions jobs run formatting, Clippy, and tests
  using the pinned toolchain and locked dependencies. Stage 01 removes the
  specification-only manifest guard.
- Choose meaningful unit, functional, and system tests during stage design. An
  arrangement where the agent writes tests and the user implements the editor is
  a possible future collaboration model, not an assumption.

## 6. Decisions still requiring discussion

| Area | Decision to resolve |
| --- | --- |
| Terminal matrix | Named terminal apps, minimum versions, tmux/connection versions, and supported nesting combinations |
| Interaction | Remaining default bindings, Ctrl+C without selection, menus, mouse gestures, clipboard fallbacks, and small-screen behavior |
| Text formats | Encoding baseline, invalid UTF-8 handling, binary detection/rejection, newline policy, and INI/Markdown dialects |
| Highlighting | User-agreed engine/dependencies, bundled or external prerequisites, and any language/embedded-syntax deferrals |
| Storage | Unified or separate backends; stable snapshots; indexing and undo resource budgets |
| Saving | Atomic replacement policy, metadata/links, cancellation, disk exhaustion, and external-change conflicts |
| Scratch persistence | Resolved in Stage 05: machine-local state, independent live owners, recovery of retired owners, fresh restored history, and scratch-only recovery; power-loss guarantees remain outside scope |
| Live content | Direct file-follow semantics, log rotation, retention quotas, and producer-error handling |
| Search | Stage 06 uses bounded ripgrep literal matching, Unicode simple case folding, optional whole-word matching, and per-document atomic replacement; design and implementation authorized |
| Performance | Benchmark corpus and CPU/storage details for the agreed 1 GiB / 8 GiB RAM baseline; measurement methods, memory budgets, and validation of agreed latency/highlighting defaults |
| Dependencies | Libraries, grammar versions, compiler/toolchain baseline, and configuration locations |

## 7. Deferred and optional capabilities

- Python or other-language plugins are optional. A versioned external-process
  protocol is a candidate once commands and document edits are stable. A plugin
  runtime is not needed to make bundled language definitions extensible.
- Rendered Markdown preview, LSP/code intelligence, split panes, project-wide
  indexing, folder/workspace opening, binary viewing/editing, regex search, and
  packaging/distribution are outside the initial required scope unless later
  explicitly added.
- Editing while a pipe is active and creating editable stream snapshots before
  EOF are deferred. Piped text becomes editable after EOF (FR-07).

## 8. Approval and stage breakdown

On 2026-10-30, the user accepted the initial highlighting heuristics, five-second
scratch checkpoints, and responsiveness goals. The user subsequently approved
proceeding from the overview to the stage breakdown. The MVP scope is approved;
the architectural proposals and unresolved decisions remain open as identified
above. On 2026-11-01, the user approved the detailed Stage 01 design and explicitly
authorized its implementation. Stage 01 is now squash-merged into `feat/initial_mvp`
through [PR #1](https://github.com/rapidclock/ede/pull/1). The user authorized the
Stage 02 design while reviewing Stage 01, and explicitly authorized Stage 02
implementation on 2026-11-02. Stage 02 is squash-merged through
[PR #2](https://github.com/rapidclock/ede/pull/2). On 2026-11-03, the user authorized
beginning the Stage 03 specification on a new stage branch, then explicitly
authorized Stage 03 implementation after the Cmd and large-paste clarifications.

The user accepted proceeding one stage at a time: detailed design, review,
separately authorized implementation, validation, and stage PR before moving on.
Each detailed design must resolve its prerequisite decisions and define behavior,
interfaces, failure cases, and acceptance criteria before its implementation is
authorized. The stages may be adjusted as we learn.

Dependencies below are minimum functional dependencies. The proposed delivery
order is 01 through 10, with completed stages integrated into `feat/initial_mvp`
through stage PRs. Tests and failure handling belong in every stage; stage 10 is
the final integrated validation, not the first time correctness is checked.
Each stage specification ends with its validation record, including actual
results, tested environments, limitations, and agreed deferrals. Keep this
evidence in the stage file rather than a separate validation document.

### 01 — Terminal foundation

Design: [01_terminal_foundation.md](01_terminal_foundation.md). Depends on: none.
Status: complete; [PR #1](https://github.com/rapidclock/ede/pull/1) merged into `feat/initial_mvp`.
Evidence: [Stage 01 validation record](01_terminal_foundation.md#validation-record).

- Deliver a buildable Rust application shell with input, resize handling, focus,
  named command routing, and a place to report background work and errors.
- Complete when it starts/exits safely, restores terminal settings, receives mouse
  events, and handles Ctrl+S without freezing output. Select the TUI/toolchain and
  initial terminal test matrix here. Activate Rust CI and native macOS checks.
- Establish command and layout/hit-testing contracts so later UI controls share
  keyboard/mouse behavior. Basic remote and tmux checks begin here. Covers FR-01
  and the foundation of FR-03/FR-09.

### 02 — Document storage and file integrity

Design: [02_document_storage.md](02_document_storage.md). Depends on: 01.
Status: complete; [PR #2](https://github.com/rapidclock/ede/pull/2) merged into `feat/initial_mvp`.
Implementation was authorized on 2026-11-02 and completed on 2026-11-03.
Evidence: [Stage 02 validation record](02_document_storage.md#validation-record).

The implementation uses one paged piece tree with private stable source snapshots,
UTF-8 text with preserved BOM/newlines, bounded caches/history, and guarded atomic
saves. Loading time before editing, resource budgets, permission/link policies,
and external-writer limitations are documented in the stage design and validation
record. It delivers a testable storage core; interactive
editing remains Stage 03.

- Select the storage model and define document revisions, range access, edits,
  undo/redo, stable source content, and resource budgets before UI features depend
  on them. Resolve encoding, newline, binary detection, and safe-save policies.
- Complete when document-level checks can open/read/edit/save text, undo changes,
  reject binaries/directories, and preserve work through I/O failures and external
  changes. Demonstrate correct paging with a test file larger than its cache budget.
- Validate storage feasibility against the 1 GiB baseline early. This stage must
  support the larger-than-RAM design rather than leave a replacement engine for
  later. Covers FR-02 and the storage foundation of FR-06.

### 03 — Interactive editing and mouse selection

Design: [03_editing_and_mouse.md](03_editing_and_mouse.md). Depends on: 01–02.
Status: merged through [PR #3](https://github.com/rapidclock/ede/pull/3).
Unavailable environment validation remains pending.
Branch: `stage/editing_and_mouse`; PR target: `feat/initial_mvp`.
Evidence: [Stage 03 validation record](03_editing_and_mouse.md#validation-record) (local evidence recorded; remaining checks pending).

- Deliver single-document editing: cursor movement, insertion/deletion, selection,
  cut/copy/paste, undo/redo, save/save-as, and unsaved-change protection.
- Complete when keyboard and mouse actions agree on Unicode/tab placement;
  dragging selects across lines and autoscrolls; scrolling, resizing, and long-line
  horizontal navigation preserve selection and document content.
- Resolve mouse gestures and clipboard fallbacks here. Verify responsive viewport
  rendering over paged storage. Covers FR-02/FR-03 and interactive FR-06 behavior.
- Support macOS Cmd aliases where delivered, retain portable Ctrl bindings, and
  validate enhanced input, terminal mappings and intercepted-key fallbacks here.
  User remapping remains Stage 09.

### 04 — Tabs and file selection

Design: [04_tabs_and_file_picker.md](04_tabs_and_file_picker.md). Depends on: 03.
Status: complete; [PR #4](https://github.com/rapidclock/ede/pull/4) merged into `feat/initial_mvp`.
Branch: `stage/tabs_and_file_picker`; PR target: `feat/initial_mvp`.
Evidence: [Stage 04 validation record](04_tabs_and_file_picker.md#validation-record).

- Deliver multiple document tabs and a keyboard/mouse file picker with directory
  navigation solely for selecting files, plus direct path entry.
- Complete when users can open/switch/close files without losing per-tab state;
  modified tabs are protected and file-access errors leave other tabs usable.
  Tab and dialog controls must work with both keyboard and mouse. Covers FR-05.

### 05 — Persistent scratch documents, recovery, and automatic file refresh

Design: [05_scratch_recovery.md](05_scratch_recovery.md). Depends on: 02–04.
Status: complete; [PR #5](https://github.com/rapidclock/ede/pull/5) merged into `feat/initial_mvp`.
Branch: `stage/scratch_recovery`; PR target: `feat/initial_mvp`.
Evidence: [Stage 05 validation record](05_scratch_recovery.md#validation-record)
(native and Linux CI results recorded).

The accepted design gives each live instance an independent scratch set. A new
instance restores the retained scratches of all retired owners; overflow beyond
64 tabs remains available through Recovery. Restored undo history starts fresh.
Named-file refresh is undoable within the existing 256 MiB history budget, with
explicit feedback when that limit requires resetting history. General recovery
of unsaved named-file edits remains outside this stage.

- Deliver unnamed scratch tabs, five-second checkpoints while modified, restoration
  on launch, save-as, and explicit discard. Resolve storage location and concurrent
  editor ownership so instances cannot overwrite each other's recovery data.
- Complete when quitting/relaunching retains current contents and tab state, and
  forced termination restores the last complete checkpoint even when the next
  write was interrupted. Persistence failures must be visible. Covers FR-10.
- Automatically refresh clean named editing tabs after external saves, without
  interrupting editing or changing the active tab. Preserve local changes and
  warn on conflicts. The stage design must specify the refresh timing and
  behavior for undo history, temporarily missing or invalid files, and retries.
- Verify that in-place writes and rename-based saves refresh clean tabs on
  macOS and Linux without user input. Verify background tabs, save conflicts,
  and edits made while a refresh is in flight. Covers FR-02.

### 06 — Search and replacement

Design: [06_search_and_replace.md](06_search_and_replace.md). Depends on: 02–05.
Status: complete; [PR #6](https://github.com/rapidclock/ede/pull/6) merged into `feat/initial_mvp` at `9714b09`.
Implementation was authorized by the user on 2026-11-14.
The design was independently reviewed and findings fixed before implementation.
Branch: `stage/search_and_replace`; PR target: `feat/initial_mvp`.
Evidence: [Stage 06 validation record](06_search_and_replace.md#validation-record)
(native and Linux/macOS CI, review, mutation and performance evidence recorded).

- Deliver literal search, case sensitivity, next/previous, replace-next/all, and
  current/all-open-document scopes, including unsaved and scratch contents.
- Complete when matches and undoable replacements are correct across storage
  boundaries, long operations can be cancelled, and results stay within resource
  budgets. Resolve whole-word scope, case semantics, and partial replacement rules.
- Evaluate ripgrep libraries against document revisions and paged storage. Define
  read-only document handling for stream integration in stage 08. Covers FR-08.

### 07 — Syntax highlighting

Design: [07_syntax_highlighting.md](07_syntax_highlighting.md). Depends on: 02–06.
Status: design draft; engine selection and implementation authorization pending.
Branch: `stage/syntax_highlighting`; PR target: `feat/initial_mvp`.
Evidence: [Stage 07 validation record](07_syntax_highlighting.md#validation-record).

- Deliver offline syntax highlighting for all eleven FR-04 languages, including
  Markdown frontmatter and fences, and JavaScript and CSS embedded in HTML.
  Users can override language per tab and select a built-in syntax theme.
- Complete when fixtures highlight correctly, adding a language leaves the UI
  code intact, and all three agreed heuristics produce a visible whole-document
  plain-text fallback while editing and search stay available. Covers FR-04.

### 08 — Pipes, pager behavior, and live following

Planned file: `08_pipes_and_follow.md`. Depends on: 02–07.

- Deliver incremental stdin display, bounded ingestion/spooling, follow/pause,
  retained-history search, and direct file following. Resolve storage exhaustion,
  producer errors, controlling-terminal access, truncation, and rotation behavior.
- Complete when active input remains read-only, EOF makes piped content editable,
  and partial lines/characters are handled correctly. Verify input/UI separation,
  replacement restrictions, quitting with an active producer, and redirected-output
  behavior. Covers FR-07 and stream integration with FR-04 and FR-08.

### 09 — Configuration and keybinding UI

Planned file: `09_configuration_and_keybindings.md`. Depends on: 01–08.

- Deliver configuration loading/persistence and a keyboard/mouse UI for remapping
  all commands. Expose effective bindings and make the agreed highlighting and
  checkpoint defaults configurable.
- Complete when UI/config changes agree, context conflicts and malformed settings
  are explained, and a broken keymap has a recovery route. Finalize remaining
  default shortcuts and terminal alternatives without altering upstream settings.
  Covers FR-09 and configuration of FR-04/FR-10.

### 10 — Integrated MVP validation

Planned file: `10_mvp_validation.md`. Depends on: 01–09.

- Validate the complete workflow on macOS/Linux and the agreed local, SSH, tmux,
  Eternal Terminal, and mosh combinations. Include reconnect/resize behavior,
  clipboard fallbacks, checkpoint interruptions, and file/storage failures.
- Complete when all required FR-01–FR-10 outcomes have evidence; the 1 GiB/8 GiB
  baseline, long-line/stream workloads, and a larger-than-RAM file have been
  exercised; and measured latency/memory results are recorded. Resolve missed
  goals through fixes or explicit user-approved requirement changes.
- Verify automatic refresh of ordinary editing tabs and preservation of local
  edits during external saves before releasing the MVP.
- Supply concise build/use/configuration guidance and supported-terminal details.
  Finish at package version `0.0.1`, with development prereleases used beforehand.
  Distribution and deployment remain outside scope.

## 9. Research references

These explain candidate technologies; they do not constitute implementation choices.

- [Ratatui terminal backends](https://ratatui.rs/concepts/backends/)
- [Crossterm input events](https://docs.rs/crossterm/latest/crossterm/event/index.html)
- [Tree-sitter highlighting and language injections](https://github.com/tree-sitter/tree-sitter/blob/master/docs/src/3-syntax-highlighting.md)
- [Markdown grammar scope and limitations](https://github.com/tree-sitter-grammars/tree-sitter-markdown)
- [VS Code piece-tree design](https://code.visualstudio.com/blogs/2018/03/23/text-buffer-reimplementation)
- [tmux clipboard integration](https://github.com/tmux/tmux/wiki/Clipboard)
- [Mosh capabilities](https://mosh.org/)
- [ripgrep searcher library](https://docs.rs/grep-searcher/latest/grep_searcher/)
- [ripgrep regex matcher library](https://docs.rs/grep-regex/latest/grep_regex/)
- [Cargo package version](https://doc.rust-lang.org/cargo/reference/manifest.html#the-version-field)
- [Semantic Versioning prerelease identifiers](https://semver.org/#spec-item-9)
