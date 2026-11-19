# 12 — Pane refinements

**Approved for implementation.** Branch `stage/pane_refinements`, PR target
`feat/initial_mvp`. Baseline is merged Stage 11 at `dc02088`.

## Dismissal

An outside left click dismisses Actions, Settings, Tabs, Help, About, Details,
Language, Syntax Theme, Open and Follow file pickers, Recovery, unguarded path
dialogs, and docked Find/Replace. The click must start and finish outside the
visible bounds of the same pane without dragging or changing geometry. The pane
consumes the gesture and returns focus to its opener or parent. Find/Replace
invalidates pending search work and focuses the editor without moving the caret.
If the editor queue is full, Find/Replace stays open and asks the user to retry.

Dirty Settings shows **Save and Exit**, **Discard and Exit**, and **Keep Editing**,
with **Keep Editing** focused. Preserve any pending inline text. Escape and Keep
Editing return to the draft. Validation or save failure leaves the draft open.
Save and Exit closes only after a confirmed write. Outside clicks on the
confirmation leave it open. Guarded Save As, other confirmations, save and
persistence errors, and Settings writes use their explicit controls.

## Actions and tab inspection

Right-align **Tabs**, **Settings**, **Menu**, and **Quit** in the workspace
header. Use two blank terminal cells between buttons and limit pointer targets
to their labels. Color the buttons cyan, yellow, magenta, and red respectively.
Keep bold text and `>Label<` markers for focused buttons. At 40×8, keep the
active tab and its close control visible. Open Help from **Menu → Help → Help**
or F1.

Actions lists File, Edit, Search, Tabs, Highlighting, Streams, Settings, and
Help at left and the active category's commands at right. Moving through or
clicking categories changes the command list. Tab and Left/Right move between
columns, the wheel scrolls the pointed column, and the selected command's full
label fits below the columns at 40×8. Keep existing command routing and
discoverability. The Help category opens **About**, which displays the running
app version and resolved configuration path, or reports that it is unavailable.

Tabs initially selects the active tab. The active tab has a `>` marker. Only
the selected row has highlight styling. **Details** reads current metadata for
the selected tab's stable ID and returns to the same selection on close. An
inactive tab can be inspected without switching. Details can show a named path,
recovery status, stream progress, language, and highlighting. If the selected ID
disappears, report that it is unavailable. Any Retry refresh action targets the
displayed ID. A parked tab shows its language under the current configuration
and indicates when highlighting analysis is deferred. Actions → Tabs → Tab
details continues to open the active tab.
**Close Selected Tab** invokes the guarded close for the selected row and is
red. **Switch** is green and **Back** dismisses the pane. At 40×8, use two
action rows to keep the full labels visible.

## Settings

Use a sidebar with **Recovery**, **Highlighting Limits**, and **Keyboard
Shortcuts** at 64 columns and wider. Use compact selectors above the content
at smaller supported widths. Each section shows its own rows. The entire pane
shares one draft. Section selection accepts valid inline scalar input into the
draft, and leaves invalid input or pending field paste in place for resolution.

Keyboard Shortcuts expands into scopes in the sidebar. **App commands** covers
workspace actions. **Focused buttons** covers navigation and activation when a
workspace button has focus. **While in Settings** covers controls within that
pane. The other groups are **Editing text**, **Find & Replace**, **File picker**,
and **Dialogs**. The right pane shows task headings where space permits, with
clearly styled **Command** and **Shortcut** columns. Use a group selector above
the columns at narrow sizes. Present human-readable action names, keep existing
configuration IDs in binding editor titles and diagnostics, and retain one
draft across all shortcut groups. At 40×8, Recovery and Limits provide access
to Reload and Restore Defaults.

Place Save, Discard, and Exit at the bottom right. Save is green, Discard red,
and Exit and Keep Editing neutral. Preserve readable labels and focus indicators
in monochrome. Reload and Restore Defaults remain available. Remove Config from
Settings and retain the configuration source and diagnostics. Truncated feedback
can be opened with **Details**. Existing Settings bindings for configuration
details open About and return to the same draft.

