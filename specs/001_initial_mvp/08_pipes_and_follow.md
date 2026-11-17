# 08 — Pipes, pager behavior, and live following

Status — Implemented, independently reviewed, and validated on macOS and Ubuntu.

Branch — `stage/pipes_and_follow`, based on `feat/initial_mvp` at `bc25929`.

PR target — `feat/initial_mvp`.

## Scope and decision status

Deliver [FR-07](overview.md#fr-07--piped-input-pager-use-and-live-following) and
integrate live text with FR-04 highlighting and FR-08 search. Existing
requirements include incremental display, read-only live streams, editable piped
text after successful EOF, explicit follow controls, retained-history search,
and direct file following on macOS and Linux. The CLI, file rotation policy,
retention budgets, and error controls below are the Stage 08 behavior. The broader
remote-terminal matrix remains Stage 10 work.

Preserve [Stage 02](02_document_storage.md) editing and save guarantees,
[Stage 05](05_scratch_recovery.md) scratch recovery and named-file refresh,
[Stage 06](06_search_and_replace.md) search semantics, and
[Stage 07](07_syntax_highlighting.md) highlighting limits. Active streams and
failed captures have session-local storage. Persistent scratch recovery starts
when stdin becomes an ordinary editable scratch after valid EOF. Pre-EOF
editing/snapshots and stdout pass-through are outside Stage 08.

## Entry points and terminal ownership

| Invocation | Result |
| --- | --- |
| `ede [--] [FILE ...]` with terminal stdin | Existing editor startup. |
| `producer \| ede [--] [FILE ...]`, or `ede [FILE ...] < INPUT` for a regular file | Open a **Stdin** tab, initially active and following, ahead of any named arguments. Count Stdin as one of the 64 startup slots, followed by named files in argument order and recovered scratches in remaining slots. Use Stage 05 Recovery for overflow. |
| `ede --follow FILE` | With terminal stdin, open one active, read-only file-follow tab and use remaining slots for scratch recovery. Read from byte zero and then follow new content by pathname. |
| Actions → Follow File | Open the existing keyboard/mouse picker in follow mode, including direct path entry. Start a new follower with the same rules as the CLI. A normal Open of the same path remains a separate editable document subject to ordinary refresh and safe-save policy. |

Support pipe/FIFO and regular-file stdin through their already-open descriptors.
Reject other nonterminal stdin types, including a socket or character device
such as `/dev/null`, with a plain diagnostic. A pathname passed to Follow File
must resolve to a readable regular file. Use native path arguments and
Stage 02 display-safe formatting. `--follow` takes exactly one path argument
(including a dash-prefixed argument) and cannot be combined with other paths or
nonterminal stdin. A literal file named `--follow` uses the ordinary `--`
separator.

Keep `--help`, `--version`, and the private syntax-worker dispatch ahead of
interactive setup. Return 2 for invalid CLI syntax and startup tab-count
overflow. Missing/invalid terminal endpoints or TERM, unsupported stdin types,
and failures before an interactive session can start return 1. A follow path
that fails to open after interactive startup reports the error in the editor,
releases its tab slot, and falls back to the usual startup selection. Document
both invocation modes and their restrictions in CLI Help and README when they
ship.

Interactive runs require tty stdout and usable TERM. Pipe or regular-file stdin
also requires a usable foreground controlling terminal. Open `/dev/tty` for
interactive input and output in that mode. Validate these requirements before
consuming any input, starting recovery claims, or changing modes. If stdout
is redirected or the controlling terminal is unavailable for a stream, fail
with a plain stderr diagnostic and leave stdout empty. Redirected informational
flags retain their existing plain-text stdout behavior.

One terminal endpoint must supply keyboard, mouse, paste, capability replies,
geometry, job-control checks, mode changes, and screen output in stream mode.
Keep the existing sole terminal decoder in `ingress`; give it the tty handle
and keep stdin bytes out of its parser. Use the explicit tty for termios and
geometry in this mode. Preserve the ordinary terminal path and the behavior of
existing terminal-only library entry points. The CLI hands inherited stdin to
the source reader only after validation, with no inheritable extra descriptor
or lingering fd 0 pipe reader. In particular, a spawned syntax helper or a
following file's worker cannot keep a closed Stdin pipe alive.

## Document states and commands

| State | Observable behavior |
| --- | --- |
| Receiving stdin | Incrementally show valid retained text. A delayed or idle producer keeps the tab live. Follow and Pause View are independent of reads. |
| Following a file | Show the supplied path, retained bytes, viewport follow state, and reading/waiting/rotation status in Tab Details. Temporary EOF waits for more text. |
| Finishing stdin | Source has closed. Commit and finalize the last validated extent and its format. Keep the tab read-only until promotion succeeds. |
| Editable scratch | Successful stdin EOF turns the same tab into an unnamed Stage 05 scratch, including empty input. Start with empty undo/redo history and preserve valid caret, selection, viewport, clipboard, and manual syntax override. Revoke stream jobs; assign a fresh revision or document identity. Save opens ordinary Save As. Normal Quit checkpoints it under Stage 05 rules. |
| Stopped or failed | Keep the last committed valid prefix visible and read-only. Show why capture ended and that it may be incomplete. A promotion failure after clean EOF is separately labeled and offers **Retry Finish** against the same complete backing text. |

While read-only, allow normal mouse/keyboard selection, internal copy, tab and
navigation commands, and Find. Reject insert/delete, Cut, paste, Undo/Redo,
Save/Save As, and replacement at the document/service boundary as well as in
the UI. Rich-paste or replacement work queued while read-only has no authority
to mutate a subsequently promoted scratch. Direct followers stay read-only.
Use explicit kinds/capabilities for stream tabs, scratch eligibility, and
replacement; a missing or present pathname alone does not imply any of them.

Add named **Pause View** / **Resume Follow**, **Follow File**, **Retry Finish**,
and **Export Retained Copy** actions where applicable. Put them in Actions
(F2 or mouse), Help as relevant, and a visible, keyboard-focusable follow toggle
where it fits. Use the same command routes for keyboard and mouse and the
existing small-terminal and modal precedence rules. No new dedicated default
key is required.

- Following aligns the viewport to the newest text using normal display cells,
  including the end of a long unterminated line. Recompute on append and resize
  without activating inactive tabs. Each tab retains its own follow preference.
  The tail caret stays at the end during automatic tracking. Existing line
  navigation, horizontal scrolling, pagination, and Go To work against
  the published retained extent.
- Explicit pause freezes the view where it is. Moving away from the tail with
  caret navigation, scrolling away vertically or horizontally, starting an
  ordinary selection or click/drag, or revealing a Find result pauses view
  following. Appends while paused preserve the viewed location and selection
  except for required grapheme or CRLF boundary repairs. Show whether new text
  arrived. `Ctrl+End` visits the end once. Resume Follow clears selection, moves
  to the current tail, and tracks further appends.
- Escape follows the existing modal, search and paste cancellation precedence.
  It never stops a source implicitly. Ctrl+C copies, with the existing no-selection
  behavior. Only explicit Close/confirmed Quit, terminal teardown, source
  completion or the errors and limits below stop stdin reading.
- **Export Retained Copy** applies only to stopped/failed captures and an
  EOF promotion failure. Confirm that an incomplete capture exports only its
  committed prefix. Stream the captured exact serialized bytes to a user-chosen
  **new** path using Stage 02 create-only guarded publication; preserve the tab
  and its source kind, including on export failure or cancellation. Refuse an
  existing destination and a direct follower's source path even when the path is
  temporarily missing. Export never assigns a saved baseline to a stream.
  Apply Stage 02's save commit point and outcome rules to cancellation, Close
  and Quit. Before publication, cancellation leaves the destination untouched
  by the export. Once publication commits, report success or an uncertain
  publication outcome and reconcile it before closing the tab.

## Ingestion, text, and retention

- Use one bounded source reader per admitted stream and serial publication per
  tab. Do source I/O, text validation, indexing and spooling off the event loop.
  Read and stage at most one 64 KiB block per source plus small decoding carry
  (at most four bytes for the initial BOM decision, three for an incomplete
  UTF-8 scalar and one for an ambiguous trailing CR). Admit up to four live
  sources in a session, including stdin. At capacity report the reason before
  opening an extra follower. Stopped and promoted tabs no longer require a
  reader slot once their work releases its resources, and still count toward
  the tab limit. Give the pipe descriptor to a dedicated cancellable read
  owner that performs no spool or index I/O. Use a bounded handoff with one
  block awaiting commit and a cancellation wakeup independent of space or
  downstream progress. The read owner closes its descriptor even while a
  spool or index write is stuck. Charge any canceled work still in flight to
  its original resource budgets until it actually releases them.
- Use Stage 02 text rules with streaming boundaries. Recognize a split leading
  UTF-8 BOM as format metadata and reject UTF-16/32 BOMs as unsupported encoding.
  Preserve valid UTF-8, LF, CRLF, mixed endings, standalone CR and absent final
  newline. Publish complete text as it arrives, including an unterminated
  line. Retain an ambiguous trailing CR until the next byte or end of source.
  After BOM classification, stop at the first invalid scalar, incomplete UTF-8
  at true EOF, or NUL in source order. Commit the longest preceding valid
  prefix that fits the quotas, even when the invalid byte is in the same read.
  Report the offending source byte offset and reason. No substitutions, skipped
  bytes, or silent promotion of a partial capture. Render all controls with the
  existing safe document-text renderer. Save As after valid stdin EOF preserves
  the exact bytes.
- Use a private, append-only spool with paged ranges and line indexing. Publish
  a new immutable logical extent after the bytes and required metadata have
  committed. Readers use a bounded lease, pinned logical length and index
  revision. Extending the backing file cannot alter a published extent.
  A failed write cannot expose a partial block as committed or affect an older
  snapshot. Coalesce notification to the latest extent per tab, keeping ordered
  state transitions and a terminal reason available even when the queue is full.
  Throttle routine redraws through the existing runtime frame scheduler.
- Stage 08 defaults are 2 GiB of retained payload per stream and 8 GiB total
  retained stream payload per process, including stopped tabs and storage pinned
  by stream leases. Count the serialized BOM and direct-follow separator bytes.
  No eviction. Once adopted by an editable scratch, account its backing once
  under Stage 02/05 limits. Use the shared Stage 02 cache/buffer allowances and
  512 MiB free-space reserve. Account separately for on-disk indexing, prepared
  commits and export staging. Keep index disk growth proportional to committed
  bytes; reserve for the worst case before appending. Atomically charge
  pending and committed payload against the shared quota so concurrent
  appends cannot both spend the same capacity. Wait for a competing temporary
  reservation to settle before reporting exhaustion. Initial limits are
  internal defaults with test injection.
- If storage falls behind, stop reading further blocks until the pending block
  commits. An upstream pipe can block through normal kernel backpressure. When
  reserve, quota, or a write failure prevents further progress, stop the source
  with visible incomplete-capture status, committed byte count and reason.
  Store the longest valid prefix within the available quota, including its BOM
  when present, without splitting a scalar or CRLF. Count a direct-follow
  separator together with its first following text unit. Use only bounded
  decoding lookahead to resolve a unit that straddles the cap. At the exact
  cap, wait for EOF or proof of additional input (one byte suffices when
  there is no decoding carry). Report overflow once proven, with no committed
  spill. Complete short successful writes before commit. If byte or index
  preparation ultimately fails, the previous commit stays authoritative for
  that failed transaction. A stream at limit still supports navigation,
  selection and search. Closing and releasing its last lease reclaims the spool.
- Use kernel read outcomes to distinguish idle, usable bytes, EOF and input
  errors. Drain readable bytes before treating a pipe hangup as EOF. Retry
  interrupted reads while the source still owns the operation. Pipe EOF reports
  closure of the data channel; a producer's exit code is available to the
  shell, so zero-byte read cannot classify an upstream program failure. Use
  existing Rust/Unix dependencies for the initial design.
  [Linux pipe(7)](https://man7.org/linux/man-pages/man7/pipe.7.html) ·
  [Apple read(2)](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/read.2.html)

## Direct file following

- Open read-only with the Stage 02 descriptor type and identity checks,
  including the nonblocking open/type check for a replacement race involving a
  FIFO. Use bounded positional reads on a worker and keep a separate source
  offset from the retained document offset. Copy validated data into the
  private spool before publishing. Never use a mutable source file as backing
  for text already shown, selected or searched.
- Follow the requested pathname, including symlink resolution. Check descriptor
  length and the current path on a 250 ms schedule off the UI thread, including
  between bounded reads during a backlog. An overdue check runs before
  starting the next read. Show temporary path disappearance or failure to open
  a replacement in Tab Details. Retry at this cadence, keeping the old descriptor
  and accepting its available appends until a replacement is ready. A real
  read error ends the capture with the last committed prefix. A path that is
  missing or invalid at initial open produces an ordinary open error.
- On a replacement with different device/inode, validate the new regular-file
  descriptor and confirm it still matches the path. Capture the old
  descriptor's current length once and drain only through that cutoff before
  switching to byte zero of that opened replacement. A shrink or rewrite
  detected during the final drain discards the uncommitted old block and ends
  the old drain at the last committed extent with a gap warning. Detection
  during the drain advances to the replacement without restarting the old
  inode, subject to text validation at the generation boundary.
  Bytes written to the old generation after the cutoff and intermediate
  generations never observed by polling may be missed.
- For the same inode, restart at byte zero on an observed shrink, or on a
  detected rewrite at or before the source cursor. Recheck a bounded 4 KiB
  witness immediately before the committed cursor around reads and at scheduled
  idle checks when size or modification time changes. If it changes or the
  file shrinks, discard the in-flight source block and start a new generation.
  Changes outside the witness, rapid truncate/regrow preserving the
  sampled bytes, and writes that race observation cannot be detected reliably.
  Direct following targets append-style logs with best-effort rotation detection.
- Retain captured generations in order. Before publishing the first text of a
  new generation, add a line separator if the retained document is nonempty
  and does not already end in LF. Insert LF normally or CRLF after standalone
  CR, so existing CR stays standalone. Each generation boundary is EOF for
  its decoder. A partial UTF-8 scalar stops the follower with a text error and
  preserves the valid prefix. Following the replacement then requires a new
  Follow File action. On a valid boundary, flush a pending standalone CR and
  reset decoding state. Strip the leading UTF-8 BOM in subsequent generations
  as format metadata, preserving a leading BOM from the initial generation
  only. The inserted separator participates in search, selection and export.
  The latest boundary and cumulative generation count
  appear in Tab Details. A partial line can never join directly to the next
  file's text. Do not keep an unbounded in-memory generation log.
- Confirm a prepared block still belongs to the current tab/source generation
  and expected position at commit. Superseded opens, old reads and late
  completions cannot reopen or alter a closed tab. A followed file is never a
  write target of the follower.

## Search, syntax, and publication ordering

- Use the existing Find modes and matching semantics. Each target is one
  immutable retained extent, with scope shown as **Retained history** and its
  captured byte count. To let explicit Find/Next/Previous finish during steady
  ingestion, hold publication of newer extents for the stream being scanned
  until its search result is handled. Show that the search is using a fixed
  extent and ingestion continues. Keep accepting validated bytes into the
  same spool, coalescing later extents and enforcing the same disk limits.
  At most one stream has a search hold at a time, including in All Open.
  Release the hold when the target finishes, the request is cancelled or
  superseded, or the tab closes. Accepted navigation pauses follow and reveals
  the match before a newer extent takes over. A later publication invalidates
  old counts/decorations as in Stage 06, never claiming an exact live count.
- Retain the last accepted navigation anchor across append-only publications
  independently of its decorations. On a new Next/Previous, check the prior
  exact range in the newly captured canonical match sequence. If it still
  matches, advance to its successor/predecessor. If it ceased to be a match
  (for example, a whole word extended past the prior EOF), start at the old
  start for Next or at/before it for Previous, using the usual wrap rules.
  Clear the anchor on the existing user-driven navigation or query changes,
  stream-to-scratch promotion, or any publication that changes prior bytes.
- Disable replacement for Current on a read-only tab, while keeping Find
  available. All Open replacement explicitly skips/counts streams and preserves
  Stage 06 per-document transactions for editable tabs. Capture the skip
  decision at replacement dispatch, so an EOF conversion never adds a target
  mid-run. After promotion a new request can replace text normally.
- Stdin starts in Auto (Plain Text); file-follow Auto uses the supplied path's
  Stage 07 associations. Permit manual language overrides. Honor the existing
  heuristics on the entire published extent and reuse the one-job/latest-request
  policy. Appends invalidate stale syntax. Coalesce or defer parsing while
  updates are sustained, showing plain text with a pending state; schedule the
  latest extent once the stream settles. Keep follow, search and editing of
  other tabs responsive.
- Tag source messages with tab, source generation and monotonically increasing
  extent/revision. Match view/navigation authority using existing view and
  request identities. Require exact final validated bytes, EOF and promotion
  success before installing edit capability and recovery identity together.
  Adopt the existing spool into paged editable storage without a whole-text
  RAM allocation or mandatory full-text copy. On adoption failure keep the
  complete source and allow Retry Finish; the producer descriptor is already
  closed. Revoke search holds on promotion and reject stale stream jobs.
  No failed or cancelled operation can publish partial text as editable.

## Close, Quit, suspension, and terminal loss

- Confirm Close of a live source or a nonempty read-only capture, identifying
  the source and retained history being discarded. Quit includes a single
  confirmation for affected streams in addition to existing unsaved named-file
  decisions, with Cancel focused. Keep sources running through prompts and
  other fallible quit work. Cancelled Close/Quit leaves sources, follow
  preferences, and retained work available. Serialize EOF promotion and the
  final Close/Quit commit. Stream discard authority covers only the confirmed
  tab's read-only source identity. If Close loses to promotion, ask under
  Stage 05's scratch Close policy. For Quit, recheck the scratch set and
  acknowledged revisions at final commit. If a promotion or change occurred
  after capturing checkpoint targets, extend the checkpoint sweep and await
  the new acknowledgements before committing Quit. Once Close/Quit commits,
  revoke any unpublished promotion before it can create a scratch.
- On confirmed teardown revoke source authority, cancel queued work, release
  pipe readers promptly, and restore terminal state before any potentially
  blocking worker join. Closing a producer's last read end can cause
  SIGPIPE/EPIPE on a future producer write. ede neither kills nor waits for
  the producer. Use cancellable readiness waits so an idle pipe cannot hang
  termination. Ensure an uninterruptible filesystem operation cannot delay tty
  restoration or keep the CLI alive indefinitely; abandon unpublished session
  work if necessary on final process teardown, with owner-scoped spool cleanup.
- Suspend uses Stage 01 restore/stop/reacquire on the tty, with foreground
  checks against that endpoint. Ingestion stops while the process is suspended
  and resumes from its previous source state after re-entry. A producer may
  block on a full pipe during suspension. Tty EOF/irrecoverable I/O loss or
  SIGHUP stops sources and attempts existing terminal cleanup. Use the existing
  runtime error/signal exit statuses and last-complete scratch recovery, with no
  assumption that an unavailable tty can be restored successfully.

## Acceptance criteria

Use controlled publication barriers for concurrency checks. Use real child
processes, pipes, PTYs, and files for kernel/terminal behavior and bounded
timeouts for CI.

| ID | Observable evidence required |
| --- | --- |
| P01 | Verify CLI classification, help/version, both startup modes, 64-slot accounting and scratch overflow. With no controlling tty, unusable TERM, or redirected stdout, verify plain failure, empty stdout, unchanged termios, and unread stdin. With a real controlling PTY and separate pipe, verify keys/mouse/paste/resize and capability replies use only the tty, with input escape bytes rendered safely as document text. |
| P02 | Gate a real producer between chunks and prove first text appears before EOF. Split a BOM, UTF-8 scalar and CRLF at each byte boundary; include an unterminated line, a long line, a trailing CR, and empty stdin. On valid EOF verify same-tab promotion, exact Save As bytes, fresh Undo/Redo, editable search/replace and scratch retention on normal Quit. Force EOF adoption failure and retry without losing bytes. |
| P03 | Before EOF and on direct/failed streams, try every mutating entry point including paste, Cut, save and replacement. Verify mixed-scope replacement skip counts. Gate EOF promotion before and after Close wins authority, and during Quit after the final checkpoint sweep captures its targets. Verify queued mutation rejection and relaunch with the newly promoted scratch when promotion wins Quit. Demonstrate deliberate breaks in the capability and promotion/close/quit guards fail the intended assertions. |
| P04 | Exercise automatic and manual view pause, keyboard/mouse resume, horizontal tail, selection/copy, resize and inactive-tab updates during ongoing ingestion. Search across block boundaries and at whole-word/CRLF edges while appends are held and resumed. Verify Next/Previous advances after every append and revalidates a match that relied on EOF as a word boundary. Cancel search to release a hold and verify stale counts/highlights on later publication. Verify syntax override and fallback with real stream revisions, including EOF. |
| P05 | Force slow and short writes, ENOSPC and source read errors. Exercise invalid/incomplete UTF-8, recognized unsupported BOMs, NUL, quota overflow across scalar/CRLF boundaries and exact-quota EOF. Vary read partitioning and race concurrent quota reservations. Verify bounded reads, reachable UI, exact retained prefix and explicit failure. Verify export byte equality, destination race protection and cancellation immediately before and after publication. Preserve the retained tab. |
| P06 | Follow a real append-style file from byte zero. Exercise idle EOF, appends, rename/recreate while the old inode remains busy, late writes to the old descriptor, temporary missing/unreadable replacement, symlink retarget, observed truncation, a same-length rewrite at the witness while idle, and invalid path types. Force ordering around cutoff, generation switch and close. A partial UTF-8 scalar at the seam must stop before the replacement. On a valid seam, check retained/exported bytes and search endpoints around a trailing standalone CR. Verify visible status and untouched source files. |
| P07 | Quit or close on an idle and a busy pipe, cancel a quit, suspend/resume and simulate tty loss. Gate a spool write during Close and verify the producer sees SIGPIPE/EPIPE and a companion tab works before releasing the write. Verify exact terminal cleanup on recoverable paths, absence of inherited pipe readers, and owner-scoped spool/lease cleanup. Ordinary editor and scratch/file-refresh regressions still pass. |
| P08 | On macOS and Linux run locked formatting, Clippy and tests. Benchmark release builds with a sustained 1 GiB stream, dense short lines and a long single line, including Find and a companion editable tab. Record first usable text, event-to-visible ordinary input latency, throughput, RSS, descriptor/queue/cache high water, spool/index disk use, and EOF adoption time. Exercise the agreed 1 s / 50 ms goals on an 8 GiB or documented equivalent baseline, plus a swapless constrained-memory run. Record hardware, filesystem, cache state and producer pacing. |

## Validation record

- Independent specification review against `bc25929` identified and verified
  fixes for rotation under backlog, pipe closure during blocked storage I/O,
  repeated Find during appends, retained-prefix boundaries, standalone CR
  across rotation, and promotion during Close/Quit. Follow-up review confirmed
  export commit reporting, idle rewrite detection and incomplete UTF-8
  handling at rotation. No actionable findings remained.
- Local documentation checks verified whitespace, links, and acceptance IDs.
  The Stage 07 baseline passed `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets --locked -- -D warnings`, and
  `cargo test --workspace --locked` on arm64 macOS 27.2 with Rust 1.99.1.
  This baseline run used the default test selection.

### Implementation checks on 2026-11-15

- P01–P02 — CLI and PTY tests cover informational flags, argument errors,
  startup slot accounting for stdin, absent controlling tty, redirected
  stdout, untouched input and terminal state on startup failure, inherited
  regular-file position, escaped control bytes, display before EOF, split
  UTF-8 BOM/scalar/CRLF, exact Save As, and retry of an injected EOF adoption
  failure. Stage 05 startup and recovery regressions passed in the same suites.
- P03–P04 — A synchronized EOF test verifies that a stream Close confirmation
  cannot discard a promoted scratch and that a replacement admitted while it
  was read-only still skips it. A quit-readiness test covers cancel after the
  final acknowledgement is queued. Real PTYs cover pause state, Find advance
  after append, stale result reporting, and removal of the follow control
  after promotion. Existing search and syntax suites ran on both hosts.
- P05–P06 — Tests cover exact-limit EOF and extra-byte overflow, byte-exact
  retained export with no clobber, a symlink retarget at publication, a dangling
  source link, and ordinary pathname rotation with a BOM and standalone CR
  at the generation seam. Older snapshots remain readable through EOF finish
  retry. The same guarded publication routine serves Save As and export.
- P07 — Tests cover idle-pipe close and cancel, EPIPE after confirmed Close
  and SIGHUP, exact recoverable termios, and a gated spool write where the pipe
  reader closes before storage unblocks. The stopped writer retains its
  admission slot until the in-flight operation releases it. Retained capture
  files are unlinked while their descriptors are open. An abandoned, blocked
  worker can leave an empty private work directory.
- Independent implementation review against `bb24330` and follow-up review of
  the final behavioral fixes found no remaining correctness blockers. In
  separate copies, deliberate mutations compiled and failed the intended
  assertions for the contiguous-root integrity guard, export destination
  recheck, exact-quota overflow, reader cancellation during blocked storage,
  and retained scratch protections against stale Close and Replace authority.

### Local and SSH environments

- macOS 27.2 (26B123), Apple M1 Max, arm64, 32 GiB RAM, APFS, tmux 3.8.
- Ubuntu 26.10, aarch64, 9.2 GiB VM RAM, ext4, tmux 3.7b. From macOS tmux pane
  `%0`, the direct SSH shell displayed a gated pipe before EOF, accepted
  caret navigation that paused follow, promoted after EOF, suspended and
  resumed with `fg`, saved both lines byte for byte, and restored exact
  `stty -g` on suspension and Quit. An isolated Ubuntu tmux server attached
  from the same pane retained the original and replacement log generations
  and exited after confirmation.
- Rust 1.99.1. The Ubuntu runs also passed release highlighting and heap
  checks, 1 GiB storage/editing/search for ordinary, dense and long corpora,
  constrained highlighting/search overlap, scratch recovery, refresh storage,
  and the Linux 64 MiB paste peak-RSS probe. The interactive, search, recovery
  and paste probes used a 256 MiB limit with swap disabled. The storage
  probes ran at 8 GiB for all corpora and at 256 MiB for ordinary text.
- The final dev.8 tree passed `cargo fmt --all -- --check`, `cargo clippy
  --workspace --all-targets --locked -- -D warnings`, and `cargo test
  --workspace --locked` on both systems. The default suites passed 273 tests
  on macOS and 276 on Ubuntu. The opt-in isolated tmux editing and suspend
  smoke passed separately on both systems.

The checks sample the requested scenarios and injected boundaries. The full
interleaving matrix for simultaneous rotation, search, export and shutdown was
not enumerated, and an actual uninterruptible filesystem hang was not induced.
Native graphical shortcut delivery, mosh and Eternal Terminal remain part of
the broader terminal compatibility work.

### P08 release measurements

Each run transferred 1 GiB from a generated producer after a gated first
marker. It measured nine ordinary keystrokes in a companion editable tab during
transfer, searched the retained stream before closing the producer descriptor,
and timed EOF adoption. The producer ran at full speed after the gate. These
are warm, generated fixtures under a PTY. The Ubuntu fixture and spool used
ext4 rather than the VM's tmpfs. Ubuntu ran in a 256 MiB cgroup with swap disabled.

| Host | Corpus | First text (ms) | Transfer (MiB/s) | Max input (ms) | EOF adoption (ms) | Editor RSS sample peak (KiB) | Sampled FDs |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| macOS | Ordinary | 240.0 | 583.8 | 37.5 | 24.2 | 5,504 | — |
| macOS | Dense | 172.5 | 192.5 | 20.1 | 24.6 | 5,520 | — |
| macOS | Long line | 170.4 | 591.7 | 20.1 | 50.2 | 5,824 | — |
| Ubuntu | Ordinary | 26.0 | 390.8 | 17.8 | 22.0 | 19,368 | 25 |
| Ubuntu | Dense | 27.9 | 150.7 | 17.9 | 22.1 | 19,296 | 28 |
| Ubuntu | Long line | 31.5 | 209.6 | 17.9 | 20.8 | 19,300 | 28 |

The 1 s first-text and 50 ms input targets were met in these runs. The source
uses one outstanding 64 KiB read credit and published stream roots use one
physical extent over the paged LF index. The expected logical spool size was
the captured payload and the index required about 128 KiB at 1 GiB. Physical
spool allocation and managed-cache high water were not sampled separately.
The cgroup reported 256 MiB peak use with zero swap in all three Ubuntu runs,
including charged filesystem cache.

### Follow-up on asynchronous test ordering

- P02–P04 — A local PTY failure and a controlled reducer test exposed a
  dropped tab-list click when the active tab's document finished loading
  between mouse press and release. The reducer regression failed before
  the fix and passed afterward. A resize test now verifies it has actually
  captured the press before checking rejection of the stale release.
- P03/P07 — The first hosted macOS run failed in the two-tab Quit cancellation
  test. A tab's filename could satisfy the wait before its confirmation
  appeared. The PTY test now waits for the matching Quit dialog row before
  submitting either decision.
- P05 — One Ubuntu run exhausted the test service's metadata quota and
  attempted to allocate again before the asynchronous reclaimer returned
  capacity. The test still requires exhaustion with live leases and now
  waits for successful admission after releasing them.
- Independent review found no production blockers. Formatting, Clippy with
  warnings denied, the locked workspace suite, and the isolated tmux smoke
  passed again on macOS and Ubuntu 26.10. The full macOS PTY suite passed three
  additional runs. Each affected PTY passed 150 consecutive runs on macOS
  and 100 on Ubuntu through tmux pane `%0`.

### Follow-up on macOS saves, scratch claims, and PTY checks

- D09 — Eight local macOS PTY tests failed when a provenance-tagged editor
  saved files without that attribute. A tagged build reproduced the failure in
  tmux. macOS stamped the staged file before the copy. Save now accepts that
  additional attribute only when it already had the same value before the copy
  and the source lacks it. Every source attribute must still match. The PTY
  regression saves, reopens, saves again, cancels Quit, and verifies Undo against
  the saved text.
- R04 — Full Ubuntu repetitions exposed a missed scratch claim after an owner
  dropped its store. A duplicated owner descriptor made the same ordering fail
  deterministically. The owner now explicitly unlocks on drop, and successful
  liveness probes explicitly unlock before closing their descriptors. A claim
  still freezes its original liveness classification. The following opener
  must recover the scratch exactly once.
- Independent review verified the save and recovery changes. Deliberate changes
  that permit missing source metadata or remove the explicit owner unlock
  failed the intended assertions in isolated copies.
- H05 — One Ubuntu run failed while waiting for color on tab 13 of the 64-tab
  test. The test now matches the heading in the document, checks its exact
  color, and captures the reason from Tab details on failure. The original
  cause remains unconfirmed. The Ubuntu test passed 25 isolated repetitions,
  15 full PTY suite repetitions, and a one-CPU run with a competing process.
- Final verification passed three consecutive plain `cargo test` runs on
  macOS 27.2 (275 tests each) and five through tmux pane `%0` on Ubuntu 26.10
  (277 tests each). Both passed formatting, Clippy with warnings denied, the
  opt-in isolated tmux smoke, and 50 focused recovery repetitions. A macOS
  executable with native provenance passed the complete PTY suite three times
  and the save and reopen regression 25 times in tmux. The default suites
  skipped their 10 opt-in tests on each platform.
