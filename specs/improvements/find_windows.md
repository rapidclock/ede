# Improvement Spec — Find and Find & Replace windows

**Status** — Design draft for review. Implementation and validation are pending.
**Release** — User-confirmed patch target `0.1.2`, from `0.1.1` on `main`.
**Branch** — `enhance/find-windows`, based on `main` at `3004bb3`.

## Current behavior and scope

Find and Replace use a dock at the bottom of the editor. It shows two controls
at a time plus a status row. Users can click visible controls, but generally
need Tab or Shift+Tab to bring other controls into view. Searches report counts,
highlight visible matches, and offer Previous and Next. Search summary reports
outcomes by tab. There is no list of individual occurrences to browse or click.

Replace the dock with two entry points into a shared non-modal search window.
**Find** searches and browses matches. **Find & Replace** adds a replacement
field and replacement actions. Both modes search the current document or all
open tabs, including unsaved text. Keep literal matching, existing shortcuts
and remaps, session field retention, cancellation, and replacement safeguards.
This supersedes the dock and control-navigation design in [Stage 06](../001_initial_mvp/06_search_and_replace.md#commands-and-interaction).

## Proposed windows

The proposed layout places a bordered window in the upper-right of the editor
area, leaving the tab strip and status bar visible. In normal terminals, cap
the width at 60 columns and the height at half the editor area. The match list
uses the remaining space and scrolls independently of the editor. The window
must stay the same size as result counts grow. When a jump would place the match
under the window, reveal the match start by repositioning the window or scrolling
the editor. Show the whole match when it fits in the exposed area.

The sketches show the two modes with results loaded. Options and the selected
result are visible by label and state, including in monochrome terminals.

```text
┌─ Find ─────────────────────────────────────────── [×] ┐
│ Find  [ apple                        ] [Find]         │
│ [▸ Options]  Case on · Whole word off · Current       │
│ [Previous] [Next]                     2 of 18 matches │
│   3:7    green apple                                  │
│ > 12:1   apple tree                                   │
│   15:7   baked apple                                  │
└───────────────────────────────────────────────────────┘

┌─ Find & Replace ───────────────────────────────── [×] ┐
│ Find     [ apple                     ] [Find]         │
│ Replace  [ pear                                     ] │
│ [▾ Options]  [x] Match case  [ ] Whole word           │
│ Scope [All open]  Insert [Tab] [LF] [CRLF]            │
│ [Previous] [Next]                     2 of 27 matches │
│ [Replace next] [Replace all…]                         │
│   notes.txt  3:7    green apple                       │
│ > notes.txt 12:1    apple tree                        │
│   todo.txt   2:5    buy apple                         │
└───────────────────────────────────────────────────────┘
```

- Open with the Find field focused. Start with Options collapsed and show the
  current case, whole-word, and scope values beside the toggle. Expanding it
  exposes labeled, clickable controls. Use **Match case**, **Whole word** and
  **Scope** to describe the existing flags. Keep Insert Tab, Insert LF and
  Insert CRLF reachable in the expanded section for the last focused text field.
  If collapse hides the focused control, return focus to Options.
- At `40×8`, use a compact scrollable form and results view with at least three
  full editor rows exposed. Show a clickable way to switch between form and
  results and to scroll to offscreen form controls. Preserve the equivalent
  keyboard routes. Keep focus and the result scroll position across resize
  where their targets remain valid. Below `40×8`, retain the resize prompt and
  field values and inhibit replacement activation.
- Keep F6 available to return focus to the editor without closing the window,
  and keep the existing binding for returning to search. With mouse capture
  enabled, clicking outside dismisses the window and consumes the click.
  Escape first cancels active work, then closes the idle window. Show a
  clickable Cancel search action while work is active. Closing retains the
  entered fields and flags and returns focus to the editor. F3 and Shift+F3
  can still navigate with the window closed.

## Input and results

- **Form focus** — Up and Down move between visible form rows, including from a
  text field into Options or the action rows. Left and Right move within the
  active text field or between controls on the same row. Tab and Shift+Tab
  remain available. Enter activates the focused control. Every visible control
  has a mouse hit target. The form, list, options, actions, and dismissal are
  reachable without Tab and without the mouse.
- **Search** — Enter in Find or click Find to submit the current literal query.
  Preserve the current initial Next behavior from the caret. Empty queries
  clear results and disable match and replacement actions. Query or option
  changes invalidate the prior list and cancel superseded search work. The UI
  prompts to search again. Editing only the replacement preserves search
  results and invalidates pending replacement intent. Each field keeps the
  existing 4096-byte UTF-8 limit, literal paste, and safe display of controls.
- **List focus** — Down from the last form row enters the list and focuses the
  current result, or the first available result. Left returns to the form. Up
  and Down select and reveal the adjacent occurrence. Page Up and Page Down
  advance by one visible list page and reveal the selected occurrence. Home and
  End reach the first and last available results. List movement stops at the
  ends. Previous and Next, including F3 and Shift+F3, retain existing wrap
  behavior and keep the selection and list viewport in sync with the jump.
  Enter on a result reveals it. A click on a result selects and reveals it in
  one action. The wheel over results scrolls the list without changing the
  current match, caret, active tab, or editor scroll position.
- **Result rows** — Include every canonical occurrence in the requested scope.
  Show one row per match in tab creation order, then by document offset. Each
  row shows the 1-based line and editor cell column of the start, a clipped and
  safely escaped preview with the matched text distinguished where visible,
  and, in All open, an identifiable tab label. Repeated matches on a line and
  matches in same-named tabs must be individually selectable. A valid jump
  activates the target tab if needed, clears its old ordinary selection, and
  reveals the distinct exact-match highlight with the caret at a grapheme
  boundary. Focus stays in the window when a result is chosen there.
- **Progress and scale** — Show the match count and current ordinal when known.
  Identify scanning, cancelled, incomplete or stale results, and complete
  searches with zero matches. Keep Search summary reachable for per-tab
  outcomes. Every match from a successfully scanned target must be reachable,
  including matches beyond the first page. Load result rows in bounded pages or
  through an equivalent bounded mechanism. Keep document I/O and scan work
  cancellable and off the UI event path. A partial or unavailable result set
  must report that state explicitly.
- **Result validity** — Bind rows and jump requests to the submitted query,
  options, scope, tab and document revision. A change to any target's revision
  or availability invalidates its affected rows. A user-directed tab switch
  invalidates a Current-scope list for the previous tab. An All-open jump keeps
  its submitted scope. Ignore late navigation after a newer action supersedes
  it. Mouse activation requires the same visible result or control from press
  to release, including across list scrolling, asynchronous updates, and resize.

## Replacement

- **Replace next** and Enter in the Replace field use the existing exact-match
  behavior. Recheck the match against the current document revision and its
  editability. If no valid current match exists, reveal one and require another
  activation to edit. After a replacement, restore a coherent list for the new
  revision and synchronize it with the existing non-wrapping advancement.
- **Replace all…** opens the existing scope confirmation with Cancel focused.
  It applies to the captured search scope and matching rules, including results
  outside the visible list page. Preserve one Undo step per changed document,
  literal replacement, read-only handling, and reporting of partial outcomes.
  Refresh or explicitly invalidate affected rows after replacement. Selecting
  or opening a result only navigates, including in Find & Replace.

## Acceptance criteria

| ID | Observable outcome |
| --- | --- |
| A1 | Find and Find & Replace open from existing entry points with matching labels and distinct controls. At `80×24`, `40×8`, and after resize, the window leaves the editor visible and the active match can be revealed outside the window. Options and their current state are discoverable when collapsed. |
| A2 | Starting in Find, use arrow keys and Enter to reach flags, browse results, and reach the available actions without Tab. Repeat with mouse only, including wheel and a single result click. Verify Tab, Shift+Tab, F6, the remapped command routes, and Escape and outside-click behavior. |
| A3 | Browse matches beyond the first page of a dense document and jump to the first and last occurrence. In All open, choose a result in another unsaved tab. Verify same-line matches, locations and previews for multibyte/multiline text, selected-row synchronization after Next/Previous, and wheel scrolling without a jump. |
| A4 | Verify empty, no-match, busy, cancelled, and partial outcomes. During controlled search or row-fetch delays, change query, scope, revision, or tab availability and verify stale rows cannot navigate or replace. Verify a press followed by a resize or row change cannot activate the new target. |
| A5 | Select a specific match from Find & Replace, replace it, and verify non-wrapping advancement and refreshed results. Confirm and cancel Replace all in both scopes. Verify one Undo step per changed document, read-only handling, and reporting if work stops partway. Check literal field paste and case and whole-word regressions. |

## Validation record

- Draft basis — Inspected `main` at `3004bb3`, including search command routing,
  rendering, search state, and existing search and terminal tests. Current
  behavior was established by source inspection.
- Pending — A1–A2 layout and input checks in a PTY and an interactive terminal
  with mouse capture. Check standard and minimum dimensions and resizing.
- Pending — A3–A5 focused Rust and PTY tests for navigation, bounded result
  access, invalidation, cancellation, and replacement integrity. Control the
  ordering of delayed results and input in race tests. Check that a deliberate
  break in any changed data-protection guard fails the corresponding assertion.
  Run relevant regressions, `cargo fmt --check`, and Clippy after implementation.
- Open design choices — The window placement, collapsed default, and key map
  above are proposed for review. The compact layout and bounded result storage
  mechanism remain implementation choices subject to the acceptance criteria.
- Next step — Review the draft with the user. Implementation requires separate
  authorization. No runtime validation has been performed for this draft.