## Acceptance criteria

| ID | Required check |
| --- | --- |
| P12-1 | At regular size and 40×8, dismiss panes over editor content, tab ×, and Quit without triggering the underlying action. Test a border, crossing drag, resize or pane change between press and release, disabled capture, picker cancellation, and Find/Replace with pending work. |
| P12-2 | Exercise clean and dirty Settings dismissal, outside clicks on the confirmation, Escape, Keep Editing, Save and Exit, and Discard and Exit. Verify pending text and persisted bytes. Test validation failures and keep existing write and discard safeguards. |
| P12-3 | Navigate Actions and Settings by keyboard and mouse at both widths. Inspect rendered bounds, section isolation, color and monochrome focus, and right-aligned actions. Save a draft spanning scalar and shortcut changes. Verify About's version, resolved path, and legacy Settings binding. |
| P12-4 | Inspect an inactive named tab and an inactive stream without changing the active document. Verify the active marker, selected-row highlight, return focus, and behavior when a target disappears. |
| P12-5 | At 40×8 and 80×24, verify the right-aligned header, two-cell gaps, individual button colors, visible focus, active tab close control, and both Help routes. |

## Validation record

**November 18, 2026.** Tested on arm64 macOS 27.2.1 with Rust 1.99.1. Baseline
PTY tests reproduced the missing outside dismissal in Settings and Find/Replace,
the missing section isolation, and the missing Details action in Tabs.
Formatting and Clippy passed. The workspace suite passed with 361 tests and
10 ignored under the default selection.

| ID | Evidence |
| --- | --- |
| P12-1 | Real PTYs verified dismissing Actions, Help, Tabs, and Find/Replace without activating the covered controls or moving the caret. Reducer checks covered picker cancellation, capture disabled, stale release after resize, and saturated search admission. |
| P12-2 | Real PTYs verified that outside clicks on the dirty confirmation leave it open, Escape and Keep Editing preserve the draft, invalid input stays editable, Save and Exit writes pending text, and Discard and Exit preserves the original file. |
| P12-3 | Rendered cells at 40×8, 64×8, and 80×8 verified section isolation, focus markers, footer alignment, and button colors. PTYs exercised the keyboard and mouse, saved a combined numeric and shortcut draft, and verified About and scrollable diagnostics. Reducer tests verified legacy bindings and diagnostic access during a pending paste. |
| P12-4 | PTYs verified the active marker and the selected row's color, metadata after a configuration reload, live retained bytes in an inactive stream, and return focus and viewport preservation. A reducer check verified missing-tab action guards. |

In isolated copies, bypassing the Settings dirty-exit guard failed the PTY
assertion for the confirmation. Closing Find/Replace after denied cancellation
failed the queue-saturation assertion. A separate implementation review and
follow-up checks found no remaining issues. Linux and graphical terminal
smoke tests were outside this local validation run.

**Shortcut and Tabs follow-up.** The 40×8 PTY test initially failed because the
shortcut list had no column headers. After the change, PTYs verified the compact
selector and wide sidebar, the exact saved `controls.activate` override, binding
IDs alongside diagnostics, and the colors and guarded behavior of the Tabs
footer after resize. A unit check confirmed that compact command labels are
distinct within each scope. Formatting, Clippy, and all 362 default workspace
tests passed on the arm64 macOS host. The workspace also reported 10 ignored
tests. The separate follow-up review found no remaining issues.

**P12-5 — Workspace header and beta version.** The new PTY regression first
failed on the uneven header. At 40×8 and 80×24 it now verifies the exact spacing,
each button color, bold focus markers, inert gaps, the active tab's close
control, each button action, and Help access from Menu and F1. The overlay and
capture tests locate Quit from the rendered header or layout. `ede --version`
reported `ede 0.0.1-beta1`. Formatting and Clippy passed on arm64 macOS 27.2.1
with Rust 1.99.1. The locked workspace suite passed with 363 tests and 10
ignored when run with controlling-terminal access. The sandbox denied access
for eight existing piped-input tests.
