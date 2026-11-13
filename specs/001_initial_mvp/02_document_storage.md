# 02 — Document storage and file integrity

Status: **Complete — squash-merged into `feat/initial_mvp` through [PR #2](https://github.com/rapidclock/ede/pull/2).**

Parent: [MVP overview](overview.md). Depends on Stage 01, squash-merged into
`feat/initial_mvp` through [PR #1](https://github.com/rapidclock/ede/pull/1).
Branch: `stage/document_storage`. PR target: `feat/initial_mvp`.
This branch includes the accepted Stage 01 baseline. The user authorized Stage 02 implementation on 2026-11-02. The design defaults
below are the implementation baseline; refinements and actual evidence are
recorded in the final validation section. Later stage designs remain outside scope.

## Outcome and boundary

Deliver a document core that can open, read, edit, undo/redo, and save ordinary or
large text files with bounded memory. Verify it independently of the TUI, including
a 1 GiB file and files larger than the configured caches. Initial content must be
available before the entire file has been copied, validated, or indexed.

Stage 02 implements storage services and their Rust tests/benchmarks. Stage 01's
CLI and empty shell remain unchanged: file arguments, cursor/selection behavior,
editable rendering, clipboard, and save dialogs arrive in Stage 03; tabs/picker in
Stage 04. Temporary working storage is not scratch recovery, which is Stage 05.
Highlighting, ripgrep integration, and active streams remain their later stages.

The 1-second first-content and 50 ms ordinary-response goals remain engineering
targets. Here measure first content **available to render**, cache-hit range reads,
and small committed edits. End-to-end visible-input latency requires Stage 03.

## Recommended storage model

Use **one paged piece tree** for every editable document. A piece is a reference
to immutable text in a source file: source identity, byte offset, and byte length.
Editing rearranges/splits these references and appends inserted text to private
storage; it does not shift or copy the untouched remainder of a large document.

| Choice | Reason and tradeoff |
| --- | --- |
| Shared engine for small/large documents | One set of edit, revision, newline, and undo semantics; more initial storage work than an in-memory-only editor |
| Private original snapshot plus append-only added text | External writes cannot later alter accepted document content or undo; opening needs temporary disk space and preparation time |
| Balanced, copy-on-write B+ tree with disk-backed nodes | Byte-range lookup and edits do not require every piece descriptor in RAM; old roots remain valid for snapshots/undo |
| Positional reads and bounded caches | No whole-file `String`, per-line object array, or file-sized memory map; cache misses run off the UI thread |
| Separate, paged source-line indexes | Millions of short lines do not require one resident offset per line; counts/locations may initially be pending |

An in-memory rope remains a reasonable alternative for ordinary files. The
recommendation avoids adding a second backend and later reconciling its behavior
with a larger-than-RAM engine. This is our design choice, not a claim that an
existing piece-tree implementation automatically supplies paging or safe saves.

Use standard-library file APIs, including Unix positional reads, where possible.
Selected additions are `tempfile` 3.27.0 for working/save files and `memchr` 2.8.3
for scanning newlines/NUL bytes. Existing Unix dependencies may support capability probes.
Versions/features/maintenance were checked before implementation;
do not add a database, async runtime, Ropey, highlighting, or search engine merely
for this stage. Keep any platform-specific snapshot/metadata calls in one adapter.

## Text and path policy

| Case | Required behavior |
| --- | --- |
| UTF-8, including ASCII | Supported, without Unicode normalization |
| Leading UTF-8 BOM | Record as format metadata, omit from logical text offsets, and preserve on save |
| UTF-16/UTF-32 BOM | Report unsupported encoding before applying NUL detection; do not call it binary |
| Invalid UTF-8 without a recognized BOM | Report unsupported/invalid text encoding with a byte offset; no lossy replacement |
| NUL elsewhere | Treat as detected binary: “Cannot open binary file”; this is an explicit heuristic, not perfect format identification |
| Other control characters | Preserve bytes; later rendering must display them safely rather than execute terminal sequences |
| LF and CRLF | Preserve existing terminators, including mixed files and absence of a final newline |
| Standalone CR | Preserve as text, not a line break; legacy CR-only line navigation is deferred |
| Directory, FIFO, socket, device | Reject as unsupported file type without reading/blocking on it |
| Missing existing-file path | Clear open error; creation uses a separate unnamed/new-document operation |
| Symlink to regular file | Open resolved target; retain supplied path and resolved identity for save-conflict checks |
| Readable but non-writable source | Allow edits to private storage; same-path save may fail, while Save As remains possible |

Encoding validation and binary detection cover the **entire snapshot**, not only a
prefix. Check recognized BOMs first; within inspected bytes, NUL takes precedence
over a generic encoding error. An early read can reject immediately, but a
prefix-only success is
provisional. Reject inserted NULs and invalid UTF-8 as well, so edits do not create
a document that this policy would reject on reopening.

The storage layer never normalizes inserted line endings. The later Enter command
uses the first encountered LF/CRLF style, or LF for an empty/no-newline document;
literal paste/replacement retains its supplied terminators. LF defines line
boundaries; a preceding CR belongs to the same terminator. Empty text has one
logical line; text ending in LF has a final empty line. Format metadata is part of
the content state used for dirty checks and serialization.

Use `PathBuf`/OS strings; do not require UTF-8 filenames. Display-safe path formatting
belongs at the UI boundary. Rejecting a source never modifies it or other documents.

## Opening and stable content

Opening has four observable outcomes: provisional preview, progress, ready editable
document, or failure/cancellation. Preview is explicitly loading/read-only, not a
successful editable open. There is no permanent size-based read-only mode.

1. On a storage worker, inspect path type/resolution and use nonblocking open flags
   plus descriptor type checks so a file-to-FIFO race cannot block opening.
   Do not trust a pre-open pathname check alone. Capture identity,
   length, nanosecond modification/change timestamps where available, link count,
   permissions, and relevant metadata. Revalidate pathname/descriptor identity.
2. Read at most 64 KiB of initial text, allowing up to three carry bytes for a
   split UTF-8 scalar. Return a bounded, validated preview without scanning the
   whole source. A preview error is distinct from a completed document.
3. Create private working files in a per-instance directory (0700, files 0600).
   Prefer an independently writable copy-on-write filesystem clone **only when
   available and tested**. The mandatory portable fallback is a cancellable,
   bounded-buffer copy; never substitute a hard link or original descriptor.
4. Validate the complete private copy, collecting format/line summaries as it is
   read. In the copy fallback this can run during copying. Check source identity
   and version before/after preparation; observed change yields `SourceChanged`
   with explicit retry, not an automatic endless reopen loop.
5. Publish the first editable root only after private content and full text
   validation succeed. Replace any provisional preview with that snapshot's
   content. Line-index acceleration may continue afterwards; publish exact counts
   immediately if validation already produced them. Opening an unchanged
   named file starts clean; a new empty unnamed document starts without a saved path.

Copy-on-write cloning is an optimization; its absence is not a platform failure.
The fallback can make the interval between preview and editing several seconds
for a large file. Report separate preview/ready times and disk cost. This tradeoff is part of the authorized implementation baseline. Do not hide the
wait or claim immediate large-file editing.

A file descriptor does not freeze another process's in-place writes. Once Ready,
only private immutable bytes back document roots, so later replacement, deletion,
truncation, or writes to the original cannot corrupt the document or undo. A
portable streamed copy is **not an atomic point-in-time snapshot of a concurrently
modified source**. Version checks detect observed changes, but cannot establish
that guarantee against an uncooperative concurrent writer. Direct live following
is a separate Stage 08 source type.

Working files are session storage, not a recovery format. Keep open handles rather
than reopening trusted data by a potentially stale temporary pathname. Remove
unreferenced files on orderly cleanup; use owner-scoped cleanup for abandoned
instance directories, never a blanket deletion of other instances' data.

## Core contracts

Keep terminal input, UI geometry, and filesystem access out of the document model's
semantic types. Use concrete Rust types and cohesive modules rather than a generic
storage framework. Core responsibilities under `src/document/` (additional cohesive modules are
listed in the validation record):

| Module | Ownership |
| --- | --- |
| `mod.rs` | Small public document/service API; identities, content state, typed errors |
| `service.rs` | Worker requests/results, per-document ordering, cancellation and leases |
| `source.rs` | Private immutable originals, added-byte storage, bounded text-page cache |
| `tree.rs` | Persistent piece tree, root publication, range traversal and node paging |
| `index.rs` | Incremental source block/newline summaries and line lookup |
| `history.rs` | Transaction roots, undo/redo, dirty/save-point state and pruning |
| `save.rs` | Save preparation, guarded publication, metadata and conflict outcomes |
| `platform.rs` | Small macOS/Linux filesystem capability/metadata adapter |

Do not split further unless implementation complexity warrants it. Tests should
exercise public behavior and fault-injection seams, not require this exact file
count. Stage 01's `status` mailbox carries brief progress text, not document data
or structured completion results.

| Concept / operation | Contract |
| --- | --- |
| `DocumentId`, `JobId`, `SourceId` | Distinct identities; never reuse a closed document identity for late results |
| `Revision` | Monotonically increases after every published content change, including undo/redo; checked for overflow |
| `ContentState` | Immutable tree root plus format metadata; distinct from revision, permitting exact undo-to-save-point dirty checks |
| `SavedBaseline` | Leased immutable content state plus destination identity/version metadata; never backed solely by a mutable destination descriptor |
| `Snapshot` | Read lease on one content state/revision; remains readable through later edits/save/path replacement until released |
| `ByteOffset`, `TextRange` | Checked `u64` offsets; half-open `[start,end)` ranges, excluding BOM; no document-sized `usize` conversions |
| `read_window(snapshot, start, max_bytes)` | Start at a scalar boundary, require a byte allowance of at least 4, return at most that allowance without splitting a scalar; continuation offset or EOF, never an implicit whole line |
| `locate_line` / `line_of_offset` | Exact answer or explicit pending/cancelled/error; never fabricate a completed line count |
| `apply(expected_revision, transaction)` | Validate/stage all changes and publish one root, or leave visible content/history unchanged |
| `undo` / `redo` | Publish a new revision pointing at the prior/next retained content state |
| `save(snapshot, target)` | Serialize that exact state, reporting saved revision/content state and commit outcome |
| `cancel(job)` / `close(document)` | Cooperative cancellation; prevent stale results from reviving closed documents; release leases deliberately |

Offsets refer to UTF-8 bytes, not scalar counts, graphemes, screen cells, or columns.
Edit endpoints must be scalar boundaries and must not bisect CRLF; inserted text
must satisfy the supported-text policy. Grapheme-aware cursor movement belongs to
Stage 03. Chunk traversal must handle scalars/CRLF split across physical pages;
return valid text chunks or documented carry bytes, never an invalid `&str`.
Encoding-error positions identify raw source offsets, including any BOM; document
range offsets are logical bytes excluding it. Do not mix those coordinate spaces.

Expose typed failures such as `UnsupportedType`, `UnsupportedEncoding`, `Binary`,
`SourceChanged`, `InvalidRange`, `StaleRevision`, `Cancelled`, `ResourceLimit`,
`SaveConflict`, `SaveAsRequired`, and an I/O error retaining its source. Do not
convert cancellation, pending indexing, or partial save commitment into success.

## Tree, indexing, and transaction invariants

- Use a byte-weighted B+ tree with an initial 16 KiB node-page size. Leaves contain
  source ranges; branches contain child page IDs and subtree byte counts. Start
  an opened file as one range, not one object per line or text page. Check all
  offset/weight arithmetic. Non-root nodes stay between half and full capacity;
  the root may be a leaf or have at least two children. Split/merge/rebalance rather
  than accumulating a linear list of edits. Coalesce adjacent contiguous source
  ranges. A BOM-bearing original's initial piece starts after its BOM.
- Tree pages are immutable after publication. Copy the changed path for an edit;
  write newly referenced text/nodes successfully before publishing the new root
  in memory. Old roots and their source handles remain valid while referenced by
  current content, the saved baseline, history, saves, or bounded worker leases.
  Per-edit `fsync` is not required: this is transaction atomicity within a running
  process, not a Stage 05 crash-recovery guarantee.
- Store evictable node/index pages on disk from the outset. Cached roots must not
  keep an entire tree resident through `Arc` links. Persistent page references use
  checked IDs; validate page bounds/type on reads and report corruption, not panic.
  Session formats need not be a public or cross-version file format.
- Index LF counts per 64 KiB source block in a paged count tree; scan within a block
  to locate an individual terminator. Carry CR/UTF-8 boundary state across blocks.
  Derived piece/subtree line summaries may be unknown until the necessary source
  ranges are indexed. Maintain logical-order line lookup over pieces, including
  inserted/deleted terminators. Do not allocate an offset for every line.
- Byte reads do not wait for a global line index. Seeking to a distant line may
  require scanning/indexing earlier unknown ranges; report progress/cancellation.
  Long-line reads/navigation return windows and continuation state, not a complete
  line allocation. Publish the longest-line/highlighting-threshold fact when known;
  the highlighting policy itself remains Stage 07.
- A transaction contains sorted, non-overlapping replacements against one base
  revision. Large batches and inserted content are staged incrementally on disk,
  including their range descriptors. Validate the whole plan before one root is
  published; never expose a half-applied replacement or partially staged paste.
- Undo stores root/state references, not copied deleted text. A transaction is one
  undo unit; new edits clear redo only on successful publication. No-op edits do
  not dirty the document or create history. Undo to the saved content state clears
  dirty status even though its new revision differs from the saved revision.
- Saving never clears history. A save of state R may finish while later edits have
  produced R+1; acknowledge R and keep the current document dirty. Old private
  sources cannot be discarded or rebound to a mutable saved path after save.
- Reclaim unreachable pages/added text through incremental compaction with an
  atomic root/source swap. Retain every live lease/history root, or wait/cancel
  background work; do not compact by materializing the document or forgetting undo.

## Scheduling and resource budgets — initial defaults

These are explicit, tunable implementation limits to benchmark. Use a `StorageLimits` value in core tests/API now;
configuration-file/UI exposure comes in Stage 09. Caches are shared across open
documents and leased snapshots rather than multiplied independently per tab.

| Resource | Starting budget / behavior |
| --- | --- |
| Resident text pages | 64 MiB aggregate, initially 64 KiB pages |
| Tree and line-index caches | 32 MiB aggregate; disk-backed overflow |
| Worker buffers, pending payloads, transaction staging | 32 MiB aggregate, including pinned chunks/leases; large data spools to disk |
| Storage requests/results | 16 bounded entries each, also charged by payload bytes; full queues return Busy/backpressure |
| Active storage work | At most two worker threads; per-document mutations serialized; bounded background slices allow foreground reads to progress |
| Undo history | Initially 1,000 transactions per document; descriptors paged; account separately for text retained by undo/redo |
| Non-current text retained by undo/redo | Initial 256 MiB per document; count each unique retained source range once, even if a baseline/lease also references it; prune oldest whole transactions with an explicit `HistoryPruned` event |
| Retained read/save leases | Initially 8 per document, plus its current state and one saved baseline; callers release/cancel job leases; no unbounded snapshot pinning |
| Working disk | Reserve at least 512 MiB free beyond planned work; account originals, added text, node/index/history files, compaction, and save staging separately |

The 128 MiB aggregate managed-memory budget includes metadata/queue overhead and
buffers, not only live text. Start with a **256 MiB process peak-RSS target** for
the single-document 1 GiB benchmark; measure allocator/runtime overhead explicitly.
Neither target is a file-size limit. Report OS page cache separately, without
claiming to control it. Reserve capacity before pinning buffers or admitting work;
return a recoverable resource error rather than exceed a bound or make an edit
silently non-undoable. If retaining even the newest undo transaction exceeds its
budget, reject the edit before publication rather than immediately pruning it.
Consequently, a single very large deletion/replacement can require a higher undo
budget; this is a resource policy, not a read-only file cutoff.
Charge retained ranges conservatively at page granularity if exact range accounting
would be more expensive. Moving the save point must not newly charge data that
was previously unaccounted for and unexpectedly destroy undo history.

Disk has no fixed file-size cutoff: opening needs enough working space for the
chosen snapshot method, plus reserved headroom. A clone is not a promise of free
future writes. Preflight free-space estimates are advisory; every write can still
fail. Disk allocation accounting and garbage collection must prevent unbounded
abandoned history/job files. If compaction requires space that is unavailable,
preserve existing roots and report that more working space is needed.

The UI submits work without waiting for disk reads, copying, indexing, edits, or
saves. Viewport results contain owned, bounded windows; a cache miss reports
pending. Structured results include document identity, revision, job identity,
and request identity; discard stale display/index results. Save completion for an
older state is still meaningful and updates only that saved state.

Check cancellation between chunks, initially no larger than 1 MiB, and between
transaction/index work slices. Coalesce progress; preserve completion/errors with
backpressure. A worker blocked inside an OS filesystem call cannot be guaranteed
to stop within 50 ms. No blocking disk operation or indefinite worker join may
move into the terminal event loop. Stop accepting work on shutdown, cancel
pre-commit work, and resolve any publishing save before releasing its resources.

## Saving and external changes

Use streamed, same-directory temporary output followed by guarded publication.
Never truncate the destination in place and never fall back to that on failure.
Filesystem guarantees depend on the filesystem; test local macOS/Linux filesystems
and report unsupported behavior rather than claiming universal network-filesystem
or power-loss guarantees.

### Destination policy

| Destination | Policy |
| --- | --- |
| New Save As path | Must not exist; publication must fail if another process creates it first |
| Existing ordinary single-link file | Require the expected baseline; preserve ordinary mode bits, owner, and group or fail |
| Symlink-opened document | Save the previously resolved target, preserving the symlink; recheck resolution and target identity; retargeting is a conflict |
| File with multiple hard links | Initially return SaveAsRequired rather than silently break aliases or overwrite them in place |
| ACLs, extended attributes, special permission bits, filesystem flags | Detect them; preserve through a tested platform adapter or return SaveAsRequired before replacement. Silent metadata removal is forbidden |
| Unwritable target / unsupported metadata preservation | Retain edits and explain failure; the UI can offer another Save As path later |

New-file permissions are 0600; changing that default is a review decision.
Private working and pre-publication staging files must be private regardless.
Creating an absent path and replacing an expected existing file are distinct API
operations. Existing-target Save As needs an explicit expected baseline/overwrite
decision; do not add an unconditional force-overwrite path in Stage 02.

### Save sequence and outcomes

1. Lease the requested immutable state and capture the destination's baseline and
   parent identity. For an existing named file, compare path/target identity and
   metadata against the last opened/successfully saved baseline; a known change
   returns `SaveConflict`. No automatic reload discards local edits.
2. Create output in the target directory, not a global temporary filesystem.
   Stream BOM plus logical text into it using bounded buffers. Apply/verify the
   required destination metadata. Flush and `sync_all` the output before publication.
3. Before replacing an existing target, compare its **complete current bytes**
   with the private baseline using bounded reads; metadata alone is insufficient.
   Recheck identity/version around that comparison and immediately before publish.
   A changed/deleted/replaced target, changed symlink resolution, or an I/O error
   cancels replacement and preserves the edited state.
4. Acknowledge cancellation up to the commit point. Publish using atomic rename
   for an expected existing target, or a tested atomic no-clobber name-creation
   primitive for a new path. Do not implement no-clobber as `exists()` then rename.
5. After successful name publication, cancellation can no longer mean “not saved.”
   Attempt parent-directory synchronization. Verify the published name/identity,
   metadata against the staged output (allowing rename's ctime change), and
   complete bytes against the immutable serialized state. Do not acknowledge an
   observed external change during this verification or use a fresh stat alone
   as evidence of success. Establish the new destination
   baseline from the immutable content state serialized, with the newly published
   file's identity/version metadata. Never use the published, externally mutable
   file as the only record of expected baseline bytes. Report one of:
   `Saved`, `NotCommitted`, or `CommittedDurabilityUncertain`. The latter means the
   path was updated but a subsequent durability/verification step failed; retain
   work and communicate uncertainty rather than claim the old file is untouched.

Before commit, any failure/cancellation leaves the original destination unchanged
by this operation and preserves current content/history. Remove incomplete output
when safe. After commit, never attempt a blind rollback over another writer's work.
Only a fully acknowledged saved state becomes the clean save point; uncertainty
keeps an attention/dirty state until explicitly resolved.

Atomic replacement is not a portable compare-and-swap of file contents. There is
a residual race between the final conflict check and rename against uncooperative
writers. The proposed policy detects observed changes and never intentionally
overwrites a known conflict; it does **not** promise race-free concurrent saves
across arbitrary applications. This limitation is part of the authorized persistence baseline; stronger
coordination would require a design change. Reload/recovery/conflict UI
belongs to later interaction stages; the core already returns precise outcomes.

## Implementation sequence

1. Integrate accepted Stage 01. Add document types, checked ranges, format policy,
   errors, and resource accounting; keep the shell/CLI behavior intact.
2. Implement private snapshot preparation, bounded reads, and cancellable opening.
   Establish preview/Ready/error semantics before enabling edits.
3. Implement paged tree primitives and revision leases, then transactions and
   undo/redo. Start correctness checks against a small in-memory reference model;
   it is a test oracle, not a second production backend.
4. Add source-line indexes, long-line windows, limits/pruning/compaction, and typed
   worker requests/results. Verify bounded operation with deliberately tiny caches.
5. Implement save preparation, metadata/conflict checks, publication and precise
   post-commit outcomes. Test every failure boundary using injectable I/O.
6. Run native fmt/Clippy/tests and storage benchmarks. Record results and remaining
   limitations; PR into `feat/initial_mvp`. A failed feasibility goal requires a fix
   or an explicit reviewed design change, not quiet deferral to the final stage.

Development package version: `0.0.1-dev.2`.
Rust/toolchain/UI dependency choices remain Stage 01's unless separately changed.

## Acceptance and evidence

| ID | Required result |
| --- | --- |
| D01 | Exact byte round trips for supported UTF-8, BOM, LF/CRLF mixtures, final/no-final newline, empty text, Unicode and non-UTF-8 filenames; distinct binary/encoding/type errors |
| D02 | Preview arrives without completing full preparation; late invalid UTF-8/NUL causes failure before editing; cancelled/changed-source opens leave no published editable document |
| D03 | Once Ready, external overwrite/truncate/delete/rename cannot change current text, leased revisions, or undo; forced copy fallback works on both platforms |
| D04 | Randomized transactions match a reference model; overlap/bad boundaries/overflow/stale revision/failed staging cannot partially mutate content or history |
| D05 | Undo/redo and undo-to-save-point work across page/piece boundaries; saving preserves undo; an older save completing during newer edits does not mark the newer state clean |
| D06 | Scalar/CRLF boundaries, line queries and continuations are correct with tiny pages, inserted/deleted terminators, millions of short lines, and a line much larger than caches |
| D07 | Text, node/index, pending payload, pinned lease, undo, and disk accounting stay bounded under eviction, edit churn, saturation and compaction; history pruning is visible |
| D08 | Inject read/write/flush/sync/metadata/publish/directory-sync errors and cancellation before/after commit; content survives and reported destination outcomes match the actual filesystem |
| D09 | External edits, changed timestamps/content, replaced targets, symlink retargeting, hard links, permissions and advanced metadata cannot silently lose known changes or metadata; new-path races never clobber |
| D10 | Worker completion/order/cancellation tests keep UI submission nonblocking; stale results/closed documents are ignored; full queues never lose a save/error completion |
| D11 | Stage 01 shell tests stay green; Linux/macOS CI runs fmt, Clippy and all core/functional checks with the lockfile |
| D12 | The benchmark record below demonstrates feasible large-file storage; unmet budgets/targets and missing environments are reported honestly |

Routine CI uses small fixtures and forced-small limits, exercising real eviction
and disk paging without requiring gigabytes on every PR. Fault-injection tests
must be supplemented by real-filesystem functional tests; mocks alone cannot
establish rename, permission, or clone semantics.

Benchmark a real, fully allocated 1 GiB UTF-8 file with ordinary lines, a
newline-dense corpus, and a very-long-line corpus. Record CPU, available RAM,
filesystem/storage, clone versus forced-copy path, cache conditions, preview and
editable-ready time, index completion time, cache-hit/miss reads, occasional
beginning/middle/end edits, undo/redo, save time, peak RSS and working-disk usage.
Keep fixture generation/verification bounded; sparse/NUL-filled files are not a
substitute for this text benchmark. Warm and cold results must be distinguished;
do not change privileged OS cache settings merely to produce a claimed cold run.

Validate the 1 GiB workload on an 8 GiB machine or a documented equivalent memory
constraint; a larger host with small engine caches proves paging, not that machine
baseline. Also run a text workload larger than the managed-memory budget with
constrained process memory, and a genuinely larger-than-available-RAM workload
where an environment is available. Record any unavailable hardware check and seek
an agreed deferral before claiming that requirement is verified. Report full-copy
readiness separately from the 1-second preview goal. Stage 03 measures actual UI
latency; Stage 10 validates the final integrated, larger-than-RAM experience.

## Implementation baseline decisions

1. One paged engine and private snapshots, accepting temporary disk use and a
   loading interval before editing on filesystems without a fast clone.
2. UTF-8-only editing, BOM/newline preservation, and explicit binary/encoding rules.
3. Starting memory/history budgets and 0600 new-file permissions.
4. Safe-save treatment of links/advanced metadata and the stated external-writer
   race/durability limits.

Implementation was explicitly authorized on 2026-11-02 using these design
defaults. Material changes to requirements still need review.

## Research references

- [Piece-tree design and tradeoffs](https://code.visualstudio.com/blogs/2018/03/23/text-buffer-reimplementation)
- [Ropey representation and UTF-8 indexing](https://docs.rs/ropey/latest/ropey/)
- [Rust Unix positional I/O](https://doc.rust-lang.org/std/os/unix/fs/trait.FileExt.html)
- [Temporary-file lifecycle and permissions](https://docs.rs/tempfile/latest/tempfile/)
- [memchr scanning primitives](https://docs.rs/memchr/latest/memchr/)
- [Apple clonefile semantics](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/man/man2/clonefile.2)
- [Linux reflink semantics](https://man7.org/linux/man-pages/man2/ioctl_ficlone.2.html)
- [Linux rename semantics and link behavior](https://man7.org/linux/man-pages/man2/rename.2.html)
- [Rust file synchronization and errors](https://doc.rust-lang.org/std/fs/struct.File.html#method.sync_all)
- [Directory synchronization requirements](https://man7.org/linux/man-pages/man2/fsync.2.html)

## Validation record

Implementation was authorized on 2026-11-02. Validation spans 2026-11-02/03.
Package: `0.0.1-dev.2`, pinned Rust 1.99.1. Local formatting, Clippy with warnings
as errors, and all **53 tests** pass: 17 library, 3 CLI, 28 document integration,
and 5 terminal integration tests. Linux adds one non-UTF-8-filename check (54 total).
[Final code CI](https://github.com/rapidclock/ede/actions/runs/41604878400) passes
on native Linux and macOS, including all four memory-capped benchmarks. The Linux
measurement table below records the [benchmark run](https://github.com/rapidclock/ede/actions/runs/41600228777);
the final compaction stress measurement was run separately on the local host.
The [stage PR](https://github.com/rapidclock/ede/pull/2) targets `feat/initial_mvp`.

### Acceptance evidence

| Checks | Evidence |
| --- | --- |
| D01 | Exact BOM, UTF-8, control-byte and mixed/final/no-final-newline round trips; distinct unsupported encoding, binary and type failures; OS-string paths and Linux non-UTF-8 filenames |
| D02 | Provisional preview before whole-file validation, late invalid encoding/NUL, cancellation, observed source changes and preparation-sync failures; no Ready or leaked working directory on failure |
| D03 | Forced-copy tests on both platforms; optional clone path on capable filesystems; external overwrite, truncate, unlink and rename preserve snapshots and undo |
| D04–D05 | 1,500 deterministic reference-model edit/undo/redo operations; multi-replacement atomicity, scalar/CRLF bounds, stale epochs, failed edits retaining redo, single-replacement/net-zero batch no-ops and undo-to-save-point; older-revision save keeps newer edits dirty |
| D06 | More than a million short lines, a 1 MiB unbroken line, scalar/page/CRLF crossings, byte/line queries and continuations; 1 GiB dense/long workloads below |
| D07 | Tiny-cache eviction, structural B+ occupancy/corruption checks, bounded snapshot/window/path admission, metadata retained after document close, explicit history pruning/newest-undo rejection, lease-safe compaction, shared-page metadata reclamation, source-hole packing and history preservation |
| D08–D09 | Injected read/write/flush/sync/metadata/publish/directory-sync/verification failures; cancellation before/after commit; no-clobber creation races and final known conflicts; symlinks, hard links, modes and native xattr behavior |
| D10 | Bounded request/result admission, cancellation and close under backpressure, stale closed results; a deliberately blocked background open still allows foreground snapshot reads |
| D11 | Existing terminal/CLI acceptance remains green; native Linux/macOS workflow uses the lockfile, fmt, Clippy and all tests |
| D12 | Real allocated text fixtures, complete saved-byte verification, beginning/middle/end edits and undo/redo, default-history churn, native APFS clone/copy comparison and swapless Linux memory caps |

### Benchmark conditions and results

The Rust harness is `examples/storage_bench.rs`, built in release mode. Every run
writes a real, fully allocated text fixture with bounded buffers, then opens,
reads, edits at beginning/middle/end, undoes/redoes, performs Save As and verifies
all output bytes. The three-position edit maximum is separate from optional
1,000-edit churn. Full preparation and line indexing finish together in this
implementation. Save timing below is creation of a new destination; same-path
conflict checking is covered by functional tests rather than these timings.

Local host: MacBookPro18,2, Apple M1 Max (10 CPU cores), 32 GiB RAM, macOS 27.2
(26B123), internal SSD/APFS. Inputs were generated immediately before each run;
there was no OS cache flush. First/repeated window reads are reported separately
by the harness; no cold-cache measurement is claimed. All lengths below are 1 GiB.

| Corpus / snapshot | Preview ms | Ready + index ms | Max edit ms | Save ms | Peak RSS MiB | CPU seconds |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ordinary / copy | 0.800 | 1,643 | 1.103 | 1,465 | 123.6 | 1.171 |
| Dense / copy | 0.979 | 5,141 | 0.912 | 1,499 | 118.3 | 4.592 |
| Long / copy | 0.401 | 1,491 | 0.819 | 1,489 | 115.1 | 1.015 |
| Ordinary / APFS clone | 0.197 | 420 | 1.336 | 1,681 | 102.8 | 1.028 |
| Dense / APFS clone | 0.382 | 4,066 | 1.419 | 1,529 | 93.6 | 4.558 |
| Long / APFS clone | 0.201 | 284 | 1.355 | 1,686 | 106.8 | 0.967 |

Actual filesystem cloning was confirmed for the three clone runs. First/repeated
4 KiB window reads were at most 0.031/0.004 ms in these runs. A separate 1 GiB
copy run retained 1,000 worst-order typing transactions: median 5.489 ms,
p95 10.404 ms, maximum 19.860 ms. Compaction took 244.690 ms, reducing metadata
from 21,920,520 to 21,871,176 bytes; all 1,000 transactions could then be undone.
Peak RSS was 130.6 MiB, managed caches 96.0 MiB, working metadata about 20.9 MiB. These are single-host
engineering measurements, not UI-latency or hardware-independent guarantees.

Linux CI: Ubuntu 24.04.5 x86_64 VM, 2 vCPUs of AMD EPYC 9V74, approximately
7.7 GiB usable system RAM, ext4 on `/dev/nvme0n1p1`. Runs use systemd cgroup v2
`MemoryMax=8G` or `256M`, `MemorySwapMax=0`, with actual limits printed by the
harness. The cap includes anonymous memory and charged filesystem cache; the
host/kernel remain outside this application cgroup. This documents the 8 GiB
baseline and a 1 GiB workload larger than its enforced 256 MiB memory allowance.

| Corpus / memory cap | Preview ms | Ready + index ms | Max edit ms | Save ms | Peak RSS MiB | CPU seconds |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Ordinary / 8 GiB | 0.182 | 11,968 | 0.241 | 10,942 | 66.5 | 1.259 |
| Dense / 8 GiB | 0.443 | 10,703 | 0.224 | 9,845 | 66.7 | 3.472 |
| Long / 8 GiB | 0.173 | 8,118 | 0.230 | 8,262 | 66.5 | 1.181 |
| Ordinary / 256 MiB | 0.588 | 12,843 | 0.338 | 12,201 | 66.7 | 2.113 |

These runs used the mandatory copy fallback. Peak RSS comes from `getrusage`,
not the systemd service summary. Largest first window read was 0.662 ms; repeated
reads were at most 0.004 ms. Cache/physical storage varied by environment, so the
longer Linux preparation/save times are reported separately from preview and
small-operation responsiveness.

CPU time excludes fixture generation and includes saved-byte verification.
Typical managed cache charge after a standard benchmark was 64.1 MiB. Logical
private working storage was 1,073,741,839 text bytes plus 196,872 metadata bytes, excluding
the input fixture, saved output and filesystem allocation overhead. The harness needs roughly 3 GiB of temporary
filesystem capacity for the fixture, private copy and save output, plus configured
free-space headroom. Cloning may reduce physical allocation but is not counted as
free future write capacity. OS page cache is not included in process RSS; its
charge is subject to the Linux cgroup limit.

The first 256 MiB runs were OOM-killed with only end-of-file synchronization.
Long writes now checkpoint data every 8 MiB in private copies, transaction spools,
save staging and fixture generation. This allows dirty file pages to become
reclaimable without synchronizing each small edit. The same 256 MiB workload then
passed. The checkpoints trade preparation/save throughput for bounded dirty
working sets on constrained systems; final save still performs full output and
parent-directory synchronization.

### Implementation refinements

- `types` defines values/policy; `model` owns document state; `source` owns byte
  arenas/read handles; `snapshot` and `transaction` own bounded leases/plans;
  `resources`, `cache` and `bitmap` implement admission/paging; `compact` performs
  collection. `tree`, `index`, `history`, `save`, `platform` and `service` keep their
  stated responsibilities. `document` re-exports the public API; the shell is unchanged.
- One worker owns all mutations, opens and saves; a second is reserved for
  foreground immutable-snapshot reads/line lookups. Snapshot handles duplicate
  positional-read file descriptors, so background work does not retain a lock
  needed by the reader worker. Both share aggregate caches and buffer admission.
- Source indexes use a disk array of cumulative LF counts at fixed block
  boundaries. Direct prefix lookup and binary search provide the source count-tree
  operations without resident per-line offsets; edited logical order uses the B+
  tree's byte/LF weights. Original longest-line information is recorded at open.
- History roots are fixed-width, append-only disk records with backwards links.
  Failed edits append unreachable records instead of overwriting redo. Undo text
  accounting uses bounded-cache disk bitmaps and skips shared/already-visited tree
  subtrees; partial current pages are conservatively charged.
- Compaction first marks all current/history/baseline references with bounded-cache
  disk bitmaps. It packs live text blocks in physical order and copies each shared
  tree page once using disk remapping, preserving B+ shape rather than replaying
  edits. When every text block is live, it retains the text/index through owned
  handles instead of copying a large file merely to collect metadata. Old paths
  may be unlinked; handles remain authoritative. The complete generation swap is
  atomic. Compaction returns Busy while external leases are held. Before more
  edits, compaction is requested after 64 MiB of node/history
  storage or text growth beyond the live-state/history allowance plus 1 MiB;
  callers release leases and retry when required.
- macOS extended attributes up to a 64 KiB names/value/accounting envelope use
  native descriptor copying followed by exact verification. ACLs and file flags
  remain SaveAsRequired; Linux extended attributes, including ACLs, also require
  Save As. New files use mode 0600. Save staging lives in a same-filesystem 0700
  directory, keeping final mode bits private until publication. Darwin inherited
  directory ACLs that defeat privacy are rejected before private bytes are created.
- Non-UTF-8 filenames are exercised on Linux. APFS rejects their creation; path
  handling throughout the core still uses OS strings rather than UTF-8 conversion.
- A 32 KiB store-metadata reservation lasts until the last snapshot releases its
  generation, including after tab close. Requests charge owned string/path capacity,
  not only text length. Invalid buffers smaller than the fixed scratch/metadata
  requirements return ResourceLimit before work starts.

### Limits and deferred integration

- Stage 02 storage is available as a Rust API and worker service. The terminal
  shell still does not open/render/edit files; that is Stage 03. No later-stage
  implementation or design is authorized by this delivery.
- UTF-8 only; NUL is a binary heuristic. ACLs, special flags/permissions, hard
  links and unsupported extended metadata require a new destination rather than
  silently dropping metadata or modifying linked aliases.
- Original longest-line information is exact at open; recomputing a current
  longest line after arbitrary edits is left to the later highlighting policy.
  Byte/line lookup and line counts remain exact after edits.
- Whole-file private preparation is required before editing. Full-copy opening is
  not an atomic point-in-time snapshot against an uncooperative writer; final
  identity/content checks also cannot turn rename into portable compare-and-swap.
- Temporary session files are not crash recovery. Filesystem/power-loss and
  network-filesystem guarantees remain limited as stated above. No cold-cache,
  reconnect/transport or end-to-end UI performance result is inferred here;
  Stage 01's agreed manual transport deferrals remain recorded in its spec.

### Review pass — 2026-11-03

Reviewed D01–D12 and Stage 01's acceptance criteria against the implementation,
failure paths and actual test coverage. This review found and fixed:

- **Incorrect save acknowledgement:** an in-place writer after publication could
  be accepted as the new baseline because verification compared two fresh stats
  without checking the published bytes. Saves now compare staged output metadata
  and complete immutable bytes before acknowledging success. Observed post-commit
  changes retain edits/undo and return `CommittedDurabilityUncertain` with
  `SaveConflict`; another writer's contents are never rolled back. Cancellation
  is checked again immediately before publication and ignored for verification
  after commitment. This adds one bounded sequential read to successful saves.
- **Missing in-flight window cancellation:** the foreground worker checked
  cancellation only before a potentially multi-page read. The cancellable window
  API now checks between text pages (at most 1 MiB by validated policy), and before
  returning. The service uses this API; its gated test proves it stops after the
  current read rather than reading the remaining pages.
- **Incomplete shutdown after panic:** a failed first worker join returned early,
  detaching later workers. Shutdown now waits for every worker before reporting
  failure. A gated second worker verifies the lifetime guarantee.

Both the save acknowledgement and in-flight cancellation regressions failed
against the prior code and pass after correction. Further tests cover post-commit
permission changes and same-length changed bytes with restored mtime, 750 seeded
Unicode/CRLF multi-replacement/history/save/compaction operations, and failed or
cancelled compaction preserving current state, baseline, undo and redo. Stage 01
also keeps signal subscriptions alive throughout terminal restoration.

Local macOS: fmt, Clippy with warnings denied, all **59 tests** (18 library,
3 CLI, 33 document integration, 5 PTY), and Rust API documentation with warnings
denied pass. [Review-fix CI](https://github.com/rapidclock/ede/actions/runs/41720605707)
for runtime commit `cba98ce` passes native Linux fmt, Clippy, all **60 tests**
(including the Linux-only filename case), and all four swapless 1 GiB benchmarks:
ordinary/dense/long under the 8 GiB cap, plus ordinary under 256 MiB. The hosted
macOS job remains queued; the local native macOS checks above are complete.

The release 1 GiB ordinary-text forced-copy benchmark with 1,000 retained edits
passes on the same local APFS host described above: preview 0.547 ms,
Ready/index 1,913.039 ms, three-position edit maximum 1.085 ms, churn median
5.467 ms / p95 9.988 ms / maximum 10.821 ms, compaction 234.191 ms, and Save As
including post-publication byte verification 1,956.229 ms. Peak RSS is 173.7 MiB
(below the 256 MiB target), managed caches 96.0 MiB, working metadata 21,871,176
bytes. All 1,000 undos and complete saved-byte verification pass. Cache condition
is generated/no flush; these are single-host measurements, not UI guarantees.

No outstanding acceptance-blocking defect was identified in the reviewed paths.
The documented concurrent-writer/filesystem limits and user-approved Stage 01
manual compatibility deferrals remain; this review does not establish absence of
all bugs or verify unavailable terminal/remote environments.
