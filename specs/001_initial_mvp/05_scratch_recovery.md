# 05 — Persistent scratch documents, recovery, and automatic file refresh

**Status:** Implemented and validated; [PR #5](https://github.com/rapidclock/ede/pull/5)
targets `feat/initial_mvp`. Design and implementation were authorized on 2026-11-14.

Parent: [MVP overview](overview.md). Depends on Stages 02–04, including
Stage 04 merged through [PR #4](https://github.com/rapidclock/ede/pull/4).
Branch: `stage/scratch_recovery`, based on `feat/initial_mvp` at `c1b8549`.
PR target: `feat/initial_mvp`.

## Outcome and decisions

Deliver persistent unnamed scratch tabs and automatic refresh of named editing
tabs. Scratch contents survive normal exit without naming or individually saving
them. After a crash, restore the last completed checkpoint. External saves refresh
clean named tabs automatically while preserving local edits.

This stage covers FR-10 and the automatic-refresh requirements of FR-02, using
the existing tab, storage, keyboard, and mouse behavior.

The user approved these decisions during specification discussion:

- Running instances own independent scratch sets. They cannot see or modify
  another live instance's scratches.
- A new instance restores scratches retained by exited or crashed instances.
  If several instances have exited, their retained scratches are combined.
- Recovery covers scratches only. General recovery of unsaved named-file edits
  is outside this stage.
- Restored scratches start with fresh undo/redo history.
- Explicit Close uses **Save / Discard / Cancel**. Normal Quit retains scratches
  without these prompts.
- Keep the 64-tab limit. Retain excess scratches and expose a recovery list.
- External refresh is undoable within existing history budgets. Keep the 256 MiB
  retained-text cap and report when it prevents undoing a refresh.

The user subsequently approved the complete design below and authorized
implementation. There are no unresolved prerequisite product decisions.
Search, highlighting, active streams, general configuration UI, and
cross-machine synchronization remain later work. Implementation refinements
must preserve these requirements or record an explicit agreed change here.

## Scratch lifecycle and startup

### Identity and presentation

- New creates an unnamed persistent scratch with a stable recovery identity,
  independent of its runtime tab and document IDs.
- Preserve editing, selection, clipboard, undo/redo, and Save As behavior.
  Scratch tabs have distinct labels and an identifiable scratch kind in Tabs
  and Tab details.
- Track named-file save state separately from checkpoint state. A checkpoint
  neither gives a document a filename nor makes Save overwrite recovery files.
- Show uncheckpointed changes, checkpoint progress, and persistence errors.
  Keep an error visible until resolved.
- Retain empty scratches. Explicitly closing one requires the same discard
  confirmation as closing a nonempty scratch.

### Instance ownership

An instance acquires a unique owner identity and holds an OS-backed exclusive
owner lock for its lifetime. Locks, rather than PID existence or timestamp age,
establish whether an owner is live.

At startup, atomically claim all recoverable scratches whose previous owners
are no longer live. Another starting instance must observe those claims before
claiming anything itself. Ownership includes unloaded recovery entries.

- While A and B run, each sees only its own scratches.
- After A exits, B continues with its existing set.
- If C starts while B runs, C restores A's retained scratches.
- If A and B both exit before C starts, C restores both sets.
- A crashed owner becomes recoverable when its process releases the lock.
- No process may clean up another live owner's files.

### Startup order and capacity

- Keep `ede [--] [FILE ...]`, native paths, duplicate-path handling, and the
  existing startup usage limit.
- Reserve slots for CLI files first, in argument order. Append restored
  scratches into the remaining slots.
- Order recovered owner groups by their most recent committed checkpoint,
  newest first, with owner identity as a deterministic tie-breaker. Preserve
  scratch order within each group.
- With CLI files, initially activate the first requested file. Otherwise restore
  the remembered active scratch from the first recovered group, or its first
  scratch when no remembered scratch is available.
- Preserve relative scratch order. Named tabs are not automatically reopened.
- Restore caret, selection, preferred column, and vertical/horizontal viewport
  against checkpointed text. Clamp invalid positions using existing Unicode and
  layout rules. If the previous active tab was named, use that owner's most
  recently active retained scratch.
- Create an empty scratch only when there are no requested files or retained
  scratches. If all startup work fails, provide a usable fallback and retain
  recovery errors.
- Failed startup opens release capacity for recovery. Late completions must not
  steal focus after the user switches tabs.
- Keep the 64-tab limit, including loading and cancelling tabs. Never delete
  retained scratches to satisfy it.

### Recovery list

Add **Recovery** to Actions, accessible through F2 and mouse controls.

- List this instance's retained but unloaded scratches, including failed restores.
- Provide **Open**, **Retry**, and **Discard**, with keyboard navigation,
  scrolling, and mouse activation at 40×8.
- Use bounded pages of at most 128 entries. Do not load the complete catalog
  into memory.
- Opening consumes an ordinary tab slot. When full, report the limit and leave
  the retained scratch untouched.
- Discard requires a scratch-specific confirmation with Cancel focused.
- Closing a tab does not automatically open an overflow scratch.
- Do not discover newly retired instances during ordinary operation. Their
  scratches belong to a later launch.

## Persistence and recovery storage

### Location and access

Use application-managed state on the machine running ede:

| Platform | Default root |
| --- | --- |
| Linux | `$XDG_STATE_HOME/ede` when absolute; otherwise `$HOME/.local/state/ede` |
| macOS | `$HOME/Library/Application Support/ede` |

Provide an absolute-path `EDE_STATE_DIR` override for isolated profiles and
executable tests. Reject a relative override. Do not fall back to temporary
storage when persistent state is unavailable.

- Create private application directories with mode 0700 and files with mode 0600.
- Reject unsafe state-root ownership or permissions with an actionable diagnostic.
- Use native filesystem paths and display them safely.
- Separate recovery objects from disposable working stores. A checkpoint cannot
  depend on temporary files surviving process exit.
- Help, version, and usage errors must not claim or modify recovery state.
- Unusable or unsupported catalog state blocks interactive startup with a clear
  error. Never silently replace it with an empty catalog.

### Publication model

Use immutable content objects and a versioned recovery catalog. The catalog is
authoritative for ownership, object references, order, active scratch, and views.

A checkpoint:

1. Captures an acknowledged immutable snapshot and matching view state.
2. Streams contents and format metadata into a new private recovery object.
3. Completes and synchronizes the object.
4. Acquires the shared catalog lock and rereads the latest catalog.
5. Applies only the submitting owner's current changes.
6. Writes and synchronizes a replacement catalog, atomically publishes it, and
   synchronizes its directory.
7. Acknowledges the precise published content and metadata generation.

Build content objects outside the catalog lock. Merge against the latest catalog
so instances cannot overwrite each other's checkpoints or ownership changes.

- Use versioned, length-delimited metadata with checked lengths and identifiers.
- Preserve exact logical text, BOM, and newline metadata without normalization.
- Validate object references and text before making restored content editable.
- Do not serialize runtime pointers, handles, or temporary storage internals.
- Before publication, interruption leaves the previous catalog and referenced
  objects usable. After publication, every reference targets a complete object.
- Treat failures around publication as potentially committed. Reconcile before
  retrying or deleting objects.
- Delete superseded objects only when no committed reference or active reader
  requires them. Remove abandoned staging objects only after their creator is
  no longer live and they are known to be unreferenced.
- Discarded or converted scratches cannot reappear through stale completions.

A malformed individual object must not prevent unrelated valid scratches from
restoring. Retain its entry, error, and recovery location. Leave an unreadable or
structurally invalid catalog untouched. Power-loss and storage-device failure
guarantees remain outside the approved recovery promise.

Implementation detail: keep a scalar-aligned UTF-8 error summary in the catalog's
28-byte diagnostic field and display the full object location. The original
failure diagnostic shows the complete reason. Successful Retry clears the saved
summary through the recovery worker; annotations cannot revive retired identities.

### Scheduling and resources

- Schedule checkpoints every **5 seconds while content or retained metadata is
  modified**. Continuous typing must not postpone the deadline indefinitely.
- Record new empty scratches and view-only changes without rewriting unchanged
  content.
- Run persistence off terminal and mutation paths. Use one checkpoint worker per
  instance, one running checkpoint, and coalesced pending work. Capture the latest
  acknowledged revision when work starts.
- Release snapshots promptly. An older completion never marks newer changes
  retained. Slow work retains progress and newer pending work; five seconds is a
  scheduling interval, not a maximum loss window.
- Use bounded streaming buffers and existing session-wide admission. Never
  assemble a complete scratch in memory.
- Initially store a complete streamed snapshot per changed scratch. Unchanged
  scratches reuse their content objects.
- Account for existing checkpoints, new objects, catalog staging, and working
  storage. Apply the existing 512 MiB free-space reserve independently on each
  affected filesystem.
- Do not impose a scratch-size cutoff or silently evict retained scratches.
  Report insufficient space and preserve the previous checkpoint.
- Preserve existing cache, queue, lease, and history budgets. Stage 09 exposes
  the interval in configuration; provide typed options for tests now.
- A checkpoint lease must yield or cancel when compaction needs exclusive
  ownership. Do not reject accepted edits because background persistence pins
  an old storage generation; retry the checkpoint against the resulting state.

## Save, close, quit, and failures

### Save As

- Save on a scratch invokes Save As. Preserve create-only publication, path
  reservations, exact saved-state acknowledgment, and existing failure handling.
- Only a confirmed successful Save As converts the scratch to a named tab and
  retires its recovery entry. Failure, cancellation, or uncertain publication
  retains recovery.
- Editing can continue during saving. Later edits remain dirty when an older
  revision is saved. After conversion they receive ordinary named-file protection
  and fall outside scratch-only crash recovery.
- Retirement revokes older checkpoint authority. Stale completions cannot
  resurrect the scratch.
- If retirement fails after the named save succeeds, report that a recovery copy
  remains. Preserve it until cleanup is confirmed; do not undo the named save.

### Explicit Close and Discard

Closing a scratch presents **Save / Discard / Cancel**, identifying the scratch
and focusing Cancel.

- **Save:** complete guarded Save As and close only after current contents are
  confirmed saved.
- **Discard:** revoke pending checkpoint work, reconcile running publication,
  and commit recovery-entry removal before acknowledging removal.
- **Cancel:** preserve the tab, contents, history, and focus.

If removal fails, retain the tab and report the failure. Unloaded recovery entries
follow the same confirmed-removal rule.

### Normal Quit

- Drain accepted edits, stop admitting new edits, and retain Stage 04's
  transactional named-tab decisions.
- Do not ask users to save or name scratches merely because they are quitting.
- Persist all final acknowledged scratch contents, order, active-scratch state,
  and positions before successful exit.
- Show progress. Cancel Quit restores the session with unsaved content intact.
  Named-tab Discard decisions remain deferred until quit can complete.
- Release ownership only after persistence is reconciled and no further recovery
  writes can occur. Restore terminal modes before final worker-shutdown waits.

Persistence failures offer **Retry**, **Save As**, and **Cancel Quit**. Never report
a successful retaining exit while current scratches remain unprotected. Saving
every affected scratch's current contents as named files provides an escape from
failed recovery storage. Explain when stale entries may reappear because their
removal could not be confirmed.

### Abnormal termination

- Signals, panic, and forced termination do not promise a final checkpoint.
  Preserve existing best-effort terminal cleanup and signal exit behavior.
- Relaunch restores the last completed checkpoint, never an interrupted object.
- Retry persistence failures automatically no more often than every five seconds;
  explicit Retry can run immediately.
- Keep repeated failures visible without flooding notifications.

## Automatic refresh of named tabs

### Detection and timing

Monitor every Ready named editing tab, including inactive tabs, without relying
on input or terminal focus.

- Probe supplied path resolution, target identity, size, and modification/change
  metadata every **1 second**, using background I/O.
- A changed observation becomes eligible after **250 ms of stability**.
- Support in-place writes, truncation, rename-based replacement, and changed
  symlink targets.
- Coalesce observations per tab. Allow one refresh preparation session-wide and
  one pending observation per tab.
- Prioritize explicit opens over queued refreshes. Blocked refresh I/O cannot
  block editing or rendering Ready tabs.
- Normally begin preparation within two seconds of observing a stable local
  change. Completion depends on size and I/O; show progress for long refreshes.
- Use stable private copying and complete-text validation. Never replace current
  text with a provisional preview.

This detects observable filesystem changes. It does not promise an atomic
snapshot of an uncooperative writer or detection when all observed metadata
differences are concealed.

### Installation and edit protection

Install only if the tab still exists and captured preconditions remain valid:

- Same document identity, revision, path, and acknowledged baseline.
- Document remains clean.
- No earlier accepted edit, paste, undo/redo, or save is unresolved.
- No queued or running save owns the tab.
- Prepared source identity/version remain acceptable under the open policy.

Recheck at publication, not only when preparation starts.

- Edits during preparation retain all local bytes and revoke installation.
- Close, Save As, newer observations, and newer requests revoke stale work.
- Preserve active tab and dialog focus. Background refresh cannot open a modal
  prompt or switch tabs.
- Publish text, format, clean baseline, revision, and history outcome together.
- Keep supplied path spelling and existing conflict/link/metadata safeguards.
- If validated text and format are unchanged, update the observed baseline
  without creating a text undo step.

### Undo and view state

- Represent changed refresh as one history operation, retaining earlier history
  within existing budgets. Refreshed contents become the clean file baseline.
- Undo restores previous contents as unsaved edits. Redo returns to the refreshed
  state, clean when that state still matches the baseline.
- Include BOM and newline-format changes in the operation.
- Keep the 256 MiB retained-text and 1,000-transaction limits. If the immediately
  preceding contents cannot fit, complete refresh with history reset to the new
  state. Clearly report that previous contents exceeded the undo limit.
- Preserve caret, selection endpoints, and viewport by logical line and grapheme
  positions, clamped to new text. Preserve horizontal offset and preferred column
  where valid. Map the latest acknowledged view at installation, not the view
  captured when preparation began. Navigation, scrolling, resizing, or switching
  away and back during preparation cannot be overwritten by stale view metadata.
  Avoid jumping to document start or end.
- Show brief tab-specific completion feedback; retain errors and conflicts in
  Tabs and Tab details.

### Modified tabs, invalid files, and retries

- A disk change on a dirty tab produces a persistent **Changed on disk** warning.
  Never replace text, clear dirty state, or advance its save baseline.
- Continue observing while dirty, deferring installation until clean. Existing
  guarded Save remains authoritative; Save As and close/reopen remain available.
- If undo restores the clean baseline, refresh against the latest observation.
- Missing, unreadable, unsupported, binary, invalid-encoding, or changing sources
  preserve the last usable document, history, and view. Show path and reason;
  never interpret open failure as an empty file.
- Retry preparation after 1, 2, 4, 8, 16, then 30 seconds, capped at 30 seconds.
  A new observed version restarts stability checks and backoff.
- Add **Retry refresh** to Actions and tab details. Retry clean tabs immediately;
  explain why local edits prevent refreshing a dirty tab.
- Successful refresh clears its warning. Unrelated probes cannot clear save
  uncertainty, which retains its existing reconciliation requirements.

## Interfaces and coordination

Keep public run entry points and CLI file syntax compatible. Add cohesive recovery
and refresh services with nonblocking submit/poll/cancel/shutdown boundaries.

| Interface | Responsibility |
| --- | --- |
| Recovery options | State root and interval; production defaults and isolated tests |
| Recovery identity | Stable scratch, owner, object, and catalog-generation identities |
| Recovery service | Startup claims, bounded listing/restoration, checkpoint publication, retirement, discard, final flush |
| Checkpoint acknowledgment | Exact content identity, view generation, publication outcome |
| Refresh candidate | Validated content and captured document/revision/path/baseline/source observation |
| Guarded refresh commit | Atomic eligible installation with history/baseline result |
| Tab status | Separate named dirty state, checkpoint state, conflict, progress, failures |

- Reuse paged text, immutable leases, resource accounting, cancellation, and bounded
  queues. Run copying, validation, serialization, and expensive preparation off
  terminal and document-owner publication paths. Installation cannot synchronously
  copy an entire document.
- Build refresh text/history and grapheme view mapping in a private generation;
  validate the source before the owner handoff and return exact prepared metadata.
  Per-tab admission epochs reject stale queued input without unrelated tabs
  starving refresh. Background snapshot leases pin old generations without
  blocking foreground compaction or holding shared cache locks through I/O.
- Match completions by stable identity and generation. Recreate runtime IDs after
  restoration. Keep accepted per-tab input order and prepared-frame protections.
- Route controls through named commands shared by keyboard and mouse, exposed
  in Help/Actions.
- Use Rust and existing dependencies unless a concrete additional need is
  demonstrated. Do not introduce a database or async runtime by default.

## Acceptance criteria and test plan

| ID | Observable acceptance check |
| --- | --- |
| R01 | Create multiple scratches, including empty and mixed Unicode. Quit without naming. Relaunch and verify bytes, format, order, active scratch, selection, caret, viewport, and fresh history. |
| R02 | With a controlled clock, verify five-second scheduling during typing, view-only persistence without text rewrites, slow-work coalescing, and exact captured-revision acknowledgment. Gate a checkpoint of the same scratch while edit churn requires compaction; verify accepted edits succeed and persistence resumes. |
| R03 | Complete checkpoint A, begin B, and terminate at controlled object-write/sync and catalog-write/publication boundaries. Recover the last completed publication without accepting partial B. |
| R04 | Run A and B independently. Verify isolation, recovery of A by C while B lives, combined recovery after exit, process-death lock release, and simultaneous claims without duplicates or loss. |
| R05 | Retain over 64 scratches across exited instances. Verify CLI precedence, bounded restoration/listing, opening after capacity frees, and retention of all overflow. |
| R06 | Cover every Save As/Close choice, conflict, cancellation, edits during save, uncertain publication, retirement failure, and stale checkpoint after discard/conversion. |
| R07 | Inject space, permission, write, sync, and catalog failures. Verify previous recovery, current text, visible errors, and quit retry/export/cancel. |
| F01 | Externally change active/inactive clean tabs through in-place and rename saves on macOS/Linux, including truncation and symlink-target changes. Refresh without input or focus changes. |
| F02 | Gate preparation, then edit, paste, undo/redo, save, Save As, close, or publish newer disk text. Verify only eligible installation and preservation of all local bytes. Also navigate, scroll, resize, and switch away/back before release; preserve the latest view at installation. |
| F03 | Check refresh undo/redo, baseline state, format-only changes, identical-content probes, earlier history, and explicit reset feedback above 256 MiB. |
| F04 | Remove a source, install invalid text/unsupported types, deny access, then restore valid text. Verify last-good contents, warnings, retry backoff/manual retry, and eventual refresh. |
| I01 | Exercise keyboard, mouse, paste, resize, modal focus, and cleanup through the executable at 40×8 and below, including cancelled quit, failures, and termination. |
| I02 | Checkpoint/restore a large scratch and refresh a 1 GiB file while another tab stays usable. Measure latency, RSS, shared-resource high waters, disk use, and completion times. |

Follow test-first development: reproduce focused failures before implementation.
Force overlapping orderings with explicit synchronization instead of timing-only
sleeps. Use subprocesses for ownership/process death, filesystem integration tests
for publication, and PTYs for executable behavior. Every executable test must use
an isolated temporary `EDE_STATE_DIR`, never the developer's recovery store.

After passing baselines, deliberately break refresh eligibility, checkpoint
revocation, ownership exclusion, and final-quit retention guards in isolated copies.
Confirm the intended content-preservation assertions fail. Run Stage 02–04
regressions, formatting, Clippy, and normal tests. Obtain independent review under
`REVIEW.md` with requirements, base/diff, tests, and mutation evidence; re-review
material fixes.

Compare measurements with the existing 1-second first-text and 50 ms ordinary-input
goals. Separate preparation time from visible input latency. Record hardware, OS,
filesystem, cache conditions, fixture shape, copy/clone behavior, disk costs, and
constrained-memory results. Investigate missed goals or obtain an explicit agreed
deferral.

## Validation record

**Implementation complete; local checks, Linux/macOS CI, resource probes,
mutation checks, and independent material-fix reviews passed.**

The [successful CI run](https://github.com/rapidclock/ede/actions/runs/43642498779)
validated implementation commit `b2373aa`. Subsequent documentation-only commits
record this evidence without changing the tested application or test code.

Validation was performed on 2026-11-14 with Rust 1.99.1,
`aarch64-apple-darwin`, macOS 27.2 (26B123), MacBookPro18,2,
Apple M1 Max, 32 GiB RAM, and APFS on the local data volume. Tests use private
temporary profiles. Generated fixtures were warm; PTY measurements exclude
graphical terminal rendering and remote transport.

### Behavior and regression evidence

Before implementation, executable tests reproduced Stage 04's scratch Quit prompt
and lack of unattended named-file refresh. Both now pass. The final normal command
is `cargo test --workspace --locked`; the final local run passed 114 unit,
4 CLI, 40 document integration, and 26 PTY tests. Two unit resource probes and
four opt-in terminal probes were excluded from that normal run. The aggregate
includes format-only/identical refresh, bounded backoff/manual retry, and
simultaneous startup claims. `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets --locked -- -D warnings`, and
`git diff --check` also passed.

| Normal CI suite | Unit | CLI | Document integration | PTY |
| --- | --- | --- | --- | --- |
| macOS 26 arm64 | 114 passed | 4 passed | 40 passed | 26 passed |
| Ubuntu 26.04 x86_64 | 115 passed | 4 passed | 41 passed | 27 passed |

The Linux-only XDG-without-HOME executable check passed. All large storage and
editing probes also passed on Linux, including Stage 05 recovery and refresh
with a 256 MiB cgroup limit and swap disabled.

| Criteria | Evidence |
| --- | --- |
| R01 | Scratch quit/relaunch PTY; exact selection, caret, preferred column, viewport, relative order, nonfirst remembered scratch across named CLI launch, empty scratches, Unicode, and fresh history in coordinator tests. |
| R02 | Controlled due/sweep state, metadata-only object reuse, gated snapshot A acknowledgment after edit B, slow-work coalescing, and edit completion while the checkpoint's write remains explicitly blocked. |
| R03 | Subprocess termination at Write, Sync, Publish, and DirectorySync boundaries recovers a complete old or new publication. Injected pre-publication failures preserve the prior catalog. |
| R04 | Independent live owners, recovery while another owner lives, combined retired sets, process-death release, frozen liveness classification during exit, and synchronized simultaneous claims with no duplicate/lost IDs. |
| R05 | Real executable retains 66 scratches across two owners, restores 64, and opens overflow after freeing capacity; 131-entry bounded forward/backward catalog paging; failed CLI opens release startup capacity. |
| R06 | Empty scratch Close guard; canceled removal before/after reply; unrelated error aborting Close; uncertain removal uses fresh authority; queued/running/uncertain stored discard rejects stale Open; failed Close Retry retains its operation; Save As retirement retry is throttled and nonmodal. |
| R07 | Write/sync/publication faults; final Quit waits for publication; persistence errors retain current text and survive later editing; inactive export targets its original tab; malformed object retains catalog entry, object path and bounded error, then repaired Retry clears that error. |
| F01 | Unattended refresh PTY; real missing/invalid-source recovery, symlink retarget, rename save, and dirty-content preservation; inactive-tab refresh during queued input in another tab; existing native guarded-save regressions. |
| F02 | Prepared root rejected after local edit/newer observation/newer source; foreground cancellation independent; private refresh Write gate does not block foreground mutation; queued input/paste reception revoke handoff; latest view maps grapheme ordinals; postcommit render failure retains authoritative revision/history and permits Undo/edit. |
| F03 | Undo/redo and new baseline, prior history, BOM-only changes, identical text without a new revision, configured byte-cap reset, repeated-refresh automatic arena reclamation. |
| F04 | Executable missing/invalid/symlink/rename recovery with last-good text intact; exact 1/2/4/8/16/30/30-second retry deadlines and manual bypass; storage permission/type/conflict regressions. |
| I01 | Full PTY regressions cover keyboard, mouse, paste, tiny geometry, modal focus, canceled quit, signals, suspend/resume and terminal restoration. Physical terminal key delivery retains the Stage 03 limitations. |
| I02 | Native and Linux 1 GiB refresh + 64 MiB scratch benchmarks and 1 GiB managed-storage probes passed; Linux used a 256 MiB cgroup limit with swap disabled. Measurements follow. |

The production interval is five seconds. Scheduling tests control the stored
deadline rather than waiting on wall time; synchronized worker gates establish
the overlap order. Worker cancellation may wait for a kernel call to return,
but immutable background generations let foreground edits/compaction proceed.
The opt-in isolated tmux editing/suspend smoke also passed locally with tmux 3.8.

### Large-document measurements

Commands:

```sh
cargo test --release --locked --test terminal large_recovery_and_refresh_benchmark -- --ignored --exact --nocapture
cargo test --release --locked --lib document::refresh::tests::large_refresh_storage_measurements -- --ignored --exact --nocapture
```

The executable used a fully written 1 GiB file with 64-byte lines and a 64 MiB
scratch received through bracketed paste. Its companion tab remained editable
during checkpointing and external refresh.

| Measurement | Native result |
| --- | --- |
| First displayed text / editable Ready | 202.268 ms / 670.635 ms |
| Companion input, 50 samples | p50 20.360 ms; p95 25.418 ms; maximum 32.938 ms |
| Refresh observed after external rename | 6,472.634 ms upper bound, including the deliberate five-second companion-input sampling period |
| Final Quit flush / scratch relaunch | 74.413 ms / 72.424 ms |
| Sampled editor RSS | 6,144 KiB, sampled with `ps`; not a kernel lifetime peak |
| Recovery object logical / allocated bytes | 67,108,875 / 67,112,960 |
| 64 MiB PTY paste reception and incorporation | 104,002.624 ms; includes terminal transport/backpressure and ingress, separate from ordinary-input latency |
| 1 GiB storage refresh preparation/publication | 2,990.512 ms; initial private snapshot used APFS clone |
| Shared managed cache / buffer high waters | 0 / 622,848 bytes; background preparation bypasses foreground caches |
| Final working text / metadata bytes | 2,147,483,648 / 295,048 |
| Sampled fixture + working logical / allocated disk bytes | 4,295,409,872 / 4,295,426,048 |

The old 1 GiB text exceeds the 256 MiB undo budget; refresh correctly reported
history reset. Working arenas may temporarily retain unreachable bytes until
their bounded compaction threshold. Repeated-refresh tests verify collection
without a later user edit. Directory sampling can miss short-lived disk peaks,
and APFS allocated blocks can share physical extents. These figures do not claim
a power-loss guarantee. Constrained-memory evidence follows separately.

The first-text and ordinary-input goals passed in this native run. Large paste
ingress is substantially slower than storage refresh: this run's 104 seconds
covers streaming 64 MiB through the PTY and receiving parser, not an ordinary
keystroke stall. No new paste-throughput acceptance threshold was approved here.

### Mutation and independent review

All mutations were applied individually in isolated copies; each compiled and
failed the intended behavioral assertion. The reviewed repository was unchanged.

| Guard deliberately broken | Behavioral failure |
| --- | --- |
| Invert owner-lock liveness | `independent_live_owners_and_combined_retired_recovery`: live B unexpectedly sees A's scratch. |
| Remove retired-entry checkpoint revocation | `discard_revokes_late_checkpoint_and_keeps_other_scratch`: stale checkpoint unexpectedly succeeds after discard. |
| Remove document/revision/store/dirty/baseline refresh eligibility | `local_edit_revokes_prepared_refresh_and_keeps_guarded_save`: expected StaleRevision is absent. |
| Bypass final-retention barrier in FinishQuit | `scratch_quit_and_relaunch_retains_text_without_save_prompts`: relaunch shows an empty scratch instead of the final acknowledged Unicode text. |

Ownership/revocation mutations ran in
`/private/tmp/ede-recovery-review.AFhuTj`; refresh eligibility in
`/private/tmp/ede-refresh-rereview.MBZWli`; final Quit in
`/private/tmp/ede-stage5-final-mutation.95A9wC`. These are ephemeral experiment
locations, not required runtime artifacts.

Independent reviewers followed `REVIEW.md` against `c1b8549`. The specification
review resolved checkpoint/compaction and latest-view requirements. Refresh review
reproduced and rechecked all six original findings, including blocked private I/O
and postcommit read failure, and reported no remaining actionable findings.
Recovery review reproduced cancellation/retirement and admission races; fixes,
bounded error annotations, and material-fix reproductions passed re-review with
no remaining findings. The rare post-Ready Info-failure diagnostic path was
inspected but not directly fault-injected.

### Final CI evidence and limits

The [first PR CI run](https://github.com/rapidclock/ede/actions/runs/43641321320)
passed formatting, Clippy, and all normal tests on macOS and Ubuntu 26.04 x86_64
(two vCPUs reporting AMD EPYC 9V74, 7.7 GiB host RAM, ext4). It also passed the existing
1 GiB ordinary/dense/long storage probes at 8 GiB, ordinary storage at 256 MiB,
and all three interactive corpora at 256 MiB, with swap disabled. The new I02
fixture writer was OOM-killed before starting the editor: it had accumulated
unsynchronized dirty file cache under that limit. Both new fixture generators
now synchronize every 8 MiB, matching the existing generator; the corrected
16 MiB/1 MiB end-to-end smoke and repeated full local checks passed. New Stage 05
resource checks now run before older benchmarks. The corrected
[CI run](https://github.com/rapidclock/ede/actions/runs/43642498779) passed every
step on both platforms, including both Stage 05 resource probes and all existing
1 GiB storage and editing probes.

| Linux at 256 MiB, swap disabled | Result |
| --- | --- |
| First displayed text / editable Ready | 42.846 ms / 14,835.216 ms |
| Companion input, 50 samples | p50 18.748 ms; p95 18.924 ms; maximum 47.919 ms |
| External refresh observed / storage-only refresh | 46,643.733 ms / 44,934.066 ms |
| Final Quit flush / scratch relaunch | 37.566 ms / 727.978 ms |
| 64 MiB paste reception and incorporation | 7,262.529 ms |
| Sampled / kernel lifetime peak editor RSS | 5,900 / 5,928 KiB |
| Shared managed cache / buffer high waters | 0 / 622,848 bytes |
| Final working text / metadata bytes | 2,147,483,648 / 295,048 |
| Sampled fixture + working logical / allocated bytes | 4,295,409,872 / 4,295,438,336 |

The portable copy path was used on ext4. The first-text and ordinary-input goals
passed. Under this memory limit, complete preparation takes longer while the
companion tab remains responsive. The cgroup's 256 MiB peak includes file cache
and the test harness; process RSS is reported separately. Recovery-object disk
usage matched the native run. These are measured environments, not a claim about
unmeasured hardware, cold-cache latency, or remote terminal transport.

- Local aggregate formatting, Clippy with warnings denied, and tests passed.
  ShellCheck 0.11.0 passed all multiline CI shell blocks, extracted and linted
  as POSIX shell through an isolated uv tool environment. The final fixture-only
  changes received an additional independent review with no findings.
- Linux native tests, 8 GiB baseline probes, and 256 MiB, swap-disabled 1 GiB
  recovery/refresh and older regression jobs passed in CI. Local Docker execution
  was unavailable; Linux evidence comes from the linked GitHub Actions run.
- GUI terminals, actual Cmd delivery, remote transports, and power-loss recovery
  are not established by these PTYs. Existing Stage 03 compatibility limitations
  remain explicit; no new deferral or relaxed recovery guarantee is approved.
