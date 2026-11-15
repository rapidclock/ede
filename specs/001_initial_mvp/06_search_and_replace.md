# 06 — Search and replacement

**Status:** Implementation in progress, authorized by the user's request to finish
Stage 06 on 2026-11-14. The independently reviewed design below is the contract.

Parent: [MVP overview](overview.md). Depends on Stages 02–05, merged through
[PR #5](https://github.com/rapidclock/ede/pull/5).
Branch: `stage/search_and_replace`, based on `feat/initial_mvp` at `c0d8824`.
PR target: `feat/initial_mvp`.

## Outcome and decision status

Deliver literal search and replacement over current document contents, including
unsaved edits and persistent scratches. Users can search the current tab or all
open documents, navigate in both directions, and replace one or all matches.
Large files remain searchable and editable without whole-document RAM copies.

Required by FR-08 and the roadmap: case sensitivity, current/all-open scopes,
visible feedback, bounded cancellable work, undoable replacement, explicit
partial outcomes, and an interface for Stage 08 read-only streams. Preserve
FR-02/03/05/06/10 integrity, keyboard/mouse access, responsiveness, and recovery.

The detailed choices below are the stage decisions used for the authorized
implementation. They resolve whole-word behavior, Unicode case matching,
multiline input, navigation, resource defaults, and per-document replacement
semantics for review. No prerequisite product question is left unspecified;
the user has authorized implementation. Library feasibility and benchmark evidence
remain implementation gates.
Regex syntax, unopened-file/project search, cross-document undo, and automatic
disk saves are outside this stage. Configuration/remapping remains Stage 09;
stdin ingestion and direct file following remain Stage 08.

## Matching contract

- Search logical UTF-8 text from immutable document revisions, excluding the
  serialization BOM. Preserve original byte offsets in every match. Never search
  only the named file on disk or expose private recovery/storage files as tabs.
- Default to **Match case on**, **Whole word off**, **Current document**. Empty
  query means no search; disable navigation/replacement and clear old highlights.
  An empty replacement deletes matches. Query/replacement strings are literal:
  punctuation, backslashes, `$1`, and regex metacharacters have no special meaning.
- Match-case compares exact scalar sequences. Case-insensitive matching uses
  Unicode simple case folding, independent of locale, with the Unicode tables of
  the locked matcher version. `K`, `k`, and the Kelvin sign match; Greek sigma
  variants match; `ß` does not match `ss`. Do not normalize composed/decomposed
  text or lowercase a copied document and lose its original offsets.
- Whole word is an additional checkbox. Accept a literal occurrence only when
  neither adjacent scalar is a Unicode word character: Alphabetic, Mark,
  Decimal_Number, Connector_Punctuation, or Join_Control (the Rust regex Unicode
  [`\w` definition](https://docs.rs/regex/1.13.1/regex/#perl-character-classes-unicode-friendly)).
  Document edges count as boundaries. This also applies to
  punctuation-only and multiline queries; it does not implement language-aware
  identifiers or word segmentation. A rejected candidate must not hide a later
  overlapping candidate that satisfies the boundaries.
- Queries may contain actual tabs and newlines. Match line endings exactly:
  LF and CRLF differ. Treat CRLF as indivisible for match endpoints, consistent
  with the storage edit contract: a lone LF cannot match the LF half of CRLF.
  Include surrounding scalar/CRLF context when checking chunk boundaries.
- Define one canonical match sequence per revision: scan from byte zero,
  choose the leftmost valid occurrence, then resume at its end. Matches do not
  overlap; `aa` in `aaa` yields only `[0,2)`. Counting, next/previous, highlighting,
  and replacement use that sequence. Reverse navigation reverses this sequence,
  rather than performing a different right-to-left greedy search. No zero-width
  matches exist because an empty query is disabled.
- Matches use scalar-aligned, edit-valid byte ranges. When a match bisects a
  displayed grapheme cluster, paint its intersecting cells and keep the exact
  match range separately. Ordinary caret/selection stays on Stage 03 grapheme
  boundaries; Copy/Cut/editing uses ordinary selection, while Replace uses the
  exact search range. Identify the current match visibly without implying that
  a snapped selection is the replacement target.

## Commands and interaction

Use shared named commands for keyboard, mouse, Help, and Actions; add Find,
Find next, Find previous, Replace, Replace next, Replace all, and Cancel search.

| Action | Proposed default / behavior |
| --- | --- |
| Find | Ctrl+F, delivered Cmd+F; open Find with query focused |
| Replace | Ctrl+H, delivered Cmd+Shift+H; open the replacement controls |
| Find next / previous | F3 / Shift+F3, including when the panel is closed |
| Submit query | Enter in query runs Find next; Enter in replacement runs Replace next |
| Cancel / dismiss | Escape first cancels work; a subsequent Escape dismisses the idle panel |

- Provide a non-modal search panel with query, replacement (in Replace mode),
  Match case, Whole word, Current/All open scope, Previous, Next, Replace next,
  Replace all, Cancel, and Close. Every control has a mouse hit target and a
  keyboard focus route. F6/Shift+F6 moves between editor and controls;
  Tab/Shift+Tab cycles panel controls. Actions supplies routes for intercepted
  shortcuts. Replace is unavailable in Find-only mode until explicitly opened.
- Retain the last valid query, replacement, and options session-wide, without
  persisting them to configuration or recovery. Opening does not overwrite the
  query with selected text. Typing changes fields but does not launch a scan:
  submit explicitly. Changing query/options invalidates previous results and
  cancels their jobs. Changing replacement text invalidates pending replacement
  intent, without invalidating an otherwise current search.
- Fields accept valid UTF-8, each at most 4096 bytes, without NUL. Literal tabs
  and newlines can be pasted; offer focusable **Insert Tab**, **Insert LF**, and
  **Insert CRLF** controls for the last focused text field. Display controls
  safely and distinguish actual LF/CRLF/tab from literal backslash sequences.
  Reuse bounded ordered field-paste handling; an invalid/oversize paste leaves
  the prior field intact and reports an error, with no document edits. Enter
  submits and Tab changes focus, rather than inserting field text.
- At 40×8 use a scrollable panel with the focused control brought into view;
  it must leave a usable text viewport. Below 40×8 preserve fields and pending
  state, inhibit destructive decisions, and retain the existing resize prompt.
  Resize/hit-testing uses layout generations; stale clicks cannot activate
  replacement controls. Modal dialogs take input before the panel.
- Closing the panel cancels outstanding work and clears match decorations,
  preserving fields and ordinary selection/view. Find next/previous reuses the
  last valid search, or opens Find when none exists. Escape does not undo an
  acknowledged replacement or cancel unrelated saves/checkpoints.

### Scope, navigation, and feedback

- Capture Current's tab at command dispatch. All open captures the tab IDs in
  creation order, up to the existing 64-tab limit, including inactive scratches.
  New tabs and Recovery entries not opened as tabs are not added mid-request.
  Identify loading, failed, closing, or closed targets as skipped; never wait
  for an indefinitely blocked open or silently report a complete zero count.
- Capture one document snapshot at a time, after edits already accepted for that
  target have settled. All-open results describe these per-document revisions,
  not one simultaneous session snapshot. Store a bounded revision/count/outcome
  summary per target. A later revision invalidates its count/highlights. Mark
  the aggregate incomplete/stale until explicitly searched again.
- Within a document, the initial Next selects the first canonical match whose
  start is at or after the caret; initial Previous selects the last whose end
  is at or before the caret. With a current valid match, Next/Previous chooses
  its successor/predecessor. Ordinary caret movement or editing clears that
  match anchor. A search command captures its caret anchor at dispatch.
- All-open navigation visits the remainder of the anchor tab in the requested
  direction, then successive/predecessor tabs in captured order, wrapping once
  and finishing the remaining portion of the anchor tab. Current scope wraps
  within that document. Stop after one complete circuit. Show **Wrapped** or
  **No matches**; a single match may be revisited only after wrapping. Previous
  must work with bounded memory, even if it requires a forward rescan.
- A navigation result may activate its target and reveal the current match only
  if that request still owns navigation: changing tabs, moving the caret,
  starting a modal, closing the panel, or issuing a newer navigation command
  revokes that authority. Background counting never changes focus or view.
  Tab switching invalidates Current's old target; All open retains its captured
  scope but cannot install stale navigation.
- On accepted navigation, clear the target's ordinary selection and place its
  caret at the grapheme boundary at or immediately before the exact match start.
  Reveal the match with minimal scrolling; retain a separately identified exact
  current-match range. Copy/Cut without a new ordinary selection follows Stage
  03's no-selection behavior; typing inserts at that caret. The panel must make
  this distinction visible. Automatic find after Replace next uses the same
  policy. Programmatic placement does not itself clear the new match anchor.
  Rejected/stale navigation changes neither caret nor selection.
- Show query options, scope, target name, scanning/cancelling state, bytes scanned
  versus snapshot length, and partial/complete match counts. Report per-tab
  skipped/stale/failed outcomes through a bounded, keyboard/mouse-accessible
  summary. Never present a partial count as final. A scan error preserves the
  last good document and permits explicit retry. Escape/Cancel remains usable
  while a worker is stalled in I/O.
- Highlight current and visible matches in the active viewport using bounded
  derived work. The current match is distinct from ordinary selection. Validate
  canonical non-overlap from the document start or a proven continuation checkpoint;
  a viewport-only scan must not invent a different sequence. Until that work is
  available, show current match/progress rather than incorrect highlights. Scans
  may publish an early navigation candidate before full counting finishes.

## Replacement and document integrity

- Replace next operates on the current exact match only if query/options,
  tab/document ID, revision, and range are still valid and the target is editable.
  If none exists, find/reveal the next match and require a second activation to
  replace it. Revalidate through storage at commit; never replace a snapped
  selection or use an old match after an edit, undo, redo, refresh, or close.
- Commit one replacement as one undo transaction. Then clear the old match and
  find the next canonical match in the new revision from the end of the inserted
  bytes. Do not recursively replace text just inserted in the same command.
  A later explicit command may wrap and revisit that text. Deletion resumes at
  the deletion point. A no-op replacement counts as an occurrence processed but
  creates no dirty state/history entry; navigation must still advance. Retain
  an explicit continuation after the processed occurrence even when the revision
  did not change; do not treat the unchanged revision as an unprocessed match.
  Automatic continuation never wraps: stop with **No further matches** at the
  scope end. For Current, scan from the continuation offset through EOF. For
  All open, scan that suffix of the processed tab, then later captured tabs in
  creation order, stopping after the last captured tab. Do not cross back to
  earlier tabs or the processed tab's prefix. Only a later explicit navigation/
  find command may wrap, so a sole no-op match is not immediately reselected by
  automatic advancement.
- Replace all captures scope and fields, then shows a confirmation identifying
  Current/All open and the literal query/replacement/options; Cancel is focused.
  Confirmation does not imply a precomputed count. Do not permit another replace
  request to overlap. At confirmation, drain earlier accepted edits and process
  captured targets sequentially in creation order, capturing each current revision
  at its turn. A target closed or made noneditable is skipped visibly.
- For each editable target, stream all canonical matches of that snapshot into
  a sorted, nonoverlapping disk-spooled `StagedTransaction`. Scan each original
  occurrence once; replacement text is never rescanned within that batch. Publish
  only the complete document transaction against its captured revision. Zero
  matches and identical resulting text create no new history/dirty state.
- Each changed document gets one undo step for the whole batch, with normal
  per-document redo, save-point/dirty semantics, and view-history bookkeeping.
  Preserve BOM, existing line endings outside replaced ranges, and suggested
  newline metadata; replacement inserts exactly its literal bytes. The ordinary
  view maps unaffected anchors through edit deltas; anchors inside a replaced
  range go to the replacement end, then clamp/snap to valid grapheme boundaries.
  Apply the same rule to inactive tabs, without stealing focus.
- Map the latest acknowledged caret, selection, preferred column and viewport
  when reconciling publication, including navigation/scrolling performed during
  planning or application. Do not restore a dispatch-time view snapshot. Mapping
  prepared off-thread must validate its view generation and remap newer view
  state before installation. Replace all preserves active tab/focus. Replace
  next's automatic navigation may move the view only while it still owns the
  navigation authority defined above; an intervening user move takes precedence.
- Keep the existing 1,000-transaction/256 MiB retained-text history policy. Prune
  only permitted older transactions with visible feedback. If the newest batch
  cannot remain undoable, reject that document before publication. Do not silently
  split it into partially committed edits or reset history to fit.
- Cancellation, staging/read/write/space failure, and stale-revision rejection
  before a document's publication leave its text, dirty state, and undo/redo
  unchanged. Stop the run on cancellation, a failed target, or a revision conflict;
  leave subsequent targets unprocessed. Already committed earlier documents stay
  changed and undoable. No cross-tab rollback or atomic disk-save promise exists.
- Check cancellation again at publication. If cancellation races with a completed
  commit, acknowledge and count that commit exactly once, then stop. Reconcile
  commits even for inactive tabs or dismissed UI. The completion summary lists
  successfully completed documents, occurrences processed, actual changed
  documents, skipped/read-only targets, failed target/reason, and unprocessed
  targets. Count processed occurrences only from acknowledged successful document
  outcomes, including successful no-op batches. Staged/scanned occurrences in a
  failed or pre-commit-cancelled document contribute zero; show any staging
  diagnostics separately. A commit completed before cancellation but acknowledged
  afterward contributes its full count exactly once. A retry
  is a new explicit run over current contents, never an automatic replay of
  earlier successful replacements.
- Replacement does not save named files. Existing guarded Save, Save As path
  reservations, external-change warnings, and uncertain-publication handling
  remain authoritative. A concurrently completing save acknowledges only its
  captured state, leaving newer replacements dirty. Scratch replacement schedules
  ordinary checkpoints and is flushed on normal quit.

## Coordination, interfaces, and resource policy

The interface names here are conceptual contracts; implementation may choose
module/type names while preserving behavior.

| Boundary | Required contract |
| --- | --- |
| UI → coordinator | Search spec (literal, case, whole-word, scope), navigation anchor, stable request ID, immutable target list; separate replacement intent with its captured fields |
| Coordinator → worker | Budgeted `BackgroundSnapshot`, document/revision identity, bounded compiled matcher, cancellation, scan continuation; no UI-thread disk reads or matching |
| Worker → coordinator | Bounded/coalesced progress, exact match ranges, per-document counts/outcomes; include request/query generation, tab/document and revision |
| Worker → storage | `BeginTransaction`/`StagedTransaction::replace`/`Apply` with expected revision and cancellation; preserve storage's atomic root/history publication |
| Coordinator → view | Install navigation only with valid navigation authority; decoration additionally matches layout generation; reconcile committed mutations regardless of view authority |
| Future stream adapter | Snapshot of retained extent with revision/extent identity and an explicit `can_replace` capability; no inference from filename or dirty state |

- Use one search/planning worker session-wide and one running search or replace
  job, plus at most one coalesced pending search intent. A new search cancels and
  supersedes older search work. While replacement is active, reject another
  search/replace with Busy feedback until cancellation/commit reconciliation
  completes. Work that loses authority releases snapshots and staged files on
  the worker; it cannot later revive a tab, overwrite a newer query, or mutate
  another document. A cancellation request is independent of queue capacity.
- Separate scans and transaction planning from foreground view reads, opens,
  saves, checkpoint writes, and the storage mutation owner. Storage application
  of a large staged batch must also preserve foreground service: extend the
  existing owner path to yield bounded preparation slices or prepare a candidate
  off-thread and perform a short revision-guarded publication. Merely moving
  matching off-thread while `Apply` monopolizes all document service is insufficient.
  Preserve per-document input order and never overwrite edits accepted meanwhile.
- Edits remain available while scanning/planning; a changed target invalidates
  its result and makes replacement fail its final revision guard. Do not retry
  automatically against different text. External refresh/undo/redo likewise
  invalidate results. Search snapshots pin old immutable generations without
  blocking compaction. Yield/release them when compaction needs resources and
  report interruption rather than pinning obsolete generations indefinitely.
- Explicit Close or Quit first cancels a replacement involving its targets and
  reconciles any in-flight publication before applying Stage 04/05 guards.
  Search-only work can be cancelled without blocking prompts. Quit stops new
  replacement admission, reconciles commits, then checkpoints current scratch
  contents; Cancel quit preserves every committed replacement and unsaved byte.
  Shutdown restores terminal modes before any worker I/O join, as before.
- Resource defaults are proposals to validate within the existing session-wide
  128 MiB managed-memory budget and 256 MiB peak-RSS target. Reserve search
  allocations within the 32 MiB shared buffer/payload allowance, not in an
  additional unaccounted pool. Maximum search-owned resident allocation is
  8 MiB including matcher compilation/cache, fields, scan buffers, summaries,
  match decorations, channels, and transaction bookkeeping. Reject admission
  visibly if this or the shared budgets cannot be reserved.
- Use scalar-aligned scan chunks of at most 64 KiB plus bounded overlap/context.
  For a query of N scalars, Unicode simple folding consumes at most 4N bytes per
  occurrence; retain that much lookahead plus adjacent scalar/CRLF context.
  Carry canonical resume offsets so overlap never duplicates/skips occurrences.
  Do not allocate a whole line, document, all matches, or all transaction
  descriptors. Keep at most one search snapshot and one transaction plan active,
  at most 64 target summaries and 256 visible match decorations. If a viewport
  exceeds that decoration limit, visibly indicate partial highlighting; navigation,
  exact total counting, and replacement must still cover every match.
- Bound search command/result channels to 16 entries each and charge payloads;
  progress coalesces. Dense matches cannot flood the UI queue. Check cancellation
  between chunks and at least every 256 candidates/descriptors. Yield after
  at most 64 KiB scanned or 256 replacements prepared per scheduling slice;
  matching/compilation must have bounded library work and memory as well.
  An in-progress OS call may delay completion, but never input/Cancel dispatch.
- Disk-spooled plans grow with matches and replacement bytes; charge them and
  pinned generations to working-space accounting, enforce the existing 512 MiB
  free-space reserve, and handle actual write failure after preflight. Cancellation
  and failure reclaim abandoned plans/generations without deleting current/history,
  saved baselines, or complete recovery objects. Use checked byte/count arithmetic;
  overflow is an explicit failure, never a wrapped count or partial publication.

### Matcher evaluation and dependency gate

Evaluate ripgrep's libraries before implementing a matcher. Primary documentation
was checked on 2026-11-14; versions below are observed research versions, not
new locked dependencies:

| Candidate | Evidence and proposed use |
| --- | --- |
| [`grep-searcher` 0.1.17](https://docs.rs/grep-searcher/0.1.17/grep_searcher/struct.SearcherBuilder.html#method.multi_line) | Provides line-oriented scanning and multiline support. Its reader multiline mode requires whole-input buffering; line buffering is also a concern for unbounded lines. Proposed: do not use its document reader loop for this bounded paged search. |
| [`grep-matcher` 0.1.9](https://docs.rs/grep-matcher/0.1.9/grep_matcher/) | Low-level matcher trait accepts byte slices and reports byte offsets. Proposed: use this interface on bounded snapshot windows. |
| [`grep-regex` 0.1.14 builder](https://docs.rs/grep-regex/0.1.14/grep_regex/struct.RegexMatcherBuilder.html) | Offers literal construction, case/Unicode settings and compiled/cache limits. Proposed: use a literal matcher with Unicode simple folding and separately checked whole-word/CRLF boundaries; never expose regex syntax or replacement expansion. |

This is an architectural inference from the documented APIs, not measured proof
that these versions meet the budget. Before adding dependencies, use a focused
Rust feasibility check for literal multiline data, Unicode offsets/folding,
maximum query expansion/compile memory, chunk overlap, bounded cancellation,
long lines, and dense matches. Bound compiled program and DFA cache separately
inside the 8 MiB allowance; compilation limit failure is a visible query error.
Check pinned toolchain compatibility, maintenance, licenses, and transitive/build
cost. Record versions and findings here. If the proposed adapter cannot meet the
contract, revisit the design before implementation proceeds; do not silently
substitute ASCII-only matching, regex behavior, or a file-size/search cutoff.

## Acceptance criteria and validation plan

Write focused Rust checks before behavior changes and observe their expected
failure, then use normal storage/coordinator/PTY entry points. Force consequential
orderings with explicit synchronization; sleeps alone are not race evidence.

| ID | Observable acceptance check |
| --- | --- |
| S01 | Reference-model checks compare complete canonical ranges/counts for literal punctuation, empty query, deletion, overlapping candidates, multiline LF/CRLF, BOM, tabs, combining/wide scalars, and matches spanning text pages, piece boundaries, and read chunks. Include tiny chunk sizes and whole-word context at every boundary. |
| S02 | Match-case and Unicode simple-fold checks cover Kelvin/K/k, sigma variants, `ß` versus `ss`, locale independence, composed/decomposed text, variable UTF-8 widths, and whole-word Alphabetic/Mark/decimal/connector/join-control boundaries. Verify backward navigation reverses the forward sequence. |
| S03 | PTY and coordinator checks exercise Find/Replace, submitted fields, all controls with mouse/keyboard, invalid/oversize/ordered paste, literal replacement syntax, LF/CRLF/tab insertion, panel closing, F3/Shift+F3, wrapping/no match/single match, 40×8 and smaller resize, modal precedence, grapheme-safe views and exact replacement ranges. Navigate from an unrelated selection to a grapheme-bisecting match, then Copy/Cut/type; verify cleared selection, snapped caret, exact Replace range and the same policy after automatic navigation. |
| S04 | Search/replace current and all open across dirty named files, inactive scratches and duplicate path aliases with different edits. Check dispatch anchors, creation order, loading/closed/new-tab scope, partial counts, error summaries, and no disk-file shortcut. A blocked open must not block search of Ready tabs. |
| S05 | Replace next must reveal before mutating without a valid match. Verify exact replacement, deletion progression, no-op advancement, one-step undo/redo, saved state and view mapping. In All open, replace in a middle/last tab with matches only in earlier tabs or its own prefix; automatic continuation must stop without wrapping, including for no-op/deletion, while later explicit navigation may wrap. Replace all stages original matches once, changes each document atomically, preserves untouched bytes/BOM/newlines, and gives one undo step per changed document. |
| S06 | Inject read/stage/write/space/overflow/history-budget failure and cancellation before staging, during scanning/application, and just before publication. Assert current document content/history/dirty state unchanged before commit, earlier documents retained after partial failure, later ones untouched, retry behavior and temporary-space reclamation. Assert exact processed counts exclude the failed/cancelled document's staged records, include successful no-op batches, and count a forced commit before cancellation/acknowledgement exactly once. |
| S07 | Gate scan/navigation and replacement publication against edits, undo/redo, refresh, compaction, tab switch/close, newer query and layout generation. Assert stale results cannot move focus, decorate new text or commit old ranges; edits survive. Navigate, select and scroll during gated replacement planning/application and between commit and acknowledgement; verify latest view mapping, inactive-tab state, and no focus theft or obsolete automatic navigation. Verify foreground reads/edits and Cancel dispatch while matching, I/O or large transaction preparation is gated. |
| S08 | Gate Save/Save As, checkpoint, Close and Quit against replacement. Check exact saved revision, newer dirty state, path reservations, unchanged conflict warnings, commit reconciliation before close/quit decisions, cancelled quit preserving edits, normal-exit scratch restoration, and existing terminal restoration ordering. |
| S09 | Test the future stream capability contract with a synthetic retained snapshot: read-only history is searchable, replacement is explicitly skipped/disabled and counted, append/extent changes revoke stale results, and an EOF-style editable revision accepts replacement. Real ingestion/follow/rotation integration remains Stage 08. |
| S10 | Stress dense matches, maximum-length queries, 64 tabs, bounded queues and repeated cancellation with measured allocation/lease/disk high waters. Run release PTY search and replacement on the existing 1 GiB ordinary/dense/long-line corpora, measure first text, ordinary-input p50/p95/max, cancellation dispatch/completion, scan/application duration, peak RSS and space. Exercise a real file larger than RAM under the existing swap-disabled Linux constrained-memory harness. |

Map S01–S10 to named tests and actual results in the Validation record during
implementation. Run `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets --locked -- -D warnings`, and
`cargo test --workspace --locked`, including Stage 02–05 regressions. For changed
revision/commit/close/quit guards protecting user data, deliberately break each
affected guard in an isolated copy after its baseline passes; show that the
intended preservation assertion fails, not just the build.

Record native macOS/Linux and PTY/tmux evidence separately from physical terminal
and remote key/mouse delivery. For S10 record hardware, toolchain, filesystem,
corpus bytes/match density, cache state, memory/swap limit and repeat commands.
Compare first usable text and ordinary input to the existing 1 s / 50 ms local
goals and Stage 05 baseline. Full scans/replace-all are allowed to take longer;
they must show progress, remain cancellable, and keep foreground input usable.
Any missed requirement needs a fix or explicit user-approved change, not an
unrecorded deferral to Stage 10.

## Validation record

**Implementation validation: native checks complete; Linux CI pending.** The user
authorized completion after design review. Earlier checkpoints are retained below;
the integrated evidence and final measurements supersede their pending notes.

- Test-first baseline: `cargo test --offline --locked --test search` initially
  failed because the search API did not exist. The matcher checks now pass for
  tiny windows, multiline/CRLF, literal metacharacters, Unicode folding/word
  boundaries, original offsets, and dense-match cancellation.
- An isolated Rust feasibility executable tested `grep-matcher` 0.1.9 and
  `grep-regex` 0.1.14 with a 2 MiB compiled-program limit and 1 MiB DFA cache.
  Literal multiline input, Kelvin/sigma/simple folding, a 4096-byte query matching
  12288-byte folded text, and stopping dense iteration at 256 candidates passed.
  Both crates are Unlicense OR MIT and build on the pinned Rust 1.99.1 toolchain.
  The six new locked transitive/direct packages use existing `memchr`/`log`;
  `grep-searcher` is deliberately omitted because its reader loop buffers full
  lines or whole multiline input. Compilation/scan peak measurements remain pending.
- `cargo test --offline --locked --test prepared_edit` first failed for missing
  preparation/publication methods. It now passes for unchanged live contents
  during preparation, a two-range atomic replacement/undo, and stale-revision
  rejection preserving newer text.

Design baseline: `feat/initial_mvp` at `c0d8824` (Stage 05 merged). Inspected
`AGENTS.md`, `REVIEW.md`, the accepted overview/roadmap, Stage 02–05 contracts,
and current snapshot, transaction, service, command, and session boundaries.
External matcher documentation informs the proposal above; feasibility,
licenses/toolchain/dependency cost, memory limits, and performance remain pending.

Design checks on 2026-11-14, macOS 27.2 arm64, Rust 1.99.1:

- Branch/base confirmed: `stage/search_and_replace` starts at project revision
  `c0d8824`; Stage 05 is merged. Only this specification and the overview change.
- Local Markdown link/fragment check passed (14 references across both changed
  documents); S01–S10 are unique and the last section is Validation record.
  Whitespace checks and `git diff --check` passed.
- `cargo fmt --all -- --check` and
  `cargo clippy --workspace --all-targets --offline --locked -- -D warnings` passed.
- `cargo test --workspace --offline --locked` passed: 184 tests, zero failures,
  six explicitly ignored benchmarks/environment checks. This is an unchanged
  Stage 05 regression baseline, not evidence of implemented search behavior.

Independent Review agent read `AGENTS.md` and `REVIEW.md`, reviewed this complete
working-tree specification and overview diff against `c0d8824`, and traced the
relevant Stage 02–05 contracts and current storage/editor/UI boundaries. It
independently confirmed branch/base, two-document scope and clean diff whitespace.
It considered the implementer's Cargo baseline above without rerunning it.

Three review findings were fixed: ambiguous selection/caret after navigation,
stale view restoration during replacement, and processed counts after partial
failure. A follow-up clarified no-op/deletion continuation and the all-open
creation-order suffix without automatic wrap. S03 and S05–S07 now specify the
corresponding acceptance checks. The reviewer inspected the material fixes and
reported **no remaining findings**. Local link/structure/scope checks were rerun
after the fixes. These are design-review results, not runtime acceptance evidence.

Native Linux, real-terminal/remote compatibility, performance, synchronized
failure/cancellation checks, and mutation evidence remain pending implementation.

### Earlier implementation checks

The implementation now uses bounded literal matching, one active search/planning
job, captured scopes/navigation authority, per-document atomic prepared batches,
and a non-modal keyboard/mouse panel. `PrepareEdit` / `FinishEdit` fork/apply
private generations off the owner, then guard the short publication. Private
application preserves source/tree IDs for in-flight saves instead of compacting
inside the batch. Repeated-batch reclamation is still being validated.

The independent core reviewer reproduced save-baseline corruption after private
compaction, including an older save acknowledged after a batch. Both regressions
now pass. The implementation reviewer subsequently reproduced uncontrolled
failed-scan retries, delayed navigation revocation, abandoned busy state, and
foreground cleanup. These have focused regression checks and material fixes:
dispatch-stamped navigation authority, explicit error finalization, latched scan
failure, and bounded worker reclamation of complete prepared/retired bundles.
The reviewer inspected those fixes; final acceptance review remains in progress.

The reclaimer's queue is charged to the shared buffer pool and bounded by the
existing 32 KiB generation reservation. Nested destruction stays on its worker.
Interactive services defer disposal; standalone blocking Document calls retain
synchronous disposal. Shutdown waits after owner joins and terminal restoration.
Cancelled prepared opens are reclaimed before terminal completion and release of
their queued requests. A real-bridge test gates reclamation and confirms another
edit is acknowledged before the cleanup gate is released.

Focused Rust checks cover literal/metacharacter/multiline/CRLF and BOM semantics,
piece/read-window boundaries, randomized scalar-reference ranges, Unicode
folding/word boundaries, grapheme-bisecting exact replacement, original-field
paste ownership, invalid field preservation, all-open wrap and nonwrapping
automatic continuation, partial failure counts/undo, read-only capabilities,
stale revision rejection, latest view mapping, quit cancellation, admission
failure, and coalesced dispatch scope. The maximum folded query initially exposed
excessive lookahead retries; prefix filtering plus anchored literal verification
reduced the complete six-test matcher suite to under one second locally.

The last complete local regression run passed formatting, Clippy with `-D warnings`,
and 213 enabled tests (seven explicitly ignored benchmarks/environment checks).
Later focused additions also pass; the complete final run remains pending.

### Final 1 GiB terminal measurements

macOS 27.2 arm64, Rust 1.99.1, MacBookPro18,2 / 32 GiB RAM, local APFS,
warm allocated fixture, release build, default clone attempt. One `SEEK_TOKEN`
per MiB is counted and replaced; ordinary input is measured during a dense-match
scan, then cancellation, save-byte checks and one-step undo are exercised.
These runs include the final reclamation implementation. Later changes preserve
ReplaceNext reporting and add a post-publication test observer; neither changes
the measured ReplaceAll path when no observer is configured.

| Corpus | First text ms | Ready ms | Scan ms | Replace ms | Input p50 / p95 / max ms | Cancel ms | Kernel peak KiB |
| --- | ---: | ---: | ---: | ---: | --- | ---: | ---: |
| Ordinary, 1 GiB | 352.397 | 849.021 | 348.558 | 1663.408 | 15.178 / 17.779 / 17.801 | 63.693 | 142640 |
| Dense, 1 GiB | 56.889 | 3927.322 | 343.111 | 1628.298 | 15.129 / 17.672 / 19.518 | 63.189 | 146080 |
| Long line, 1 GiB | 57.092 | 336.163 | 354.014 | 1596.951 | 15.113 / 17.651 / 17.679 | 59.386 | 141952 |

Command: `EDE_BENCH_MIB=1024 EDE_BENCH_CORPUS=ordinary EDE_BENCH_PEAK_RSS=1
cargo test --release --offline --locked --test terminal
large_search_and_replace_benchmark -- --ignored --exact --nocapture`.
All three native runs passed count, saved-byte, Undo, successful-exit and terminal
restoration assertions. Cancellation was observed in each run; the reported time
is Escape dispatch to displayed settled cancellation, including the terminal
Escape disambiguation interval. The sampled RSS is not the peak; the table uses
the kernel lifetime peak. First text and input meet the local 1 s / 50 ms goals.
Dense indexing becomes editable later than its first preview, as in prior stages.
Linux constrained-memory execution is pending CI.


### Integrated implementation evidence

Application version: `0.0.1-dev.6`. Native environment: macOS 27.2 arm64,
Rust 1.99.1, MacBookPro18,2, 32 GiB RAM, local APFS. Branch
`stage/search_and_replace`; base `c0d8824` on `feat/initial_mvp`.

The matcher reserves the complete 8 MiB search allowance from the shared buffer
pool. It scans bounded windows and keeps at most 256 visible ranges. The worker
serializes scan, plan and view mapping. All-open membership and navigation
ownership are captured at admission. Exact scalar matches remain separate from
grapheme-safe ordinary selections.

Replacement forks storage/history privately, retains physical root identities
for in-flight saves, and publishes after identity/revision/generation/cancellation
guards. Separate private maintenance compacts obsolete storage during idle time;
publication additionally requires the captured save baseline and no blocking
save leases. It preserves logical revision, text, undo and current views. New
search/edit work cancels it. Failed maintenance waits for another acknowledged
activity before retrying. Complete retired bundles are disposed on the bounded,
charged reclaimer rather than the foreground coordinator or storage owner.

Acceptance evidence uses the normal Rust storage, coordinator and PTY entry points:

| Criteria | Named checks and evidence |
| --- | --- |
| S01–S02 | `tests/search.rs`: six matcher tests, including randomized canonical scalar-reference comparisons, tiny windows, CRLF, multiline, BOM/pieces, Unicode folds/word boundaries, maximum 4096-byte query and cancellation. |
| S03 | `literal_find_panel_uses_unsaved_text`, `literal_replace_all_confirmation_and_undo`; five `app::search::tests` cover paste origin, invalid/oversize preservation, reverse focus/F6, all panel controls keyboard/pointer targets at 40×8, three text rows, resize preservation and blocked tiny confirmation. `exact_combining_match_never_reuses_an_unrelated_ordinary_selection` exercises navigation/Cut/type/exact replacement. |
| S04 | `search_current_and_replace_all_use_unsaved_content_and_one_undo`, `coalesced_current_search_keeps_dispatch_target_after_switch`, and all-open navigation/failure/64-tab tests exercise captured scopes and inactive unsaved text. Existing blocked-open and independent-tab regressions also pass. |
| S05 | Atomic prepared-edit tests, no-op continuation, all-open forward/backward wrapping, nonwrapping automatic continuation and one-step Undo. Follow-up navigation retains acknowledged replacement counts and a separately labeled target outcome; explicit new search resets them. |
| S06 | Prepared-edit cancellation/stale guards and existing transaction fault tests preserve live text/history. `all_open_failure_counts_only_completed_documents_and_retains_per_document_undo` injects the second plan's failure. `cancellation_and_close_after_commit_count_once_and_preserve_newer_input` gates after commit and admits Cancel/navigation/edit/Close before acknowledgement: processed=1, changed=1, dirty Close guard and newer text survive. |
| S07 | Gated stale planning, latest view mapping, cancelled navigation, admission-time revocation, refresh versus scan/plan, and failure/retry tests. `retired_generation_cleanup_never_blocks_foreground_editing` uses the real bridge and holds cleanup while another edit is acknowledged. Gated maintenance similarly allows editing/new search. |
| S08 | Late-save baseline regression, prepared maintenance versus live save leases/new SaveAs destination; Quit waits for cancelled planning; Quit during maintenance stays available. `replaced_scratch_is_restored_after_normal_quit` checks actual PTY Quit/relaunch. Existing checkpoint, guarded-save, cancelled-Quit and terminal restoration regressions pass. |
| S09 | `read_only_retained_history_is_searchable_and_skipped_by_replacement` covers synthetic retained read-only history and later editable revision. Real stream ingestion remains Stage 08 scope. |
| S10 | All three 1 GiB release PTY runs above. `sixty_four_tabs_and_repeated_cancellation_stay_within_shared_budget` performs 64-document replacement, 20 cancellation/retry cycles and asserts measured buffer/cache high waters within 32/96 MiB. `repeated_replacements_reclaim_storage_and_preserve_undo` performs 40 batches with a 128 KiB/one-transaction history budget, asserts live text storage below 2 MiB, shared buffer high water within 32 MiB, and working Undo. |

Test-first evidence: the repeated-batch check initially failed with 2,686,976
retained text bytes, then passed with automatic private maintenance. The gated
ReplaceNext cancellation test initially reported zero processed after a commit;
it now retains one processed/changed document. The all-open acknowledgment test
initially identified the wrong tab and now identifies the exact replaced target.

Changed data guards were deliberately broken only in isolated source copies after
unmutated baselines passed. Each mutation compiled, ran and failed its intended
assertion:

- Revision guard: newer text was replaced by obsolete prepared text.
- Preparation cancellation token: text became `lost` instead of retained `keep`.
- Maintenance save-baseline guard: Save As destination reverted from `second` to `first`.
- Maintenance blocking-lease guard: publication incorrectly succeeded through a live lease.
- Completed-replacement state reset: gated Quit disappeared behind maintenance.

Independent Review agents read `AGENTS.md` and `REVIEW.md`, traced the working diff
against `c0d8824`, and reproduced/followed up the material fixes. Review was read-only;
experiments used isolated copies. They checked mutation evidence and independently
ran focused matcher, prepared-edit, coordinator and terminal checks. Final review
of automatic continuation reporting found no remaining findings; the reviewer
independently passed 25 coordinator checks, including its two added regressions.

Final native checks passed: `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets --offline --locked -- -D warnings`,
`cargo test --workspace --offline --locked`: **227 enabled tests passed, zero
failures; seven opt-in environment/benchmark checks ignored**. All three new large
search benchmarks were also run explicitly as recorded above. `git diff --check`
and all 18 local Markdown link targets passed. The CI workflow adds all three
1 GiB search/replace corpora inside the existing Linux 256 MiB, swap-disabled
systemd harness. Native benchmark fixtures are warm; no physical terminal,
remote transport or cold-cache result is implied by PTY evidence. Those environment
limitations carry forward from the earlier terminal stages.
