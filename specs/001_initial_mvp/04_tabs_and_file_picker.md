# 04 — Tabs and file selection

**Status** — Implemented and independently reviewed. See the validation record
for the tested environments and remaining compatibility coverage.

See the [MVP overview](overview.md). Depends on [Stage 03](03_editing_and_mouse.md),
merged through [PR #3](https://github.com/rapidclock/ede/pull/3).
Branch `stage/tabs_and_file_picker` targets `feat/initial_mvp`.

## Outcome and scope

Open several files and unnamed buffers in one terminal session. Switch and close
tabs with the keyboard or mouse. Each tab retains its text, undo/redo, caret,
selection, and vertical and horizontal scroll positions. Open provides an
in-app directory browser and direct path entry.

The stage delivers the MVP's **tabs and interactive file selection** requirement
(FR-05). It applies **basic editing and document integrity** (FR-02) to closing
and quitting with multiple documents, and **first-class mouse interaction**
(FR-03) to switching and closing tabs and operating the file picker. The
acceptance criteria below specify the Stage 04 behavior.

Stage 04 unnamed buffers are session-only. Dirty unnamed buffers require a save
or explicit discard on close and normal quit. Stage 05 adds scratch retention
and recovery. Directory browsing serves individual file selection. The
[MVP roadmap](overview.md#8-approval-and-stage-breakdown) governs later features.

## Tabs and startup

- `ede [--] [FILE ...]` accepts OS-native paths in argument order, including
  paths starting with a dash after `--`. Keep the existing help, version and
  usage-error behavior. With zero paths, create one empty unnamed buffer.
  Create one tab per distinct startup path in argument order, initially
  activate the first, and prepare files in that order. Show queued tabs
  as waiting to load. Report each failed path even if another tab succeeds.
  If all startup opens fail or are cancelled and the session is empty,
  create an empty unnamed buffer. An open of a missing path reports an error.
- New appends and activates an empty unnamed tab. Open of a new file appends
  and activates a tab, waiting if another file is preparing. When preparation
  starts, show Stage 03's bounded provisional preview when available and report
  progress. Only a Ready tab accepts editing or document clipboard actions.
  On Ready, initialize its caret and scroll positions at the document start.
  Close cancels the target's queued or running open. Escape cancels the active
  tab's queued or running open when no dialog owns Escape.
  Failed or cancelled loads remove only their own tab. When the active
  tab is removed, select the next tab to its right or, at the end, its
  left neighbor. An open finishing in the background keeps the user's
  current tab active.
- Reopening the same absolute OS-string path spelling focuses its existing
  tab. Resolve a relative path against the launch directory for CLI input
  or the displayed directory for picker input. Compare the native bytes of
  the resulting absolute spelling. Distinct spellings, including symlink
  aliases, may have independent tabs. Stage 02 save checks apply to each
  tab's own baseline. To reload a file explicitly, close it and open it again.
- Tab order is creation order. Closing an inactive tab preserves the active
  tab. Closing the last tab leaves an empty session with New, Open, and Quit
  available. Unnamed tabs get distinct session labels. Named tabs show a
  display-safe basename, with the full path available in the focused tab's
  details. The tab strip indicates active, modified, loading, saving and
  save-attention states even when labels are clipped.
- Switching restores the tab's caret, selection, preferred column and
  viewport, clamped to valid positions after a resize. Restore editor focus
  when switching from the Tabs dialog. Start a fresh layout if needed and
  show a pending view until it is usable.
- Preserve Stage 03's 40×8 minimum. Keep the active tab visible and make
  overflow reachable through a scrollable **Tabs** list accessible from
  Actions and a visible mouse control. Use the same prepared geometry for
  drawing and hit-testing. A stale press, release or drag after a tab switch,
  list change or resize cannot activate a different tab or place a caret in
  a different document.

| Action | Keyboard default | Mouse route |
| --- | --- | --- |
| New / Open | Ctrl+N / Ctrl+O, plus delivered Cmd+N / Cmd+O | Actions |
| Previous / Next tab | F7 / F8, wrapping at the ends | Click a tab, or highlight it in Tabs and click Switch |
| Close active tab | Ctrl+W, plus delivered Cmd+W | Active tab's close control or Actions |
| Close highlighted tab in Tabs | Focus Close and press Enter | Click Close in Tabs |
| Show Tabs | F2 then Tabs | Tabs control or Actions |

F6 / Shift+F6 moves between editor and controls. Tab inserts text in the
editor and moves focus in dialogs. In Tabs, arrows and Page Up/Down move the
highlight, Enter on the list switches, and Escape dismisses. Close in Tabs
targets the highlighted tab, including when it is inactive. Keep commands
discoverable in Help/Actions, with keyboard routes for intercepted shortcuts.
Modal input belongs to the dialog, including during resize and bracketed paste.

## Open dialog

- Open starts in the active named tab's parent directory or, for unnamed
  and empty sessions, the launch directory. Show the current directory, a
  focusable listing, Up, a path field, and Open and Cancel controls. Up goes
  to the parent of the displayed path and is disabled at root. Include dotfiles.
  A click highlights a row. Enter in the listing or activating the Open
  button submits the highlighted row. Enter in the path field submits the field.
  Up/Down and Page Up/Down navigate the listing, the wheel scrolls it, and
  Tab/Shift+Tab cycle controls. Everything remains reachable at 40×8.
- Activating a directory browses it. Activating a regular file opens it in
  a tab. A symlink is classified by its target and its original path is
  passed through to storage. Unsupported file types and broken links give
  actionable errors without reading their contents. Stage 02 validates the
  file again at open, including a path changed after listing.
- The path field accepts an absolute or current-directory-relative path.
  Existing directories browse and regular files open. Use literal path
  text, including spaces, with no shell or tilde expansion. Retain Stage 03's
  UTF-8 and 4096-byte field rules, including ordered paste and rejection
  feedback. Listing selection and CLI paths use OS strings directly, so a
  supported non-UTF-8 filename can be opened without entering its display
  label. Escape control characters in every displayed path and filename.
- Directory enumeration, metadata lookup and direct-path resolution run off
  the UI thread. Show loading and keep Cancel responsive. Missing paths,
  denied access, disappearing entries and read failures report the attempted
  path and retain the last usable listing and entered text. If the initial
  directory fails, keep Up and direct entry usable. A file that fails after
  admission reports its error from the loading tab and leaves other tabs
  intact. Cancel and newer requests revoke the earlier listing's authority
  to change the dialog.
- Use bounded pages of at most 128 entries, directories first and then by
  native filename bytes within each group. Provide keyboard and mouse access
  to subsequent and previous pages. A page may scan the directory off-thread
  with bounded retained candidates and cooperative cancellation. Listing
  reflects a changing filesystem and may differ between requests. No silent
  truncation at a fixed total entry count.

## Save, close and quit

- Save targets the active tab at dispatch. Keep the Stage 02/03 save policy,
  including guarded same-path Save, create-only Save As, and an exact saved
  content state. Save As retains its direct destination field, relative to
  the launch directory. Reject a Save As destination whose absolute spelling
  belongs to another open, waiting or loading tab. Reserve that spelling from
  admission until the save settles, including time in the save queue. During
  the reservation Open and other Save As attempts report that the path is busy.
  Update the saving tab's path only after confirmed Save As. A later edit
  stays dirty even if the save finishes while that tab is inactive.
- Close drains earlier accepted edits for the target before checking its
  state. Clean tabs close after any in-flight save outcome is reconciled.
  Dirty tabs get a path- or name-specific **Save / Discard / Cancel** prompt
  with Cancel focused. Save continues only once the tab's current content is
  confirmed saved, including completion of Save As for unnamed tabs. Failure
  or cancellation leaves the tab and its history open. Cancel restores the
  tab and focus that were active when close began.
- Quit drains earlier accepted edits and considers all tabs in tab order,
  including inactive dirty tabs and tabs with unresolved saves. Stop admitting
  new document edits while resolving quit. Use the same per-tab prompt.
  Defer discarding content or closing any tab until every required decision
  has resolved. Cancel aborts quit and preserves all unsaved contents and
  open documents, aside from loads that independently failed. Completed saves
  retain their ordinary effects. Once quit is confirmed, cancel remaining
  loads, restore terminal modes and finish worker shutdown off the UI path.
- Discard revokes the target's queued save intent and requests cancellation
  of any pre-commit save. Await and report its outcome before closing.
  Uncertain publication blocks automatic close or quit. Show that the
  destination may have changed and require a fresh acknowledgement tied to
  that save attempt before discarding. Preserve Stage 03's Save As/Cancel
  recovery from conflicts and unsupported metadata. Below 40×8, preserve
  pending prompts and wait for supported geometry before accepting decisions.

## Coordination and limits

- Keep one session-wide storage service and private clipboard. Store
  document state, view positions, edit history positions and save state
  against stable tab and document IDs. Route commands using the tab ID at
  dispatch. Keep in-progress paste tied to its original field or tab.
  Preserve per-tab input order. Match responses by tab/document,
  job and request identity. View results also match the revision and current
  layout generation. Reconcile committed edits and saves even for inactive
  tabs. Release stale snapshots and close storage created by cancelled opens.
- Use one running file preparation at a time across the session, including
  CLI opens. An open may copy and index a large file. Separate this work from
  the mutation and foreground-read paths so a blocked open cannot prevent
  viewing or editing a Ready tab. Prepare against the shared resource pool
  and transfer a prepared document to the storage owner only if its tab
  still exists and the open remains current. Run picker I/O with one worker
  and coalesce superseded listing requests. Storage uses a preparation worker
  alongside the mutation owner and immutable-read worker, with shared admission.
- Use one save worker across all tabs, with at most one pending save intent
  per tab. Service pending saves in admission order. Capture the target at
  dispatch and the acknowledged content at worker start, after edits accepted
  before the save request have settled. Show queued and saving states.
  Coalesce repeated normal Save. Reject a new Save As with busy feedback if
  that tab already has a pending intent or an in-flight Save As. While a Save
  As is queued, normal Save coalesces into it. A normal Save queued behind a
  running Save As uses the new path only if Save As succeeds. A failure
  stays visible and requires an explicit retry. Background save messages
  identify the tab.
- Use a 64-tab session limit, including queued, loading and
  cancelling opens until their resources are released. Reject additional
  opens or New visibly and keep current tabs usable. More than 64 startup
  paths is a usage error before terminal acquisition. Keep the Stage 02/03
  cache, queue and workspace admission budgets session-wide. Per-document
  history limits still apply. Charge metadata and pending path payloads
  against bounded budgets and surface working-space exhaustion without
  evicting a user's tab. Keep only the active text layout and its bounded
  replacement work; inactive tabs retain view positions without pinning
  rendered layouts.

## Acceptance criteria

| ID | Observable check |
| --- | --- |
| T01 | Launch with zero, one and several files, including invalid paths and a dash-prefixed name after `--`. Check order, errors, first usable tab and fallback. Create unnamed tabs, open files, reopen the same spelling, switch through overflow and close active, inactive and last tabs. Check the capacity error and resource reuse after close. |
| T02 | Give two tabs different contents, histories, Unicode selections, preferred columns and scroll positions. Switch and resize by keyboard and mouse at 40×8 and below. Verify only the intended document receives edits, undo/redo and saves, while internal copy/paste works across tabs. Exercise stale mouse events and modal focus. |
| T03 | Browse parent/child directories, hidden entries and several listing pages. Open by selection and by relative and absolute path. Test spaces, control characters, symlinks, native non-UTF-8 names where supported, unreadable or vanished entries, and unsupported types. Assert failed actions preserve existing tabs and that literal pasted paths use field input only. |
| T04 | Close a dirty named and unnamed tab with each prompt choice. Exercise quit with several dirty tabs, including Discard on an early tab and Cancel on a later one. Verify all tabs and unsaved bytes survive Cancel. Cover Save As cancellation/collision, save conflicts, pre-commit failure and uncertain publication. |
| T05 | Gate an open and directory read at controlled points. Verify the relevant UI can cancel promptly, and a blocked open permits switching to and editing a Ready tab. Release completions after switch, cancel, close and a newer picker request. Assert they neither steal focus nor revive tabs or install stale listings. |
| T06 | Gate a save of A, switch to B, edit/save B and make a newer edit in A. Release the save and verify acknowledged bytes, per-tab dirty state and save queue order. Gate close/quit against queued edits and an in-flight save, including an uncertain outcome, and verify the guard waits for the correct tab and save attempt. |
| T07 | Exercise the executable in a PTY with keyboard, mouse, paste, resize and terminal cleanup. Open the agreed 1 GiB fixture alongside other tabs and measure first usable text, ordinary input latency, peak RSS and shared resource high waters. Compare against the MVP's 1 s / 50 ms local goals and the Stage 03 single-tab baseline. Record environment, corpus, cache conditions and any remaining compatibility gaps. |

Use Rust checks through the normal entry points and explicit synchronization
for T05–T06. Before implementing changes to close/quit guards, reproduce their
failure with a focused test. After the baseline passes, deliberately break a
guard in an isolated copy and confirm the intended content-preservation
assertion fails. Run the Stage 02/03 regressions, formatting and Clippy.

## Validation record

Implementation and independent review used Stage 03 base `ea3db51`. Local
checks ran on macOS 27.2 arm64 with Rust 1.99.1, on a MacBookPro18,2 with 32 GiB RAM.

| Criterion | Evidence |
| --- | --- |
| T01 | CLI and PTY checks cover native arguments and `--`, distinct startup order, errors and fallback, reopening, switching, overflow, and closing through the last tab. Coordinator checks cover the 64-tab limit, native spelling and reuse after closing. |
| T02 | PTY checks exercise Unicode edits, tab-local undo, mouse selection, stale strip release, switching at 40×8, and guarded input below 40×8. Stage 03 text layout, view history, and clipboard regressions run with the shared session. |
| T03 | PTY browsing and literal bracketed-paste path entry pass. Filesystem tests cover bidirectional pages, dotfiles, control escaping, directory symlinks, a broken link, a FIFO, and a disappearing page. The non-UTF-8 filename test runs on Linux filesystems that support it. |
| T04 | PTY close checks cover unnamed Save As, cancellation, collision and success. A two-dirty-tab quit test verifies Cancel after an earlier Discard preserves both buffers. Save conflict, known failure, and uncertain publication are also covered in the storage/editor regressions. |
| T05 | Synchronized storage and coordinator checks cover independent edits during a blocked open, ordered requests for the opening document, closing a blocked tab, late Ready cancellation, and release of cancelled working storage. The picker check forces cancellation and replacement during blocked I/O and verifies stale replies cannot update the dialog. |
| T06 | Gated save checks verify FIFO, coalescing, Save As reservation, exact per-tab saved bytes, inactive dirty state, preservation of newer edits, guarded close waiting for its requested save, and a fresh acknowledgement for each uncertain save attempt. |
| T07 | Native PTY and isolated tmux 3.8 tests pass. The warm 1 GiB ordinary text run and managed usage probe are below. |

The multi-file CLI and PTY checks initially failed on Stage 03's single-file CLI,
replacement New, and direct-path-only Open. A synchronized test also reproduced
the blocked mutation lane. During Stage 04 review, focused tests reproduced
out-of-order requests to a loading document, hidden Escape accepting a decision,
and an older save consuming a newer close decision. The corrected cases pass.

Isolated mutations confirmed that the tests catch loss of each guard. Removing
the storage handoff check installed a cancelled document. Forcing early discard
lost `first-unsaved` when quit was subsequently cancelled. Bypassing the queued
save handoff prevented the guarded tab from closing after its requested save.
Accepting an older uncertainty acknowledgement suppressed the newer warning.

Formatting, Clippy with `-D warnings`, and
`cargo test --offline --locked --all-targets` passed. The independent reviewer
rechecked the material fixes and ran the library and PTY tests.

### 1 GiB ordinary text

The warm generated 1,073,741,824-byte file had 15,790,081 logical lines. The
release-mode PTY run used a second named tab and switched between the files
before measuring 40 ordinary edits on the large file. The filesystem was local
APFS, with the default clone attempt. Clone success was outside instrumentation.
Kernel peak RSS came from macOS `/usr/bin/time -l`.

| Run | First text ms | Ready ms | Input p50 / p95 / max ms | Sampled RSS KiB | Kernel peak KiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| Stage 03 single tab, Nov 4 follow-up | 156.022 | 702.432 | 17.322 / 27.200 / 34.534 | 149136 | — |
| Stage 04 with concurrent build work | 301.776 | 1210.978 | 20.424 / 76.994 / 83.734 | 140016 | 140128 |
| Stage 04 isolated run | 163.763 | 567.286 | 15.136 / 33.913 / 34.494 | 142704 | 142736 |

The isolated run met the 1 s first-text and 50 ms ordinary-input targets. The
concurrent run exceeded the ordinary-input target. The separate coordinator
probe ran the same-size ordinary corpus with two unnamed tabs beside the large
file. It measured peak shared cache 781,312 bytes, peak shared buffers
1,301,001 bytes, peak layout charge 7,518 bytes, and 1,091,469,312 allocated
working-store bytes.

Reproduce the PTY check with `EDE_BENCH_MIB=1024 EDE_BENCH_CORPUS=ordinary
EDE_BENCH_PEAK_RSS=1 cargo test --release --locked --test terminal
large_file_interactive_benchmark -- --ignored --exact --nocapture`.
For managed usage, use `EDE_RESOURCE_MIB=1024 EDE_BENCH_CORPUS=ordinary
cargo test --release --locked --lib
editor::tests::interactive_resource_and_incorporation_measurements --
--ignored --exact --nocapture`.

Native Linux and constrained-memory CI are tracked on the stage PR. Graphical
terminal key delivery, remote transport, and cold-cache latency remain outside
the local validation.
