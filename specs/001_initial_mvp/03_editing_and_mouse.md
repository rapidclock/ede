# 03 — Interactive editing and mouse selection

Status: **Local implementation and code review complete — environment validation pending.**

Parent: [MVP overview](overview.md). Depends on completed Stages 01–02.
Branch: `stage/editing_and_mouse`. PR target: `feat/initial_mvp`.
Baseline: Stage 02 squash-merged through [PR #2](https://github.com/rapidclock/ede/pull/2).
The user authorized this specification and, after the Cmd and paste-size
clarifications, explicitly requested Stage 03 implementation on 2026-11-03.
Initial defaults described below are used for that implementation; unresolved
compatibility checks remain in the validation record.

## Outcome and boundary

Connect the terminal shell to the paged document service. A user can open or create
one document, navigate and edit it, select with keyboard or mouse, cut/copy/paste,
undo/redo, and save without losing work through an ordinary failure or accidental
quit. Keyboard and mouse use the same text-to-cell mapping and named commands.
Large files remain editable once Stage 02 preparation finishes; no UI size cutoff
may turn them permanently read-only.

This stage covers FR-02, single-document FR-03, and interactive FR-06 behavior.
Tabs and directory browsing remain Stage 04; scratch retention/recovery Stage 05;
search, highlighting, streams and configuration remain their planned stages.
An unnamed buffer here is temporary and must have unsaved-change protection. It
does not yet satisfy FR-10; normal-exit scratch retention remains required in
Stage 05. Do not claim completion of the full MVP requirements at this stage.

## Entry, opening, and document lifecycle

**Initial interface:** `ede [--] [FILE]` accepts zero or one OS-string path.
No path creates an empty unnamed document. A missing supplied path reports an open
failure rather than silently creating a named file. More than one path is a usage
error (code 2) until tabs exist. Informational flags and pre-terminal validation
retain Stage 01 behavior; piped stdin/redirected stdout remain unsupported here.

Provide New and Open commands through a keyboard/mouse action menu. Open uses a
direct-path dialog; no directory listing or picker is introduced. Relative paths
resolve against the launch working directory; no shell expansion or execution.
Preserve `PathBuf` values independently of display-safe labels. Startup with an
open failure stays usable with the error visible and an empty unnamed buffer.

Opening shows Stage 02's bounded provisional preview and preparation progress.
Label preview as loading/read-only; do not accept edits or copy it as confirmed
document content. Escape cancels preparation. Ready replaces the preview with
the validated snapshot, initially at byte zero. Binary/encoding/type, space and
source-change failures report their distinct errors; retry is explicit.

For New/Open replacing an existing document, first resolve unsaved changes. Keep
the old document and view until the replacement is Ready. A failed/cancelled open
restores them. At most the current document and one pending replacement are held;
their storage budgets are aggregate. Cancel and close abandoned jobs so late Ready
results cannot install a document after a newer request or quit.

Header/status show the display-safe path or Untitled, modified state, loading/saving
state, cursor line/cell column, and errors. A failed cursor/line lookup shows a
pending/error indication, never invented coordinates. Show full error details in
a dismissible overlay when the status row cannot contain them.

## Text positions, navigation, and rendering

The existing storage offsets are `u64` logical UTF-8 bytes excluding BOM. Keep
those separate from zero-based line numbers, grapheme boundaries and screen cells.
Selection is a half-open range between an anchor and an active caret; store its
direction. Carets and selection endpoints must be grapheme boundaries and must
never split CRLF. LF/CRLF are one navigation/deletion unit; a standalone CR remains
text under Stage 02's policy.

**Initial display defaults:** unwrapped logical lines, tab stops every four cells
from the logical line start, and narrow East Asian ambiguous-width characters.
Horizontal scrolling applies to every line, including lines over 64 KiB. Soft
wrapping is not required by the accepted overview; revisit it separately if wanted.
No automatic indentation, tab-to-space conversion or Unicode normalization.
Provide Go To via Ctrl+G/actions: enter a one-based logical line and optional
one-based cell column as `LINE[:CELL]`. Reject invalid/out-of-range line numbers;
use the same nearest-grapheme cell mapping as mouse and vertical movement.

- Left/Right move one extended grapheme, crossing line endings at their boundary.
  Without Shift, an existing selection collapses to its start/end respectively.
- Up/Down move one logical line, retaining the preferred cell column across short
  lines; choose the closest valid caret boundary, with ties before the grapheme.
  Horizontal movement, clicking and edits reset the preferred column.
- Home/End go to logical line start/end, before the terminator. Document Start/End
  go to byte zero/EOF. Page Up/Down move by at least one line and otherwise by the
  visible text height minus one, moving the viewport by the same amount.
- Shift extends from the existing anchor for each movement. Select All selects
  the complete logical text without reading or allocating its contents.
- Keyboard movement/editing reveals the caret. Wheel scrolling moves only the
  viewport and preserves caret/selection; do not snap back on the next frame.
  Resize preserves logical positions and clamps scroll offsets.
- Click beyond a line's displayed end chooses its end, not virtual padding. Click
  below EOF chooses EOF. Tabs and wide glyphs have no interior caret positions:
  choose the nearer boundary, ties before. Highlight every occupied cell of a
  selected tab or glyph. Use the same rule for vertical movement and hit-testing.
- If a glyph is clipped by either horizontal edge, render safe placeholder cells
  with the same selection mapping rather than half a wide glyph. An isolated
  zero-width cluster gets a visible placeholder mapped to its original bytes.

Never send document control sequences to the terminal. Render C0/C1 controls,
ESC, DEL and directional formatting controls as visible escaped tokens; tabs and
newlines use their layout rules. Escaped tokens are atomic hit targets. Apply
equivalent escaping to paths and errors. Preserve underlying bytes in edits,
clipboard and saves. Unicode shaping/bidirectional layout is outside this initial
terminal model; render logical order and document terminal/font width limitations.

Use one revision-tagged immutable layout result for drawing, caret placement,
selection painting and cell hit-testing. A pointer event may act only on a frame
whose document/revision/geometry still applies. Invalidate and request fresh layout
after edits, resize, scroll, modal changes and resume; never place a caret from an
obsolete cell map. Pending layout must not block help, cancellation or quit.

## Editing and undo

Printable text inserts at the caret or replaces the selection in one transaction.
Enter inserts the snapshot's suggested LF/CRLF style; Tab inserts a literal tab
when text has focus. Backspace/Delete remove the previous/next grapheme or the
selected range, including an entire CRLF when crossing a line. At BOF/EOF they are
no-ops. After insertion/replacement, place the caret after the inserted bytes,
advancing to the next valid grapheme boundary if neighboring text merges with
them. Read-only loading state cannot mutate text.

Paste is data, including embedded control characters, never a series of commands.
Preserve supplied line endings as Stage 02 requires. Invalid inserted text/NUL,
storage exhaustion or cancellation before commit reject the entire edit and keep
the old document, caret and selection. No partial paste or cut is visible.

**Initial undo policy:** one delivered text-insertion event, deletion command,
cut, or complete paste is one undo unit; consecutive typing is not grouped in this
stage. Undo/redo restore the corresponding caret/selection and reveal it. Keep
view-history metadata aligned with actual storage transactions and pruning;
no-op edits add neither history entry. New successful edits clear redo; failed
edits retain it. Undo back to the saved content state clears the modified marker.
Copy, movement and scrolling do not create undo entries. Saving preserves history.
Report storage history pruning without claiming unlimited undo.

## Mouse and clipboard

Required mouse behavior:

- Left click focuses text, places the caret and clears selection. Shift-click
  extends from its anchor. Press-drag-release selects in either direction across
  lines; pointer capture stays with the text selection rather than activating
  header controls crossed during the drag.
- While dragging outside the text rectangle, continue selection by timer-driven
  autoscroll: proposed 50 ms ticks, one line vertically or four cells horizontally
  per tick. Clamp to document limits. Stop on release, focus loss, modal opening,
  suspend or resize; retain the selected range. Do not require fresh motion events
  to keep autoscrolling. Terminals may stop delivering events outside their window.
- Wheel Up/Down scroll three logical lines per event. Horizontal wheel events,
  when delivered, scroll four cells. Keyboard horizontal navigation remains
  available when the terminal cannot deliver horizontal wheel events.

**Initial gesture scope:** single-click, Shift-click, drag and wheel are required
here. Double-click word selection, triple-click line selection and draggable
scrollbars remain optional and are not claimed as delivered.

**Initial clipboard policy:** provide an application-owned clipboard backed by
private, bounded-buffer disk spooling, independent of the system clipboard. Ctrl+C
copies selected original bytes; without selection show “No selection” and do
nothing. Ctrl+X first stages the copy, then removes the selection atomically;
publish the staged clipboard only on successful deletion. Copy/spool/edit failure
leaves the document and previous clipboard intact. Ctrl+V
inserts the application clipboard as one undoable edit. An empty clipboard is a
visible no-op. Keep clipboard contents on document replacement, clear on process
exit, and apply private-file permissions and temporary-space admission.

[Stage 10 clipboard modes](10_mvp_validation.md#clipboard-setting-and-client-paste) build on
this policy. Internal and Both preserve the private clipboard behavior.

External text enters through the terminal's Paste action/bracketed paste. Without
bracketed paste, received characters behave as ordinary key input; do not promise
safe recognition of a paste the terminal does not identify. Stage 03 must support
external and internal pastes larger than 1 MiB without an arbitrary total-size
cutoff, subject to the existing text-validity and available-storage contracts.
All document paste routes normalize LF, CRLF, and standalone CR to the first
LF or CRLF in the destination, using the same rule as Enter. When the current
document has no LF or CRLF, both commands use the applied default line ending.
Tab Details shows the effective line ending for the selected editable tab.
Copy retains the exact selection, and fields accept the bytes their own input
rules allow.
Receive large bracketed pastes incrementally into private disk staging with bounded
buffers, then commit the complete paste as one edit. While `Receiving paste` is
displayed, use the shown `kill -USR1 PID` command from another shell on the editor
host to cancel the active frame. A legacy Escape key sends the same byte as
pasted Escape, and a paste can also contain the full enhanced Escape sequence.
The reader drains a cancelled frame through its closing marker. Once the
frame closes, Escape can cancel a pending paste before commit. A delivered Cmd+V key
invokes the same internal Paste command as Ctrl+V. If the terminal handles Cmd+V
itself and sends pasted text, insert that text once through the terminal-paste
path; do not also paste the internal clipboard. Likewise, a terminal-handled
Cmd+C copies its terminal selection, not necessarily ede's document selection.
Help must distinguish these behaviors. Cmd aliases do not by themselves provide
system clipboard access. Native clipboard APIs,
helper commands and OSC 52 are not selected by this stage. This provides a local
and remote fallback without assuming the editor can access the user's local
clipboard.

Offer a discoverable keyboard/mouse action to pause application mouse capture for
terminal-native selection and to re-enable it from the keyboard. Suspend/resume
must preserve the chosen capture mode. Explain terminal-specific selection
modifiers and tmux copy mode in help; do not assume one universal modifier or alter
tmux/terminal settings. Internal copy/paste works regardless of capture mode.

## Commands, focus, and dialogs

All actions use named command identities and shared binding/help metadata. Buttons
and menus dispatch the same commands. **Initial defaults** extend the existing
bindings; alternative menu access is required for intercepted shortcuts. Cmd
aliases use the same commands, focus rules and unsaved-change protection as their
portable equivalents. Stage 03 defines these defaults; Stage 09 adds remapping.

| Action | Portable default when editor text is focused | Additional macOS shortcut, when delivered |
| --- | --- | --- |
| New / Open | Ctrl+N / Ctrl+O | Cmd+N / Cmd+O |
| Save / Save As | Ctrl+S / F4 | Cmd+S / Cmd+Shift+S |
| Quit / Help / action menu | Ctrl+Q / F1 / F2 | Cmd+Q for Quit |
| Move / extend selection | Arrows, Home, End, Page Up/Down / Shift plus movement | Same portable controls |
| Document Start / End | Ctrl+Home / Ctrl+End | Same portable controls |
| Select All | Ctrl+A | Cmd+A |
| Go to line[:cell], from 1 | Ctrl+G | Same portable control |
| Insert newline / tab | Enter / Tab | Same portable controls |
| Delete backward / forward | Backspace / Delete | Same portable controls |
| Cut / Copy / internal Paste | Ctrl+X / Ctrl+C / Ctrl+V | Cmd+X / Cmd+C / Cmd+V |
| Undo / Redo | Ctrl+Z / Ctrl+Y | Cmd+Z / Cmd+Shift+Z |
| Next / previous control | F6 / Shift+F6; Tab / Shift+Tab in menus/dialogs | Same portable controls |
| Cancel | Escape | Escape |

### macOS Cmd delivery and fallback

**Agreed scope — 2026-11-03:** Stage 03 must support customary Cmd shortcuts where
the terminal delivers them, alongside the portable Ctrl bindings. Cmd support is
part of this stage, not deferred to configuration work. The exact default table
above is the selected initial default. Essential operations stay available when Cmd
delivery is absent or intercepted.

Handle both supported delivery routes:

- Enhanced keyboard events carry Cmd as the Super modifier. Preserve that modifier
  and map each listed combination explicitly; do not globally replace every Ctrl
  binding with Cmd or strip Shift from Cmd+Shift+Z/S. Accept delivered aliases on
  either supported host OS so a macOS terminal connected to a Linux host can use
  them; the server OS cannot identify the client's keyboard capabilities.
- A user-configured terminal mapping may translate Cmd+S to Ctrl+S, for example.
  Treat the resulting event as the existing portable command. Provide tested,
  optional mapping instructions for supported terminal setups; never modify
  terminal, OS or tmux settings automatically.

Use an enhanced protocol when supported, with a bounded capability check and
graceful fallback if unavailable or unanswered. Keep the single terminal reader;
capability responses must not become document text, and startup must not wait
indefinitely for a response. Crossterm 0.29 documents that receiving Super requires
[`DISAMBIGUATE_ESCAPE_CODES` via `PushKeyboardEnhancementFlags`](https://docs.rs/crossterm/0.29.0/crossterm/event/struct.KeyModifiers.html).
The [Kitty keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/)
identifies macOS Command as Super. Integrate protocol push/pop into terminal
ownership: restore acquired protocol state on exit, errors and suspend; re-enable
it on resume when supported. Do not leave enhanced input modes active in the shell.

Some Cmd combinations may remain assigned to terminal actions even when enhanced
input works. ede cannot dispatch a keypress it never receives. Help must list Cmd
aliases with that condition, retain the portable alternatives, and link tested
setup guidance. Record delivery per shortcut and full terminal/transport path;
neither terminal capability nor the host OS proves every Cmd shortcut is received.

### Focus and dialogs

Text focus consumes Tab as insertion; Shift+Tab does not silently unindent or
insert a tab. F6 gives a keyboard route out of text. Enter activates a focused
control rather than inserting text outside the editor. Modal contexts route text
to their field only; save/quit commands cannot bypass an unresolved modal prompt.
Escape first cancels a cancellable operation owned by the overlay or dismisses
that overlay; otherwise cancels dragging, then clears selection. It never quits.
Suspend remains a help/menu action; Ctrl+Z belongs to Undo. Repeats may insert,
delete, move and extend selection; do not repeatedly launch dialogs/save/quit.
Ignore releases and preserve modifiers. No essential action depends on Cmd,
Ctrl+Shift+letter, enhanced protocols or a particular terminal's paste shortcut.

Dialogs expose labels, focus, buttons and errors using keyboard and mouse. Enter
submits a path field; Escape cancels. Do not show clickable controls that are
disabled or outside the current layout. Keep Stage 01's minimum 40×8 geometry:
overlays may scroll within it; below it, show the resize prompt, preserve all state,
cancel pointer capture, and keep quit/cancel reachable. Dirty quit stays pending
until its full confirmation can be shown; never treat a tiny screen as approval.
Restoring size/focus/resume redraws before accepting text hit-tests.

Direct fields accept terminal paste as field data, with a 4096-byte field bound;
this is separate from unlimited-total-size document paste. Read its disk spool
off the UI thread, preserve following typing/backspace/Enter order, and reject
invalid UTF-8/NUL or oversized field input visibly without changing the document.
Cancel revokes late field results; deferred Enter waits for supported geometry.

## Save and unsaved-change protection

Save (Ctrl+S or a delivered Cmd+S) saves a named document through the existing
guarded-save service; an unnamed document opens Save As. F4 or a delivered
Cmd+Shift+S always opens a direct destination-path dialog.
**Initial Stage 03 Save As policy:** create a new destination only. An existing
path reports “Destination exists; choose a new path”; do not invent overwrite
semantics unsupported by `SaveTarget::New`. Same-path Save retains Stage 02's
permission, link, metadata and external-change checks. No force-overwrite button.

Save captures a specific revision and reports progress. Continue navigation,
selection and editing while it runs. Track the saved revision/content state: a
successful save of R does not clear edits made after R. Change the displayed path
after successful Save As acknowledgement, retaining later edits as dirty.
Coalesce repeated Save requests to at most one pending latest-state save intent.
Stage 02 serialized save I/O and edits on its mutation worker. Stage 03 extends
that scheduling: perform immutable snapshot serialization/publication on one
bounded save worker, then reconcile its exact
saved state and destination baseline on the document owner. Do not hold a document
lock throughout save I/O or acknowledge a stale save as the latest content. Keep
only one save in flight; preserve every Stage 02 conflict, metadata, cancellation
and commit-outcome guarantee. A deterministic blocked-save test must demonstrate
that edits commit before the save is released and remain dirty afterward.

Before New, Open or normal Quit would drop modified content, show Save, Discard,
Cancel. Cancel is the default focus; Escape keeps work. Save continues the pending
action only when the current content is confirmed saved. If later edits exist,
ask again; never close on an older save's completion. Empty/unmodified unnamed
buffers require no prompt. During a save, disabling editing in a confirmation
overlay is allowed; input outside that overlay must not leak into the document.

Pre-commit failure/cancellation retains content, history, path and dirty state.
Show `SaveConflict` and `SaveAsRequired` with Save As/Cancel choices, preserving the
accepted metadata/link policies. Post-commit durability/verification uncertainty
must explicitly say the destination may have changed, retain the unsaved work and
block automatic close/quit. Escape cannot reverse publication. Choosing Discard
while a save is active first cancels/waits for its reported outcome; acknowledge
any uncertain publication before proceeding. Replacement/quit must not erase a
save result that still needs reporting.

External destination changes are checked when saving. **Initial scope:** no
filesystem watcher or automatic reload in Stage 03. Reopen through Open explicitly
and apply the same unsaved-change prompt; no destructive automatic reload.

Stage 01's externally requested termination (SIGINT/SIGTERM/SIGHUP), panic and
terminal-loss cleanup remain best-effort exits, not interactive quit. This stage
does not promise recovery from them. Stop new work, request cancellation and
restore the terminal before waiting for storage shutdown; no worker may write to
terminal output. An OS call may delay final worker exit; do not wait for it while
keeping terminal modes acquired. Recovery remains Stage 05.

## Integration and bounded work

Retain the existing separation: `input` normalizes text/paste and commands; `app`
owns interaction state and intentions; `runtime` executes effects and polls
storage; `ui` consumes prepared layout. Introduce cohesive text-layout and clipboard
modules as needed, without turning the storage core into terminal-aware code.

Use `StorageService` for open/new, reads, line lookup, edits, undo/redo and save.
Its foreground worker serves immutable reads while the mutation worker handles
long operations. Direct snapshot I/O, transaction spooling, clipboard I/O and
potentially long segmentation/layout scans belong off the UI thread. If additional
service requests are needed (for example `line_of_offset`, caret boundaries or
dirty/save-point metadata), specify their typed revision-tagged result and retain
the existing admission/cancellation contracts. They do not exist merely because
this draft names them.

- Every completion carries document/job identity, captured revision and request
  generation. Drop obsolete view results; reconcile committed edits/save results
  before retiring their document. Release stale leases promptly. Compaction must
  release/reacquire UI snapshots, never discard history to escape Busy.
- Preserve input order across edits and navigation: do not resolve a later caret
  command against text that predates an earlier pending edit. Serialize mutations
  against acknowledged revisions; commit caret/history metadata only on success.
  Bound pending input to a proposed 256 events/1 MiB of in-memory queued data plus
  one disk-spooled paste. This queue budget does not cap a paste's total size.
  Coalesce
  superseded view requests and drag motion, never text or ordered edit commands.
  On admission exhaustion, display a busy/rejected-input message; do not silently
  lose input, replay a failed edit, or build an unbounded retry queue.
- Initial UI text/layout cache: 8 MiB aggregate beyond Stage 02 caches, with at
  most one active and one replacement layout job. Use scalar-aligned windows up
  to 64 KiB and sparse, evictable revision-tagged layout checkpoints. Never retain
  one object per document line/grapheme or allocate an entire long line/selection.
  The currently displayed view, replacement/in-flight layout data, text windows
  and checkpoints share this UI budget. Stage 02 caches and Ratatui's terminal
  screen buffers are separate; 8 MiB is not a total process-memory limit.
- Grapheme boundaries may require context outside a window; fetch it incrementally
  with cancellation. Pathological clusters/lines must not require a whole-cluster
  allocation. If a cluster exceeds the bounded render allowance, show a labeled
  placeholder mapped to its complete range; keep movement/copy/edit semantics.
  Expensive horizontal seeks may report pending while scanning from a checkpoint.
  Do not claim arbitrary long-line cell lookup is constant time.
- Proposed work slices: at most 64 KiB scanned before a cancellation check; UI
  completion/layout incorporation yields within 2 ms. Keep Stage 01 frame scheduling
  and signal/input checks. While bytes are pending, retain or clearly mark the
  last valid frame; do not present stale text as the current editable revision.
- The current event API delivers a complete bracketed paste as an owned string;
  staging that string afterward cannot bound the parser's peak allocation.
  Select or adapt the terminal ingress path to stream paste chunks into private
  disk staging, preserving keyboard/mouse/protocol parsing and input order. Bound
  buffers and queued chunks, not total paste size; keep cancellation responsive
  and leave the document unchanged on staging failure or cancellation. Validate
  the full parser-to-storage path before claiming an end-to-end memory bound.
  This is Stage 03 work, not an approved deferral to a later MVP stage.

Use `unicode-segmentation` 1.13.3's
[chunk-context grapheme cursor](https://docs.rs/unicode-segmentation/latest/unicode_segmentation/struct.GraphemeCursor.html)
and `unicode-width` 0.2.3's
[string/emoji cell-width rules](https://docs.rs/unicode-width/latest/unicode_width/),
matching Ratatui's resolved versions. Keep version agreement, licenses and actual
terminal behavior under review when upgrading. Width rules belong to the shared
layout adapter, not separate keyboard/mouse estimates.

## Acceptance criteria

| ID | Observable result / evidence required |
| --- | --- |
| E01 | Launch with no path, one path and `--`/dash-prefixed or non-UTF-8 paths; reject excess arguments. Preview before Ready, cancellation and late binary/encoding/source-change failures preserve a usable UI and any old document. |
| E02 | Movement, insertion/deletion and selection agree for ASCII, tabs, combining marks, CJK, emoji/ZWJ, isolated combining clusters, LF/CRLF/mixed endings, empty text, EOF and no final newline. Test chunk boundaries and round-trip bytes. |
| E03 | Keyboard and cell hit-testing choose the same boundaries, including ties, escaped controls, clipped wide glyphs and blank cells. No document/path/error control sequence can affect terminal state. |
| E04 | Forward/reverse multiline drag, Shift-click, wheel and timer autoscroll preserve selection; release/focus loss/resize/suspend/modal changes stop capture. Stale frame/completion events cannot move the caret or revive retired content. |
| E05 | Copy and Cut preserve the original selection bytes. Terminal, desktop, and retained clipboard pastes normalize CR, LF, and CRLF to the destination style. External and internal pastes larger than 1 MiB succeed with sufficient storage, use bounded ingress/staging buffers, and each commit as one undo unit. Measure parser-to-storage peak memory as paste size grows. Delivered Cmd+C/X/V and Ctrl+C/X/V invoke the same internal commands. Terminal-handled Cmd+V inserts its text once. No-selection copy is harmless; paste controls do not invoke shortcuts. Failed/cancelled large cut or paste leaves document/clipboard/history unchanged. Verify both capture modes and remote clipboard fallback. |
| E06 | Undo/redo restores text and caret/selection, keeps redo after failure, skips no-ops, follows pruning and returns clean at the save point. Ordinary typing, multiline selection replacement and large disk-spooled paste each have the specified undo units. |
| E07 | Keyboard/mouse Save/Save As and New/Open/Quit prompts protect work. Test path errors, existing Save As destination, permission/metadata/link refusals, external conflicts, cancellation before/after publication, uncertainty and save-R finishing after edit-R+1. |
| E08 | All controls/dialogs are reachable by keyboard and mouse at 40×8; Tab inserts text only in editor context. Test every listed delivered Cmd alias against its portable command, including Shift distinctions, focus/modal rules and quit protection. Verify translated terminal mappings, unavailable/unanswered enhancement fallback, intercepted shortcuts and protocol restoration on error/exit/suspend/resume. Tiny/zero sizes retain work and confirmation state. Existing help, Ctrl+S flow-control, signals, suspend/resume and restoration tests remain green. |
| E09 | With storage deliberately delayed or queues full, input/help/cancel/resize stay responsive, buffers stay within stated limits, and overload is visible. Closed/obsolete jobs release leases; compaction and shutdown cannot deadlock behind UI snapshots. |
| E10 | Measure actual first displayed text and event-to-visible response on ordinary, dense-short-line and very-long-line allocated text, including the 1 GiB/8 GiB baseline. Record process RSS, UI/cache/queue bounds, disk usage, hardware and cache conditions; exercise beginning/middle/end edits and scrolling during save/open. |

Use Rust unit/integration tests for boundary/layout and reducer behavior, a
deterministic delayed-worker seam for ordering/cancellation, Ratatui screen tests
for rendered cells, and real PTY tests for input/save/quit/restoration workflows.
Use temporary isolated files; destructive fixtures must never touch user files.
Native macOS/Linux CI runs locked tests, formatting and Clippy with warnings denied.

Cmd validation requires synthetic modifier/normalization checks and real terminal
evidence. Demonstrate direct Cmd/Super delivery on at least one named/versioned
macOS terminal supporting enhanced input, a documented Cmd-to-Ctrl mapping, and
portable operation without enhancements. Record which shortcuts the terminal
intercepts, its relevant configuration, and local versus tmux/remote results;
include a macOS-client/Linux-host path. These checks remain pending until run or
explicitly deferred; do not infer terminal support from synthetic events alone.

For E10, report first provisional display separately from editable Ready, and
event processing separately from frame submission/terminal transport. Target
first usable text within 1 second locally and ordinary visible response within
50 ms; report percentiles/maxima and sample count rather than a single best run.
Include long-line seeks and delayed I/O outside the ordinary-response sample.
Repeat the constrained-memory workload used by Stage 02 with the UI integrated.
Use manual evidence for terminal/font width, native selection and local/tmux/remote
input paths; PTY simulation does not establish those. Carry forward existing
agreed Stage 01 deferrals explicitly; new missing checks need an agreed deferral
before claiming completion. Final larger-than-RAM/end-to-end MVP coverage remains
Stage 10, not implied by this stage's cache-bound tests.

## Implementation decisions and remaining review

Cmd support and portable fallbacks were agreed on 2026-11-03 as described above;
the user subsequently authorized implementation on the same date. The binding
table describes the initial implementation defaults.

The accepted outcome and Stage 02 integrity contracts remain requirements. The
initial implementation uses these defaults:

1. Unwrapped lines, four-cell literal tabs, grapheme navigation, narrow ambiguous
   widths and safe control-token rendering.
2. The context-sensitive binding table/F6 focus route, simple drag gestures and
   fixed initial wheel/autoscroll rates; optional multi-click gestures deferred.
3. Private internal clipboard plus terminal paste/native-selection fallback.
   Native clipboard APIs/helpers/OSC 52 are not selected by this implementation.
4. One event/command per undo unit, temporary unnamed buffers until Stage 05,
   create-only Save As and explicit reopen rather than automatic reload.
5. A bounded save worker so edits can commit during save I/O, with reconciliation
   on the document owner and all Stage 02 save guarantees preserved.
6. Bounded UI and in-memory queues, explicit overload feedback, and streamed
   terminal ingress. These budgets do not impose a total paste-size cutoff.

Dependency selection uses the already resolved Ratatui Unicode dependencies:
`unicode-segmentation` 1.13.3 and `unicode-width` 0.2.3 (MIT/Apache-2.0); their
chunk cursor and width APIs were checked against current primary documentation.
Native clipboard integration is not selected. The remaining native terminal and
transport matrix remains explicit validation work.

## Validation record

### Terminal paste line endings (E05)

Before the correction, a macOS PTY test and an injected desktop clipboard
test reproduced incoming line endings being preserved during paste. The
revised checks cover all document paste routes, UTF-8 and split endings,
the caret after conversion, and the effect of changing the saved default on
an open file with and without an existing line ending. On macOS arm64,
371 workspace tests passed, along with formatting, Clippy, and independent
review.

The Tab Details PTY checks cover the saved default on an open file with no
line endings, fallback after deleting the last line ending, and the first
line ending of a mixed file changing during a parked tab's external refresh.

In the Ubuntu 26.10 arm64 UTM guest, the original installed build displayed
`<U+000D>` when the terminal pasted two lines copied from Text Editor. An
earlier build restored those line breaks and saved the expected LF and CRLF
bytes. Windows Terminal under WSL remains open for direct testing.

**Local implementation reviewed — 2026-11-03 and 2026-11-04.** The requested thorough
code review includes fixes and regression coverage below. Available local checks
are recorded here; full Stage 03 acceptance remains pending unavailable terminal,
Linux/CI and remote-environment evidence. This does not assert those environments
passed. Earlier evidence
remains in the [Stage 01](01_terminal_foundation.md#validation-record) and
[Stage 02](02_document_storage.md#validation-record) records.

### Implementation and local checks

The shell now uses `StorageService` through an ordered background coordinator.
A dedicated VT reader streams bracketed paste to private disk files in 64 KiB
chunks, avoiding Crossterm's complete-string paste allocation. Initial portable
and delivered Cmd defaults, dialogs/guards, disk clipboard, history positions,
mouse selection/autoscroll, Unicode cell mapping, and independent saving are
implemented. Go To (`Ctrl+G`, `LINE[:CELL]`) provides practical large-file seeks.

Layout data is charged to 3 MiB per prepared result within the aggregate 8 MiB UI
allowance. Two process-wide RAII layout slots include displayed and in-flight
results; obsolete unconsumed replacements are released before another build.
Printable ASCII and suitable Unicode graphemes use compact stride runs with exact
per-cell selection and hits; non-additive cross-grapheme ligatures stay separate.
Actual descriptor capacities are admitted before growth, and completed rows are
packed to exact span counts. String growth includes its minimum allocation;
windows/context remain bounded and checkpoints are capped at 256.
Valid checkpoints strictly before an edit transfer to its new revision; later
mappings are invalidated. Input admission includes active envelopes (256 events /
1 MiB), at most one pending paste, and adjacent disposable view requests coalesce.
Pointer placements carry document/revision/geometry tags and use the displayed
cell map before entering the ordered queue. Save workers use independent read
handles, reconcile on the owner, and preserve post-publication uncertainty.
Unexpected worker loss fails visibly rather than leaving an eternal busy state.
Queue rejection keeps a dismissible error even when subsequent completions update
the status row. Guard/path replies carry intention identities; Cancel revokes
late replies and automatic follow-up actions. Close/replacement checks and a
waiting confirmation do not accept editor mutations.

Local macOS checks on 2026-11-03:

- `cargo fmt --all -- --check`: pass.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: pass.
- `cargo test --workspace --locked`: pass (103 regular tests; large-file, paste
  memory and isolated-tmux checks are opt-in and recorded separately).
- `RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --locked`: pass.
- New CI shell block: ShellCheck pass, using `shellcheck-py==0.11.0.1` in a
  temporary isolated uv environment; no system Python packages installed.
- Plain/macOS/Linux resource-wrapper test launchers: ShellCheck pass. The
  documented SC2016 exception defers quoted PID/argument expansion to the nested
  child shell; the synchronization read intentionally discards its value.
- Markdown links/anchors and whitespace: pass (13 local links/anchors in the
  README, overview and Stage 03 document; source whitespace also checked).

| Criteria | Local evidence and scope |
| --- | --- |
| E01 | CLI flags/path/`--`/non-UTF-8 argument handling and pipe non-consumption; startup open, failed replacement retention, and read-only preview path. Existing storage progress/cancellation/late-invalid-source tests pass. |
| E02–E03 | Chunked graphemes checked against the contiguous Unicode oracle, including combining/emoji/flags/CRLF and a cluster exceeding 64 KiB. Tab/wide/control-token cell hits, long-line viewport continuation, preferred columns, and revision-safe positions pass. Frontend/font agreement remains manual. |
| E04 | Executable rapid multiline Unicode drag→copy→paste→save round trip; stale hit rejection; timer-driven autoscroll and focus/resize/overlay capture cancellation. Wheel/keyboard viewport state is shared with prepared layouts. |
| E05–E06 | Large internal/external paste exceeds 1 MiB, is one undo unit, preserves bytes, and retains document/clipboard/history on invalid text or failed cut. Queue overload is explicit; queued paste cancellation leaves no edit. Real PTY multi-megabyte save/undo/redo passes; additional paste memory results below. |
| E07 | File round trip, existing Save As refusal, default-Cancel dirty quit, save-point undo, Stage 02 fault/conflict suite, blocked-save concurrent edit, and post-publication save-worker panic handling. Save of an older state leaves newer edits dirty. |
| E08 | Synthetic exact Cmd/Super/Shift bindings, CSI-u delivery, late/unanswered capability fallback, push/pop and re-entry restoration, paused-capture preference, modal/tiny geometry, signals and suspend/resume. Real Cmd interception/mappings and transport coverage remain pending. |
| E09 | Bounded queue/clipboard admission, parser buffers, sparse checkpoints and cancellation; delayed-save and lost-worker checks. Separate incorporation/resource measurements are recorded below. |
| E10 | Real allocated 1 GiB / 8 GiB interactive PTY benchmarks below. Constrained-memory Linux UI CI is configured but has not executed for this branch. |

### Code review and regression fixes

The pass traced input → modal/command routing → ordered coordinator → storage
commit/save → layout → displayed cells, including cancellation and shutdown.
Fixes cover:

- A four-byte Unicode scalar immediately before LF could panic in line-tail
  slicing. Aligned tail reads now include the complete scalar and terminator.
- A failed save retry could clear a prior uncertain-publication warning. Only
  acknowledged saving resolves it; Discard still requests acknowledgement.
- Committed edits now retain pending caret/history reconciliation through read
  failure/cancellation, preventing repeated edits or missing undo-view entries.
  Undo/redo restore acknowledged position metadata before fallible refresh work.
- Queued Open and startup cancellation persist before worker execution. Separate
  view/navigation generations cancel future/superseded reads without dropping
  navigation on resize; explicit cancellation survives read-token creation.
- Non-vertical navigation no longer scans the old line solely to calculate its
  column. Horizontal overscroll clamps when visible lines have all ended.
- Drag release uses its final delivered coordinates. Paused capture ignores
  residual mouse events, and the menu shows its persistent enabled/paused state.
- Direct-field paste runs off the UI thread, preserves immediately following
  edits/submission, and cannot apply to a reopened or dismissed dialog.
- NUL/Ctrl+Space and unsupported functional/mouse keys cannot insert stray text
  or activate controls. Truncated UTF-8 cannot swallow the next key; repeated
  Escape remains cancellable. Bracketed-paste controls remain data.
- Input payloads move out of charged envelopes without an uncharged duplicate.
  Layout accounting includes actual vector capacity and small-allocation growth;
  packed rows and Unicode runs keep large views within the admitted budget.
- Save polling/admission errors are surfaced rather than ignored; owner loss
  cannot leave a save waiting indefinitely. Quit remains reachable even when
  opening and creation of the fallback empty buffer both fail.

Regression evidence includes a 300-operation contiguous Unicode editing/history
model, four-byte-scalar/LF cases, large Unicode and mixed-stride views, retained
allocation accounting, pre-execution cancellation, save uncertainty after retry,
post-commit read failure, final mouse-release placement and pasted-path→typing→
Enter through the real executable. This pass does not claim a proof of no bugs.

### Independent follow-up pass — 2026-11-04

A fresh pass checked the specification against current source and traced failure,
ordering and ownership paths without treating the earlier validation as proof of
correctness. It found and fixed six further issues:

1. **Stale cell checkpoints after cancelled layouts.** Every scan now validates
   the snapshot revision; an edit can transfer checkpoints only from its actual
   base revision. A regression removes a leading tab, skips a revision's layout,
   then checks navigation and a subsequent edit beyond the 64 KiB boundary.
2. **Deferred field submission left old controls active.** Timer-driven path
   submission now reconciles geometry/focus before drawing or accepting another
   key. The regression verifies immediate following text returns to the editor.
3. **Completed terminal paste files could accumulate before editor admission.**
   Ingress permits one outstanding completed terminal paste and visibly rejects
   another while draining its complete payload as data. The reader retains file
   ownership until consumers release it, keeping large-file cleanup off the UI
   thread. Rejection uses a durable overlay so a worker update cannot hide it.
   This enforces pending-work admission; it adds no total paste-size cutoff.
4. **Scroll clamping depended on whether a paint job ran.** Logical offsets now
   clamp independently of superseded layouts, including reversal at EOF/line end
   and scrolling after a document shrinks. Opposite wheel directions are not
   combined; vertical coalescing also preserves horizontal-clamp ordering.
5. **Mouse release could activate a different focused confirmation button.**
   Release now activates the captured, clicked control even if keyboard focus
   changed while it was held. New save-error/uncertainty overlays cancel capture.
   The regression exercises Save press → Tab to Discard → release over Save.
6. **An owner panic could detach an independent save.** A save-thread owner now
   cancels/joins during unwinding, before releasing its snapshot/admission. A
   deterministic blocked-publication test injects a mutation-owner panic and
   proves shutdown waits for the save and preserves the destination bytes.

Follow-up checks: `cargo test --workspace --locked --quiet` passes **110 regular
tests** (57 library, 4 CLI, 37 storage, 12 executable PTY); four explicit opt-in
checks remain outside that count. Formatting, Clippy with warnings denied and
Rustdoc with warnings denied pass. The isolated tmux 3.8 editing/mouse/capture/
suspend test passes again. The 256 MiB resource probe also passes for all four
corpora: the final run's maximum incorporation is 0.170 ms and aggregate layout
charge peaks at 2771896 bytes; its mixed-Unicode frame CPU maximum is 4.723 ms. A new 1 GiB
ordinary-file executable run is recorded in the table below.

Unavailable native iTerm2, Linux/CI and remote transport checks retain their
previous status. No pass or new environment deferral is inferred from this review.

### Review fixes — 2026-11-12

- Discard now acknowledges a specific uncertain save job. A new uncertain
  publication requires a fresh acknowledgement. A pre-publication failure
  stays visible if it arrives just before or during a pending Discard.
  Controlled save-worker tests verify both orderings and destination bytes.
- Equal-byte replacements advance the caret and clear the selection without
  adding an undo entry or clearing redo. Coordinator and executable PTY tests
  cover the resulting text, caret and history.
- Text hits validate the viewport offsets, document revision and geometry.
  Drag motion requires an accepted press for the same gesture. Tests cover
  wheel followed by an immediate click, rejection of a stale drag anchor,
  rapid drag/copy/paste and a held press in the first visible text column.
- An executable PTY test verifies backward focus via CSI-u Shift+Tab. Another
  cancels an active bracketed paste via the displayed `SIGUSR1` route, then
  saves literal enhanced Escape and Ctrl+Q bytes from a subsequent paste.
- Runtime shutdown signals ingress and storage before restoring terminal modes
  and joining the workers. A controlled save blocked before publication sees
  the stop flag and completes with cancellation, preserving destination bytes.

The macOS arm64 checks passed with Rust 1.99.1. The locked workspace suite passed
120 regular tests, including 16 executable PTY tests. The separate isolated tmux
3.8 smoke test, formatting, Clippy and Rustdoc with warnings denied, and diff
whitespace checks also passed. An independent reviewer verified the changed
paths and reran the editor and PTY suites. In an isolated copy, deliberate breaks
in the save acknowledgement, failure visibility, drag-anchor and shutdown guards
failed the corresponding behavioral assertions. Native terminal and transport
checks below remain pending.

### Native interactive performance

Machine: Apple M1 Max, 32 GiB RAM, macOS 27.2 (26B123), arm64, pinned Rust 1.99.1.
Files were generated by actual writes in isolated local temporary directories,
then opened in a real Unix PTY (`TERM=xterm-256color`, `/bin/sh -i`). Conditions
are warm generated-file runs, with default clone-attempt opening; per-run clone
success was not separately instrumented. These are PTY-observed frame times,
not graphical-terminal, cold-cache, remote, or larger-than-RAM measurements.

Reproduce with `EDE_BENCH_MIB=1024` or `8192`, `EDE_BENCH_CORPUS=ordinary|dense|long`,
and `cargo test --release --locked --test terminal
large_file_interactive_benchmark -- --ignored --exact --nocapture`.
Each run samples 40 ordinary input-to-visible edits, then navigates/edits the
beginning, middle and end, saves while accepting a later edit, verifies the
published bytes/length, and requires dirty-quit protection for that later edit.
RSS is the maximum sampled editor-process RSS, not an instrumented peak.

| Corpus | GiB | First text ms | Editable Ready ms | p50 / p95 / max ordinary ms | Sampled RSS KiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| Ordinary, final compact layout | 1 | 161.676 | 552.226 | 15.131 / 30.825 / 32.513 | 147536 |
| Dense short lines | 1 | 110.918 | 6693.361 | 17.464 / 25.743 / 33.231 | 144416 |
| One long line | 1 | 92.560 | 455.212 | 15.422 / 21.155 / 23.536 | 150592 |
| Ordinary | 8 | 189.987 | 5766.644 | 18.299 / 42.237 / 45.510 | 164768 |
| Dense short lines | 8 | 137.655 | 45409.484 | 18.166 / 39.258 / 39.654 | 159856 |
| One long line, final compact layout | 8 | 164.685 | 2601.157 | 15.101 / 33.005 / 34.308 | 127760 |
| Ordinary, post-review retest | 8 | 168.745 | 4851.794 | 15.168 / 33.257 / 34.679 | 134160 |
| Dense short lines, post-review retest | 8 | 93.331 | 44203.541 | 15.100 / 18.158 / 27.237 | 146048 |
| One long line, post-review retest | 8 | 97.846 | 3109.322 | 15.116 / 18.132 / 18.143 | 160592 |
| Ordinary, kernel-peak wrapper | 1 | 115.073 | 1009.022 | 15.117 / 31.780 / 32.452 | 145360 |
| Ordinary, 2026-11-04 follow-up | 1 | 156.022 | 702.432 | 17.322 / 27.200 / 34.534 | 149136 |

The first 8 GiB long-line run exceeded the ordinary-input target (p95 58.158 ms,
max 68.437 ms). Pending interaction-result polling was reduced to 2 ms while idle
polling remains at most 20 ms. An isolated retest met the target (max 37.131 ms).
The final compact-layout run in the table also meets it; the initial failure is
retained here. The remaining earlier rows precede the final span compaction. Dense 8 GiB contains 4,294,967,297 logical lines;
Ready preparation is deliberately distinct from its early read-only display.
Managed usage, disk accounting and separate reducer/incorporation timings have
an additional coordinator/UI probe below; an instrumented process RSS peak
is reported separately where the optional kernel resource wrapper was used.
The final 1 GiB ordinary run with `EDE_BENCH_PEAK_RSS=1` also reports a kernel
lifetime peak of **145392 KiB**, using macOS `/usr/bin/time -l` around the launcher
that execs ede. This measures that editor PID's lifetime, distinct from sampling.

### Managed resources and separate UI CPU timings

`EDE_RESOURCE_MIB=256 cargo test --release --locked --lib
interactive_resource_and_incorporation_measurements -- --ignored --nocapture`
passes with real disk-backed storage/coordinator operations and Ratatui's test
backend. Each generated 256 MiB corpus receives 40 edits. Ordinary/dense/long
use 80×24; alternating ASCII/Unicode uses 240×82 to exercise descriptor capacity
and visible-view replacement. The UI retains the current view while its replacement
is built. Reducer dispatch, prepared-screen incorporation and frame CPU submission
are measured separately from storage work and terminal transport.

| Corpus | Reducer dispatch max ms | Incorporation p95 / max ms | Frame CPU p95 / max ms | Aggregate layout charge peak bytes | Working allocated block bytes |
| --- | ---: | ---: | ---: | ---: | ---: |
| Ordinary | <0.0005 | 0.002 / 0.002 | 0.173 / 0.209 | 7518 | 269152256 |
| Dense short lines | <0.0005 | 0.002 / 0.002 | 0.075 / 0.079 | 7518 | 285974528 |
| One long line | <0.0005 | 0.001 / 0.001 | 0.070 / 0.072 | 7518 | 269152256 |
| Mixed Unicode | <0.0005 | 0.208 / 0.211 | 5.397 / 5.594 | 2771896 | 269152256 |

All four runs report 764672 cache bytes and the same cache-pool high-water sum,
829441 admitted storage-buffer peak bytes, 268435496 working text bytes and
707144 logical metadata bytes after edits. Cache peaks sum individual pool peaks
(a conservative aggregate upper bound). The layout counter includes retained and
in-flight capacity, and a separate regression independently counts actual retained
row/span/string capacities, including initial small allocations and mixed strides.
Two 3 MiB layout reservations leave the rest of the 8 MiB UI allowance for bounded
windows/context, checkpoints, transient growth and small UI state. Input admission
separately exercises all 256 slots and exactly 1 MiB of charged payload, including
the active envelope; it rejects the next event rather than duplicating that payload.

These are managed charges, not allocator/process RSS. Working block totals use
`st_blocks × 512` across private working files, include bitmap/history/index files,
and exclude the input fixture and destination. APFS clone sharing makes them
filesystem-reported allocated blocks, not unique physical disk ownership. Warm
fixtures/hardware match the native performance runs. The measured incorporation
maximum meets the proposed 2 ms slice; frame CPU is a separate measurement.

### Paste ingress memory

`EDE_PASTE_MIB=2|64|128 cargo test --release --locked --test terminal
large_paste_memory_benchmark -- --ignored --exact --nocapture` sends fixed-size
chunks through the actual terminal reader, commits the paste, saves it and checks
the resulting length. No payload-sized string is constructed in the sender.

| Paste MiB | Baseline RSS KiB | Maximum sampled RSS during ingress KiB | RSS after document commit KiB | Receive/render/save ms |
| ---: | ---: | ---: | ---: | ---: |
| 2 | 2960 | 3088 | 5392 | 4617.770 |
| 64 | 2944 | 3056 | 69536 | 126226.264 |
| 128 | 2960 | 3104 | 94048 | 262471.936 |

Post-review kernel-peak runs use `EDE_BENCH_PEAK_RSS=1` with the same ignored test:

| Paste MiB | Baseline RSS KiB | Sampled ingress maximum KiB | Document-ready RSS KiB | Kernel lifetime peak RSS KiB | Receive/render/save ms |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 2 | 3040 | 3152 | 5584 | 5952 | 3736.285 |
| 64 | 3040 | 3168 | 69808 | 113232 | 130510.626 |

The kernel peak includes commit, document caches and saving, while the ingress
column samples only reception. It does not represent parser-only allocation.
`/usr/bin/time -l` supplies the macOS lifetime peak in bytes (converted to KiB);
the optional Linux wrapper uses `-v` and is unverified in the unavailable Linux
environment. Normal application execution has no dependency on this tool.

Ingress remains bounded while document-cache usage grows within separate storage
budgets. These measurements include scripted macOS PTY transfer and save; they do
not isolate parser throughput or establish paste speed for a graphical terminal.
An earlier real-PTY Unicode/CRLF paste also round-trips 2,359,307 bytes through
save and one-step undo/redo. Internal disk-spooled paste has automated coverage
above 1 MiB. No total paste-size cutoff is selected.

### Local tmux smoke

`cargo test --locked --test terminal isolated_tmux_editing_and_suspend_smoke --
--ignored --exact --nocapture` passes through tmux 3.8, with a private temporary
socket/server and `-f /dev/null`. It verifies ordinary editing, internal
copy/paste, external bracketed paste containing Ctrl+Q as data, exact saved bytes,
SGR mouse drag/release→copy→paste, paused mouse capture with internal paste/undo,
Help → Suspend with capture paused, the shell's `fg`, normal Quit, and matching shell terminal
attributes after its line editor settles (Darwin's transient PENDIN is masked).
The private server is removed on success or failure; no user configuration changes.
This establishes the tested local portable path, not Cmd delivery or SSH support.

### Outstanding environments and checks

- Native Linux and the new 256 MiB / swap-disabled ordinary/dense/long UI CI runs.
  Local Docker is installed but its daemon is unavailable; no Linux result is
  inferred from macOS checks or workflow configuration.
- Native-client tmux mouse/capture and advanced-keyboard combinations beyond the
  scripted portable/SGR/capture smoke above.
- A named/versioned macOS terminal: actual Cmd/Super delivery, every advertised
  alias/interception outcome, optional Cmd→Ctrl mapping instructions, native
  selection and font-width behavior. The user identified iTerm2, sometimes tmux;
  installed iTerm2 is 3.7.6. Native testing is unavailable because the computer-use
  tool refuses access to that app ("Computer Use is not allowed to use the app
  'com.googlecode.iterm2' for safety reasons"). No alternate GUI-control route was used.
- macOS-client/Linux-host SSH, with/without tmux; remaining transport matrix.

The user explicitly reaffirmed the full agreed scope on 2026-11-03. Required
coverage is broader than the user's personal iTerm2 setup:

| Required path | Stage 03 evidence/status |
| --- | --- |
| macOS direct terminal, legacy and enhanced input | Real executable PTY and synthetic protocol checks pass; native iTerm2/other frontend behavior remains pending. |
| macOS terminal → tmux | Isolated tmux 3.8 portable editing/paste/save, SGR mouse/capture and paused-capture suspend/resume smoke passes; native client Cmd/font/selection combinations remain pending. |
| Native Linux terminal, direct and tmux | Native/constrained-memory CI and graphical frontend checks pending. |
| macOS terminal → SSH → Linux, direct and tmux | Pending a test host; no remote pass inferred from local protocol tests. |
| Agreed mosh / Eternal Terminal paths, with and without tmux | Earlier unavailability remains recorded; Stage 03 behavior is unverified. |

For this code-completion review, the user explicitly excluded unavailable iTerm2
and Linux environments. Those exclusions do not establish environment passes.
The user's Stage 01 approval to defer unavailable manual frontend/SSH/mosh/Eternal
Terminal checks remains recorded there. It does not automatically approve new
Stage 03 gaps. No new deferral is claimed here; Stage 03 remains in progress.

### Linux and SSH follow-up during Stage 08

On 2026-11-15, Ubuntu 26.10 arm64 passed the full locked suite, including
real PTY editing, clipboard, paste, mouse, resize and job-control checks. The
isolated tmux 3.7b smoke passed portable editing, mouse selection, bracketed
paste, suspend/resume and termios restoration. All three 1 GiB interactive
file corpora passed under the 256 MiB, swap-disabled cgroup. The 64 MiB
paste probe passed with the Linux `/usr/bin/time -v` wrapper. The macOS
tmux client also reached the Ubuntu editor over SSH with and without a nested
Ubuntu tmux server. See the
[Stage 08 validation record](08_pipes_and_follow.md#validation-record) for the
manual SSH checks. Native graphical terminal behavior and the mosh and
Eternal Terminal paths still need their own environments.
