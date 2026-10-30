# 001 — Initial MVP overview

Status: **Draft — awaiting user approval.**

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
pager-like navigation and log following. Familiar desktop-editor behavior is the
proposed interaction model; a particular default keymap has not been approved.

Users initially compile and run the application themselves. Packaging, installers,
automatic updates, and release distribution are not MVP requirements.

## 2. Scope and requirement status

- **Required:** the functional requirements in section 3, including both viewing
  and editing large files and compatibility with remote terminal workflows.
- **Proposed:** the architecture and baseline policies in sections 4 and 5.
- **Unresolved:** the decisions in section 6. These are not implied commitments.
- **Deferred/optional:** the capabilities in section 7.

Markdown means source editing and syntax highlighting, including YAML frontmatter
and fenced code blocks. A rendered Markdown preview is not required.

## 3. Functional requirements

### FR-01 — Platforms and terminal lifecycle

- Run natively on macOS and Linux; Windows support is outside the initial scope.
- Support local sessions and sessions through SSH, tmux, Eternal Terminal, and
  mosh, including combinations such as a remote connection with tmux.
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
  changes when closing tabs or exiting.
- Keep cursor placement, selection, and text rendering consistent for tabs,
  combining characters, and wide Unicode characters.
- Preserve supported encoding and newline conventions according to an explicit
  policy. Do not silently replace undecodable bytes and save corrupted content.
- Report open/save failures clearly and preserve unsaved work on failure.
- Detect relevant external changes and define an explicit reload/conflict policy;
  do not silently overwrite another process's changes.
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

- Bundle support for Markdown, TOML, YAML, JSON, JavaScript, TypeScript, Python,
  Rust, INI, XML, HTML, and CSS. Syntax highlighting must work without a language
  server or runtime grammar downloads.
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

### FR-05 — Tabs and interactive file selection

- Open multiple documents in tabs. Show the active document and modified state.
- Switch and close tabs using the mouse and keyboard, preserving each document's
  content and its cursor/viewport state.
- Provide an in-application file picker with directory navigation, file selection,
  and a way to enter a path directly. Handle permission and missing-file errors.
- Make ordinary files, large files, and stream documents distinguishable without
  requiring users to learn unrelated navigation interfaces.

A permanent project tree, fuzzy project search, and split panes are not implied by
the file-picker requirement.

### FR-06 — Large-file viewing and editing

- Allow both viewing and editing large files, including files larger than available
  RAM. A mandatory read-only cutoff is not an acceptable final MVP solution.
- Bound resident text caches and background queues. Account separately for index
  metadata, edit history, syntax trees, search results, and temporary disk usage.
- Display initial content without waiting for a full-file scan or complete line
  count. Show indexing/progress state where an operation needs unexplored data.
- Render only needed content and handle extremely long lines without allocating
  or laying out the entire line for every frame.
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
- Define a way to take received content into editing. A stable editable snapshot
  while a live tab continues following is proposed; editing the live source is not
  yet an agreed requirement.

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
- Search retained stream history with an explicit scope; do not ambiguously apply
  replacements to content that is still arriving.
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
- Expose current commands and their effective bindings through discoverable UI.

TOML configuration, a command palette, and an input recorder for diagnosing remote
key delivery are current proposals.

## 4. Proposed architectural direction

Use Rust for the editor core, terminal integration, and tests. Keep auxiliary
scripts small; use Python with `uv` only where appropriate.

Separate terminal input/rendering, interaction state, document operations/storage,
and background services. Route keyboard and mouse actions through the same named
commands. Background work should use document revisions so stale results cannot
overwrite current state. Changes should be expressed as undoable transactions.

Ratatui with Crossterm is the leading TUI candidate; Tree-sitter is the leading
highlighting candidate. These are research recommendations, not approved dependency
or version selections. Audit grammar coverage, compatibility, and licenses first.

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
- Benchmark ordinary files, multi-gigabyte files, millions of short lines, very
  long lines, and sustained streams. Specify hardware and cache conditions when
  defining memory, CPU, first-display, and interaction latency targets.
- Verify local and remote terminal combinations for mouse, keys, paste, resizing,
  latency, detach/reattach, and reconnect behavior. Terminal compatibility needs
  integration/manual evidence beyond ordinary unit tests.
- Establish native Linux and macOS checks as implementation begins. The initial
  simple GitHub Actions workflow runs formatting, Clippy, and tests on Linux once
  a root Cargo manifest exists; it explicitly reports a skip before then.
- Choose meaningful unit, functional, and system tests during stage design. An
  arrangement where the agent writes tests and the user implements the editor is
  a possible future collaboration model, not an assumption.

## 6. Decisions still requiring discussion

| Area | Decision to resolve |
| --- | --- |
| Terminal matrix | Named terminal apps, minimum versions, tmux/connection versions, and supported nesting combinations |
| Interaction | Default keymap, menus, mouse gestures, clipboard fallbacks, and small-screen behavior |
| Text formats | Encoding baseline, invalid UTF-8/binary handling, newline policy, and INI/Markdown dialects |
| Storage | Unified or separate backends; stable snapshots; indexing and undo resource budgets |
| Saving | Atomic replacement policy, metadata/links, cancellation, disk exhaustion, and external-change conflicts |
| Live content | Direct file-follow semantics, log rotation, retention quotas, and editable snapshots |
| Search | Whole-word semantics, Unicode case behavior, multi-document replacement and cancellation |
| Performance | Benchmark corpus, reference hardware, numeric targets, and highlighting budgets |
| Dependencies | Libraries, grammar versions, compiler/toolchain baseline, and configuration locations |

## 7. Deferred and optional capabilities

- Python or other-language plugins are optional. A versioned external-process
  protocol is a candidate once commands and document edits are stable. A plugin
  runtime is not needed to make bundled language definitions extensible.
- Rendered Markdown preview, LSP/code intelligence, split panes, project-wide
  indexing, regex search, and packaging/distribution are outside the initial
  required scope unless later explicitly added.

## 8. Approval and next planning step

No overview approval has been recorded. No stage breakdown or stage files are
defined in this draft.

After explicit user approval of the overview, add a stage breakdown here that
states the observable outcome at the end of each stage. Then elaborate stages in
`NN_stage_name.md` files, with enough detail for a human to implement them without
chat context. Discuss implementation authorization separately. Update this
overview as agreed requirements change rather than treating approval as a freeze.

## 9. Research references

These explain candidate technologies; they do not constitute implementation choices.

- [Ratatui terminal backends](https://ratatui.rs/concepts/backends/)
- [Crossterm input events](https://docs.rs/crossterm/latest/crossterm/event/index.html)
- [Tree-sitter highlighting and language injections](https://github.com/tree-sitter/tree-sitter/blob/master/docs/src/3-syntax-highlighting.md)
- [Markdown grammar scope and limitations](https://github.com/tree-sitter-grammars/tree-sitter-markdown)
- [VS Code piece-tree design](https://code.visualstudio.com/blogs/2018/03/23/text-buffer-reimplementation)
- [tmux clipboard integration](https://github.com/tmux/tmux/wiki/Clipboard)
- [Mosh capabilities](https://mosh.org/)
