# Improvement Spec — Find and Find & Replace windows

**Status** — The collapsed-title revision is implemented and validated on
macOS and Ubuntu 26.10.
**Release** — User-confirmed patch target `0.1.2`, from `0.1.1` on `main`.
The review build is `0.1.2-dev.3`.
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
The [Minimize addendum](#minimize-addendum) updates the window controls and
search command routes for the same release.

## Windows

Anchor both windows to the bottom right above the status bar, with the result
list above the form. Cap the width at 60 columns and keep at least three editor
rows exposed. Omit the result area when there are no results. Add blank rows
between the list, fields and actions, Options, and navigation when room allows.
Draw a dim horizontal divider when the results and form share the window and
there is room for the divider and at least one result.
Keep the selected match visible by scrolling the editor as needed. The list
scrolls independently of the editor.

**Settings → Editor → Find match rows** controls the maximum visible result
rows in both modes. Default to 3 and accept integers from 1 to 10. Use a
stepper with clickable minus and plus buttons and Left and Right on the focused
setting. Up and Down select Settings rows. Save through the existing Settings
action, using `find_match_rows` under `[editor]` in `config.toml`. Show fewer
rows in short terminals or when fewer results exist. The dialog's Options
continue to contain the search flags, scope, and literal inserts.

The sketches show Find with Options collapsed and Find & Replace with Options
expanded after a replacement. The colored Find button, disclosure marker, and
result status have distinct text cues for monochrome terminals.

```text
┌─ Find ──────────────────────────────────────[−]  [×]─┐
│   3:7    green apple                                  │
│ > 12:1   apple tree                                   │
│   15:7   baked apple                                  │
│───────────────────────────────────────────────────────│
│ Find    [ apple                              ] [Find] │
│                                                       │
│ › Options  Case on · Match Whole Word off · Current tab│
│                                                       │
│ [Previous] [Next]                     2 of 18 matches │
└───────────────────────────────────────────────────────┘

┌─ Find & Replace ────────────────────────────[−]  [×]─┐
│   3:7  notes.txt  Replaced · green apple               │
│ > 12:1 notes.txt  apple tree                           │
│───────────────────────────────────────────────────────│
│ Find    [ apple                              ] [Find] │
│ Replace [ pear                                      ] │
│                          [Replace Next] [Replace All] │
│                                                       │
│ ⌄ Options                                             │
│   [x] Match case  [ ] Match Whole Word                 │
│   Insert [Tab] [LF] [CRLF]                             │
│   [All open tabs]                                     │
│                                                       │
│ [Previous] [Next]                     1 of 26 matches │
└───────────────────────────────────────────────────────┘
```

- Open with the Find field focused. In both modes put **Find** at the right
  edge of the query row. Use the existing green foreground and focus treatment
  from the Settings Save action for the enabled Find button. In Find & Replace,
  put **Replace Next** and **Replace All** on their own row immediately below
  the replacement field, with the group aligned to the same right edge. Use
  cyan for **Replace Next** and yellow for **Replace All**, including the
  confirmation action. Preserve the text labels and focus cues in monochrome.
- Put **Options** and all its controls below those actions in both modes,
  before result navigation. Use an unboxed heading like the Actions category
  rows and Settings sidebar groups. A leading `›` indicates collapsed and `⌄`
  indicates expanded. The whole heading is clickable and shows keyboard focus.
  Click or Enter toggles it, Right expands it, and Left collapses it with the
  heading focused. Start collapsed with the case, whole-word, and scope values
  alongside the heading. Indent **Match case** and **Match Whole Word** when
  expanded. Give the scope selector its own visual position and show its
  current value (**Current tab** or **All open tabs**). Keep Insert Tab, Insert LF and
  Insert CRLF there for the last focused text field. If collapse hides the
  focused control, return focus to Options.
- At `40×8`, use a compact scrollable form and results view with at least three
  full editor rows exposed. Show a clickable way to switch between form and
  results and to scroll to offscreen form controls. Preserve the equivalent
  keyboard routes. Keep focus and the result scroll position across resize
  where their targets remain valid. Below `40×8`, retain the resize prompt and
  field values and inhibit replacement activation.
- Keep F6 available to return focus to the editor without closing the window,
  and keep the existing binding for returning to search. With mouse capture
  enabled, clicking outside the expanded window dismisses it and consumes the
  click.
  Escape first cancels active work, then closes the idle window. Show a
  clickable Cancel search action while work is active. Closing retains the
  entered fields and flags and returns focus to the editor.

## Input and results

- **Form focus** — Up and Down move between form rows. When Options is
  expanded, they reach each control in order, including controls on the same
  row. Left and Right move within the active text field or between controls on
  the same row. Tab and Shift+Tab switch between the form and available
  results, returning to Find from any result. Before results are available,
  Tab and Shift+Tab may still cycle the form controls. Enter activates the
  focused control. Every visible control has a mouse hit target.
- **Search** — Enter in Find or click Find to submit the current literal query.
  Select and reveal the first live result in list order regardless of the
  editor caret, and focus the list once the submitted search finds a match.
  A no-match, cancelled, or superseded search leaves form focus alone.
  The Next and Previous actions keep their existing relative navigation.
  Empty queries clear results and disable match and replacement
  actions. Query or option
  changes invalidate the prior list and cancel superseded search work. The UI
  prompts to search again. Editing only the replacement preserves search
  results and invalidates pending replacement intent. Each field keeps the
  existing 4096-byte UTF-8 limit, literal paste, and safe display of controls.
- **List focus** — Tab moves immediately between results and the form. Up and
  Down move by one result row and stop at the ends. Page Up and Page Down move
  by one visible page. Home and End reach the first and last available rows.
  Moving onto a live match reveals it. Enter or one click reveals a live match
  directly. A replacement feedback row can receive focus for browsing and
  leaves the editor position unchanged. Previous and Next wrap through live
  matches and keep the selection and list viewport
  in sync with the jump. The wheel over results scrolls the list without
  changing the current match, caret, active tab, or editor scroll position.
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
  searches with zero matches. Show Search Details with separate lines for
  per-tab counts and outcomes, a separate replacement section when applicable,
  and readable separators. Escape controls within individual values while
  rendering actual layout line breaks. Up, Down, Page Up, Page Down, Home, End,
  and the mouse wheel scroll long reports within the available content. Every
  match from a successfully scanned target must be reachable, including matches
  beyond the first page. Load result rows in bounded pages or
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

- **Replace Next** and Enter in the Replace field use the existing exact-match
  behavior. Recheck the match against the current document revision and its
  editability. If no valid current match exists, reveal one and require another
  activation to edit. After a replacement, restore a coherent list for the new
  revision and synchronize it with the existing non-wrapping advancement.
- **Replace All** opens the existing scope confirmation with Cancel focused.
  It applies to the captured search scope and matching rules, including results
  outside the visible list page. Preserve one Undo step per changed document,
  literal replacement, read-only handling, and reporting of partial outcomes.
  Refresh or explicitly invalidate affected live matches after replacement.
- **Replacement feedback** — Once an edit is acknowledged, show the occurrence
  in the Find & Replace list with its original preview and a visible text
  label. Use **Replaced** for changed text and **Removed** when an empty
  replacement deleted the occurrence. Keep the label visible when the preview
  is clipped. Replace Next shows the feedback promptly and keeps the next live
  match visible alongside it where space allows. Feedback covers the latest
  replacement action that changed text, including successfully changed
  documents in a partial Replace All. Use the same bounded list mechanism for
  large batches. Only edits that changed text and were committed get a label.
- Feedback rows record where a change was made. They are excluded from the live
  match count, Next and Previous, and replacement actions. When inserted text
  still matches the query, list its live match separately from the feedback
  row. Clear feedback when the window closes or a new query, option, or scope
  takes effect. Clear a document's feedback when it changes again through
  edit, undo, redo, or refresh, or when the tab becomes unavailable.
  Rescanning live matches after replacement preserves that action's feedback.

## Minimize addendum

This approved v0.1.2 follow-up gives the user a visible way to keep a search
running while working in the editor. Before this follow-up, closing the dialog
cancelled the scan and cleared its results. F3 and Shift+F3 could start a fresh
scan from the editor using remembered search text, and Actions listed separate
search, replace, and cancel commands.

- Add **Minimize** (`[−]`, blue foreground) and **Close** (`[×]`, red
  foreground) at the right end of the title row in both modes. Leave at least
  two blank terminal cells between the buttons. Each glyph and its brackets
  form a separate click target, and the gap activates neither button. Both
  controls have visible keyboard focus and remain distinct in monochrome. Up
  from the first form row reaches the title controls, Left and Right switch
  between them, Down returns to the form, and Enter activates the focused
  control.
- Minimize returns keyboard focus to the editor while the same scan continues.
  Collapse the expanded window into its single bordered title row at the bottom
  right above the editor status bar. Keep the same width, full **Find** or
  **Find & Replace** title, and button columns in both views. Switch blue
  `[−]` to blue `[+]` when minimized. Keep red `[×]` and the two-cell gap.
  Show progress, match counts, and applicable cancellation or stale-result
  messages in the editor status bar as space allows. Reserve the title row so
  the caret cannot land behind it. Normal editor input and clicks elsewhere
  leave the title row in place.
- Preserve the mode, field values, flags, and any still-valid results and list
  position while minimized. Scan completion may update the status bar but cannot move
  the caret, scroll the editor, switch tabs, or take focus. Existing revision
  and scope rules still govern result validity, including edits made from the
  editor while the window is minimized. Minimizing cannot confirm a pending
  replacement, repeat an action, or change replacement safeguards.
- **`[+]`** and **Ctrl+F / Cmd+F** reopen the same minimized window, including
  Find & Replace, with the same search and any available results. Opening a
  search from Actions or the Find & Replace shortcut while minimized also
  restores that window. Restore a valid previous form or results focus, with
  the Find field as the fallback. Restoring does not run another search or
  replay a caret jump that was suppressed by minimizing.
- **Close** cancels running work, clears results, and dismisses either view.
  The input values and flags remain available on the next open. In the expanded
  window, **Esc** or the visible **Cancel search** action stops an active scan
  and leaves the window open. **Esc** closes the expanded window when idle.
  With the window minimized, **Esc** in the editor leaves the scan and bar in
  place.
- List only **Find** and **Find & Replace** under **Actions → Search**. Remove
  the default F3 and Shift+F3 bindings and the editor-level routes that start,
  advance, replace, or cancel a remembered search through those command IDs.
  Next, Previous, Replace Next, Replace All, and Cancel search remain available
  from the expanded window when applicable. Existing remaps for these actions
  can operate there only. Close on the minimized bar remains available while
  the editor has focus.

```text
Expanded title   ┌ Find ────────────────────────[−]  [×]┐
Minimized title  ┌ Find ────────────────────────[+]  [×]┐
```

## Acceptance criteria

| ID | Observable outcome |
| --- | --- |
| A1 | Both modes open from existing entry points and show a green, right-aligned Find action. Find & Replace shows cyan Replace Next and yellow Replace All, right-aligned immediately below the replacement field. In both modes, the unboxed Options heading follows the actions, shows its collapsed summary, and expands to reveal Match Whole Word and the separate scope selector. Labels and focus are visible in monochrome. At `80×24`, `40×8`, and after resize, the window exposes the editor and the active match can be revealed outside it. |
| A2 | Starting in Find, use arrow keys and Enter to expand and collapse Options, reach flags, browse results, and reach the available actions without Tab. Repeat with mouse only, including clicking the Options heading, wheel scrolling, and a single result click. Verify Tab, Shift+Tab, F6, the remapped command routes, and Escape and outside-click behavior. |
| A3 | Browse matches beyond the first page of a dense document and jump to the first and last occurrence. In All open, choose a result in another unsaved tab. Verify same-line matches, locations and previews for multibyte/multiline text, selected-row synchronization after Next/Previous, and wheel scrolling without a jump. |
| A4 | Verify empty, no-match, busy, cancelled, and partial outcomes. During controlled search or row-fetch delays, change query, scope, revision, or tab availability and verify stale rows cannot navigate or replace. Verify a press followed by a resize or row change cannot activate the new target. |
| A5 | Select a specific match from Find & Replace, replace it, and verify non-wrapping advancement and refreshed results. Confirm and cancel Replace All in both scopes. Verify one Undo step per changed document, read-only handling, and reporting if work stops partway. Check literal field paste and case and whole-word regressions. |
| A6 | Verify Replaced for changed text and Removed for deletion, including a partial batch and feedback beyond the first page. Verify identical, skipped, and uncommitted edits leave no new marker. Browse feedback without jumping or changing the live count, including when inserted text itself matches. Verify feedback clears on undo, subsequent edits, and search changes. |
| A7 | Both modes anchor at the bottom right and list results above the form. With no saved preference, show up to three result rows. The Settings stepper stops at 1 and 10, accepts keyboard and mouse input, and persists on Save. After restart the preference applies to both modes. Reduce rows as needed at compact sizes and keep the selected document occurrence visible above the window. |
| A8 | With the editor caret at the last match or in a later tab, submit a search and verify the first result is selected. Use Down immediately after completion and Tab or Shift+Tab to return from anywhere in the list. A no-match query leaves the field usable. Verify a visible dim divider when both parts fit and Search Details with per-tab lines and no escaped layout newlines. |
| A9 | In both expanded modes, verify blue Minimize and red Close are individually clickable and keyboard reachable, with two blank, inactive cells between them. Verify monochrome focus. At `80×24` and `40×8`, minimize to a single bordered row in the same width, with the full title, a blue `[+]` at Minimize's column, and the red `[×]` at the same column. Verify independent click targets, resize, and progress or counts in the editor status bar. |
| A10 | Hold a scan at a controlled point, minimize it, and verify the editor accepts input without losing focus. Verify both restore routes while busy and after completion, preserved search state without resubmission, and no delayed caret or tab jump. Exercise Current tab invalidation and stale revisions while minimized. Verify Close cancels and clears in both views and Esc preserves the expanded window while cancelling. |
| A11 | Verify Actions → Search contains only Find and Find & Replace. F3, Shift+F3, and an existing remap for a removed editor-level route cannot start a scan with Find closed. Verify in-window match navigation and replacement, including Replace All confirmation and cancellation. |

## Validation record

- Collapsed-title revision (A9–A10) — The user requested a true single-row
  title bar at the regular dialog width, with `[+]` in the Minimize position.
  The new terminal-cell regression failed on `dev.2` because the minimized
  row was 50 columns wide and the expanded row was 60. The `dev.3` check verifies
  width and position, the full titles, button columns, colors, inactive space,
  and progress in the editor status bar at `80×24` and `40×8`. Native terminal
  tests cover mouse and keyboard restore, close, resize, and stale results.
  Formatting, strict Clippy, and the full locked workspace suites passed on
  arm64 macOS 27.2.1 and the arm64 Ubuntu 26.10 UTM guest with Rust `1.100.0`.
  The suites passed 392 and 395 tests respectively, with 10 and 11 existing
  ignored tests.
- Minimize addendum (A9–A11) — The user approved the interaction, spaced
  title controls, and implementation for `0.1.2-dev.2`. A new native terminal
  test failed on `dev.1` because Minimize was absent. Terminal-cell and native
  PTY checks verify spacing and colors, keyboard and pointer controls, restore
  in both modes, the caret above the bar at `40×8`, stale rows after editing or
  switching tabs, and the two Search menu items. The removed key bindings and
  remapped command boundaries are checked through application dispatch. The
  retained stream regression advances through the visible **Next** action.
- Minimize concurrency and review (A4, A5, A10) — Gated native runtime tests
  hold the scan and a later viewport read. They verify continued scanning,
  restore without resubmitting, cancellation and close, and no late caret or
  active-tab change. Independent review rechecked these paths and the existing
  stale replacement guard, with no remaining actionable findings.
- Minimize regression runs — Formatting and strict Clippy passed with Rust
  `1.100.0` on arm64 macOS 27.2.1 and the arm64 Ubuntu 26.10 UTM guest. The
  full locked workspace suite passed 391 and 394 tests respectively, with
  10 and 11 existing ignored tests. Both used the CI 256-file limit.
- Minimize optimized Linux checks — Twelve enabled native search tests
  passed. The ordinary, dense-line, and long-line 256 MiB search and replacement
  benchmarks passed with a 256 MiB memory limit and swap disabled. The full
  1 GiB benchmark matrix was not run for this build.
- Second follow-up (A2, A3, A8) — Authorized first-result selection, automatic
  results focus, Tab between form and results, a soft divider, and structured
  Search Details. A native terminal regression reproduced selection of the last
  match when the caret started there. Native tests verify selecting the first
  occurrence in both modes, first-tab ordering for All open tabs, immediate arrow
  navigation, Tab and Shift+Tab, no-match editing, minimum-size layout, the divider,
  and per-tab report lines. A gated scan verifies stale completions preserve newer
  focus and that a replacement receipt cannot stand in for the first live row.
  The wrapped report test checks bounded scrolling and escaping only within values.
- Second follow-up regression runs — Formatting and Clippy passed on macOS 27.2.1
  and the Ubuntu 26.10 UTM guest, with Rust `1.100.0` on arm64. The full locked
  workspace suite passed 387 and 390 tests respectively with the CI 256-file limit.
  Independent review rechecked the async focus, report scroll limits, and stale
  replacement guard and found no remaining actionable issues.
- Second follow-up optimized Linux checks — Ten enabled native search tests and
  the ordinary, dense-line, and long-line 256 MiB benchmark fixtures passed
  on the Ubuntu guest. The benchmarks ran with a 256 MiB memory limit and swap
  disabled. Ubuntu 26.04 verification is pending.

- Follow-up (A1–A3, A7) — Approved bottom-right placement, matches above the
  form, a shared 1–10 row stepper with default 3, vertical arrow traversal
  through Options, clearer scope labels, and section spacing when room allows.
  Before the fixes, real PTY tests reproduced Down skipping Match Whole Word
  and a one-row wheel step skipping the next match. The settings schema tests
  cover both bounds and reject invalid integers and value types.
- Follow-up review (A1–A5, A7) — Independent review caught multibyte match
  reveal, compact list focus, and an intermittent Replace All click dropped
  during an empty result-page refresh. Native PTY regressions cover Unicode
  reveal, resize in both directions, stepper persistence, arrow-only Options,
  wheel-only browsing without a caret jump, and click-to-jump. The capture
  signature test verifies visible result metadata still invalidates old hits.
  The reviewer also verified twelve consecutive native Replace All runs and
  rechecked selection preservation and one-row browsing after the fixes.
- Follow-up regression runs — Formatting and locked workspace Clippy with
  warnings denied passed on macOS 27.2.1 and the Ubuntu 26.10 UTM guest with
  Rust `1.100.0` on arm64. The full locked workspace suite, with the CI
  256-file limit, passed 383 tests on macOS and 386 on Ubuntu. The standard
  run reported 10 and 11 existing ignored tests, respectively.
- Linux release checks — The guest passed optimized native Find and Settings
  tests and the search and replacement benchmark on ordinary, dense-line,
  and long-line 256 MiB fixtures with a 256 MiB memory limit and no swap.
  The available UTM guest reports Ubuntu 26.10. Ubuntu 26.04 and the full
  1 GiB CI benchmark matrix remain unverified on the guest.
- Draft basis — Inspected `main` at `3004bb3`, including search command routing,
  rendering, search state, and existing search and terminal tests. Current
  behavior was established by source inspection.
- Layout revision — Checked the Actions category and Settings sidebar group
  rendering and the Settings Save button styling in `src/ui.rs`. Incorporated
  the requested action placement, labels, color distinctions, and disclosure
  style into both sketches.
- Test-first check — The new occurrence-navigation PTY test failed against
  the bottom dock because the Options heading was missing.
- A1–A3 — Terminal-cell color and alignment assertions, plus real PTY tests
  at `80×24` and `40×8`, cover Options, resizing, mouse and arrow navigation,
  wheel scrolling without a caret jump, and a jump into an unsaved tab.
  The 64-tab search and replacement test passed with a 256-file limit.
- A4–A6 — A gated page-read test held a selection unresolved while a replace
  request arrived. An isolated removal of the expected-selection guard made
  the content assertion fail by replacing the preceding occurrence. Focused
  tests cover confirmation and cancel, non-wrapping advancement, undo,
  deletion deep in the list, and live versus replaced case-folded matches.
- Implementation — The compact window switches between a scrollable form and
  the list. The coordinator spools exact ranges to temporary storage and
  prepares at most 16 previews per request on a worker. Single-occurrence
  replacement feedback appears near its original match position.
- Independent review — The separate reviewer rechecked selection authority,
  snapshot leases, passive scrolling, and single-occurrence feedback after
  the fixes. The review found no remaining high-severity issues in those
  paths.
- Regression run — On arm64 macOS with Rust `1.100.0`, formatting and
  workspace Clippy passed. The full locked workspace suite passed with 379
  tests passing and 10 existing ignored tests, with the CI 256-file limit and
  native controlling-terminal access. Linux release benchmarks were pending
  at the initial handoff.
- Release baseline — Fetched upstream before handoff. `origin/main` remained
  at `3004bb3` with package version `0.1.1`. Finalizing `0.1.2` awaits
  explicit release approval.
- Authorization — The user requested implementation on 2026-11-24.
