# 10 — Integrated MVP validation

Status — **Implementation authorized. Integrated validation in progress.**

Branch — `stage/mvp_validation`. PR target — `feat/initial_mvp`.

Depends on Stages 01–09, integrated through Stage 09 at `dfacfe2`. The
[approved MVP overview](overview.md) defines the scope and FR-01–FR-10. This
specification defines the validation scenarios, reference environments, and
pre-release changes. The user authorized Stage 10 implementation and validation
after confirming the shortcut checks should run last. Record the actual terminal
versions and profiles used for validation.

## Outcome and scope

A source build of ede at package version `0.0.1` can be used for ordinary editing,
occasional editing of files larger than RAM, and live-output inspection. The
stage closes when the acceptance criteria have recorded evidence and a user can
build, operate, configure, and recover the editor with the README. Reuse the
existing CLI, TUI, storage policies, and tests. Implementation work covers
validation tooling, fixes needed to satisfy the approved contracts, and the
pre-release changes below. Packaging, installers,
automatic updates, release distribution, and deployment are outside this stage.

Use prior validation records for detailed behavioral coverage, failure guards,
and known limitations. Match each reused result to the code and environment it
actually exercised. Rerun applicable evidence when intervening changes affect
its behavior or conditions. Run the integrated workflows, candidate build and
native gates on the candidate revision. A Stage 01 or Stage 03 deferral of a
manual check leaves it open for Stage 10.

## Approved pre-release changes

The user reported that Copy in ede on Ubuntu left the Mac clipboard unchanged,
both through their tmux pane and in Terminal.app over direct SSH. They proposed
a Settings choice to keep both clipboard workflows. They also reported that
physical Cmd shortcuts invoked Mac or terminal actions, including a dialog to
export terminal output on Cmd+S. Stage 10 covers the clipboard and UI changes
below and verifies the existing Cmd requirement at the end.

### Clipboard setting and client paste

Stage 03 defines a private clipboard per running ede process. Settings offers
**Internal**, **System**, and **Both**. In `config.toml`, use `clipboard.mode`
under `[clipboard]` with `"internal"`, `"system"`, or `"both"`. Omission and
Restore Defaults select Both. `--safe-config` always uses Internal. Legacy
`copy_to_system = false` means Internal and `true` means Both. A config may
specify one key. Apply changes through the existing save/reload revision path
and retain the last good revision on an invalid reload.

- **Internal** and **Both** retain explicit Copy and committed Cut in ede's
  private clipboard. `Ctrl+V` and a directly delivered Cmd+V paste those bytes.
- **System** copies only to the terminal. An existing private copy stays
  available when switching back to Internal or Both. Ede's Paste shortcut
  directs the user to the terminal Paste command. Cut leaves the document
  intact and asks the user to switch to Internal or Both because a terminal
  write cannot confirm that the cut text is recoverable.
- In every mode, the terminal's Paste action supplies system clipboard text
  through terminal input. If the terminal handles Cmd+V itself, insert the
  received text once.
- In System and Both, admit one OSC 52 clipboard write for a completed Copy or
  eligible, committed Cut when the captured mode is still current. The destination is
  the client terminal's system clipboard, including when ede runs remotely.
  Use only the write operation. Reading system clipboard contents is outside
  this integration.
- Limit outgoing selections to 8 KiB of UTF-8. Check exact round trips up to
  that limit on each supported path.
  In Both, leave larger selections available internally and explain the
  limit. In System, report that the selection was not copied. Send one bounded
  base64 payload with terminal framing.
  Keep spool reads off the UI thread and serialize the frame with other tty
  output. Never emit raw selection bytes as terminal commands or write the
  sequence to redirected document output.
- No selection, failed Copy, and failed or cancelled Cut send no clipboard
  write. A successful Cut whose external write fails leaves all cut text in
  the internal clipboard. Before transmission, retire a pending write if
  overtaken by a newer completed Copy or Cut, a clipboard mode change,
  or terminal suspension or teardown. Re-enabling starts with future commands
  and cannot revive an older write. Serialize accepted writes so older data
  cannot follow a newer write.
- Finish an admitted frame before sending unrelated output. If the write fails
  after sending any frame bytes, recover the terminal parser with a verified
  abort sequence before drawing again, or treat the tty as unusable under the
  existing terminal-loss policy. Do not retry a partial payload or wait
  indefinitely. Recoverable write failures preserve the internal clipboard and
  committed Cut state. A lost tty follows the existing interrupted-session
  recovery rules.
- Report **Copied internally** or **Cut internally** when a private copy was
  saved without a system write. Include the reason when the requested write
  was skipped or failed. In System mode, report why a failed copy did not send.
  When the frame has been sent, report **System copy sent | unconfirmed**.
  A successful tty write cannot confirm the client accepted it.

Document the terminal's clipboard write permission and tmux forwarding
requirements for setups that pass. A tmux copy-mode selection reaching the Mac
clipboard establishes only the tmux-originated path. `set-clipboard external`
can permit that while ignoring writes by applications in a pane. Verify
`set-clipboard on`, the clipboard capability, and the outer terminal's
permission as applicable in a disposable profile. Document the tested
terminal selection or tmux copy-mode fallback where OSC 52 is unavailable.
The editor leaves terminal and tmux settings under the user's control.
[Stage 03 clipboard policy](03_editing_and_mouse.md#mouse-and-clipboard),
[tmux clipboard settings](https://github.com/tmux/tmux/wiki/Clipboard),
[iTerm2 clipboard permissions](https://iterm2.com/documentation-preferences-general.html).

### Empty startup, tabs, and picker

- Show a welcome view if startup resolves with zero tabs. Offer **Open File** and
  **New Scratch** with their effective shortcuts, portable defaults Ctrl+O
  and Ctrl+N, and mouse actions using the existing command routes. Preserve
  startup and recovery order and restore retained scratches as usual. Keep
  each file-open error visible in the resulting session, including when there
  are zero tabs. Startup errors that prevent the interactive session still use
  the existing exit diagnostic. Successful opens, recoveries, pipes, and
  followers use their existing tab flow. A scratch is created by New Scratch
  or an existing explicit workflow such as stdin promotion. The same welcome
  view works after closing the last tab. This supersedes the automatic blank
  scratch in Stages 04–05.
- Give each visible tab a compact visual boundary and distinguish the active
  tab through its shape or text emphasis as well as any color. Preserve
  modified and attention indicators, tab hit targets, and access to overflow
  tabs at 40×8. Keep the header to its existing one row.
- In the Open picker, a single click selects a row. Two consecutive completed
  left clicks on the same current row within a 400 ms interval invoke
  the existing Open action once, entering a directory or opening a file. Keep
  keyboard Enter and the Open button. Clear click history on activation, a
  different target, a drag, focus loss, or a listing or layout change, so an
  old click cannot activate another path.

The user clarified that reStructuredText, CSV, and TSV support means syntax
highlighting. Those additions are deferred until after `0.0.1`.

## Candidate and test fixtures

- Build a release-profile binary from the candidate revision with the pinned Rust
  toolchain, a native C compiler for the bundled grammars, and the checked-in
  Cargo lockfile. Use that binary for terminal, manual, and performance runs.
  Where a test builds its own binary, record its profile and revision. Run
  native macOS and Linux checks with their own binaries.
- Start fixture-based sessions with isolated working, config, state and temporary
  directories, an absolute `XDG_CONFIG_HOME` and `EDE_STATE_DIR`, a supported
  locale, and a usable `TERM`. Use isolated tmux sessions and disposable files,
  producer processes, and fault targets. Use a separate profile for tests that
  intentionally exercise default path resolution or settings changes.
- Prepare UTF-8 fixtures with distinctive markers and byte-for-byte expected
  outputs. Include BOM, mixed LF/CRLF and a final unterminated line, tabs,
  combining text, and wide characters. Add distinct invalid UTF-8, recognized
  unsupported encoding, NUL/binary, and directory targets. Provide a Markdown
  fixture with YAML frontmatter and a bundled-language fence, HTML with
  JavaScript/CSS, a fixture for each bundled grammar, and an unknown fence.
- Use real child processes, PTYs, and files where the contract depends on kernel
  behavior. Use explicit barriers to order races on both sides of the relevant
  commit or ownership change. Use bounded waits for observable state and keep
  failure logs. Run deliberately broken guards only in an isolated copy and
  verify the intended assertion fails after the baseline passes.

## Integrated workflows and failure cases

Each scenario uses normal entry points and compares visible outcomes with
destination or recovered bytes as applicable. The prior stage links supply the
complete contracts and focused cases.

### V01 — Edit and navigate (FR-01, FR-02, FR-03, FR-05)

Open two files with the CLI and one with the picker. Open the picker by
keyboard and through Actions with the mouse. Exercise directory navigation and
file activation on both routes, and open a file by literal path. Switch tabs
with keys and mouse. Place the caret and drag across literal tab characters,
combining text and wide glyphs, autoscroll, copy/cut and paste internally, then
bracketed-paste external text. Resize while selecting and verify hit targets
and positions. Undo and redo. Save and compare exact bytes and newline
conventions. Verify failed opens distinguish binary, unsupported encoding,
invalid UTF-8 and directory input and preserve existing tabs. At 40×8 exercise
Actions, Save/Save As, and dirty Close/Quit prompts with keyboard and mouse.
Cancel Close and Quit and verify the tab remains dirty and protected on the
next attempt.

[Stage 03 editing](03_editing_and_mouse.md#acceptance-criteria)
[Stage 04 tabs and picker](04_tabs_and_file_picker.md#acceptance-criteria)

### V02 — Guarded save and refresh (FR-02, FR-06)

On native macOS and Linux, open two clean files. From a second process,
exercise in-place and rename saves on both the active and inactive tabs. Wait
for tab-specific completion feedback before sending input, switching tabs or
changing focus. Verify the active tab and focus stayed in place, the refreshed
bytes and latest mapped view are correct, and undo restores the previous text
as dirty while redo returns to clean. Repeat dirty and with a held refresh that
started while clean, then edit before releasing it. The dirty revision
survives, the warning remains, and guarded Save preserves the externally
written destination. Also test a missing or invalid replacement that returns to
valid text. At save, force permission or space failure and a conflict
introduced before publish. Check the original destination, retry or Save As,
and preserved unsaved bytes. If publication can have happened, verify the
actual destination and the uncertainty message.

[Stage 02 storage](02_document_storage.md#acceptance-and-evidence)
[Stage 05 refresh](05_scratch_recovery.md#automatic-refresh-of-named-tabs)

### V03 — Recover scratches (FR-10, FR-09)

Use multiple scratch tabs with different selections/views and an active
scratch. Observe the default five-second checkpoint schedule and apply a
shorter interval through Settings. Quit after further edits before the next
periodic write. Relaunch and check text, tab order, active scratch, view state,
and fresh undo history. Repeat after saving one scratch as a named file and
explicitly discarding another. For crash recovery, publish checkpoint A, hold
checkpoint B before catalog publication and kill the editor. Verify recovery of
A without mixed content. Interrupt after publication as a separate case and
check the newly committed catalog. Inject checkpoint storage failure during
Quit and verify that the editor offers recovery and preserves current text
through retry or Save As. Reuse two-owner tests to verify a live owner cannot
be claimed by another instance.

[Stage 05 recovery](05_scratch_recovery.md#acceptance-criteria-and-test-plan)
[Stage 09 settings](09_configuration_and_keybindings.md#acceptance-criteria)

### V04 — Search and replace (FR-02, FR-06, FR-07, FR-08)

Put literal markers across storage boundaries, in a scratch with unsaved edits,
in a named tab, and in active stdin. Use both keyboard and mouse to open the
search controls, change current/all-tab scope, case and whole-word options, and
run or cancel an action. Verify wrap feedback and exact match positions. Use
Replace Next and Replace All and verify the reported processed and changed
counts against actual edits. Check that Current replacement is disabled in
active stdin and that the All Open summary counts the read-only tabs it
skipped. Undo once in each changed document and verify unchanged disk bytes
before saving. Cancel at a controlled boundary between documents and exercise
a failure there. Require the summary and actual edits to agree. After stdin
EOF, include the promoted scratch in replacement.

[Stage 06 search](06_search_and_replace.md#acceptance-criteria-and-validation-plan)
[Stage 08 streams](08_pipes_and_follow.md#acceptance-criteria)

### V05 — Highlighting (FR-04, FR-06, FR-09)

Verify offline detection and visible styling for every bundled language and the
embedded fixtures, including a neutral code body and styled delimiters and
label for an unknown fence. Change language and reload a custom theme,
association and query. Exercise logical sizes at and above 20 MiB and lines at
and above 64 KiB, including an offscreen long line. Isolate one limit at a
time. Use a controlled analysis timeout to test the 250 ms default, stale
result rejection, visible whole-document fallback, and continued edit/search.
Apply a lower limit through general settings, verify it affects eligibility,
then restore the default. Use focused tests for worker budgets and limit
equality, with candidate TUI checks for the visible fallback and continued
editing.

[Stage 07 highlighting](07_syntax_highlighting.md#acceptance-criteria-and-validation-plan)
[Stage 09 settings](09_configuration_and_keybindings.md#acceptance-criteria)

### V06 — Pipes and file following (FR-01, FR-07, FR-08, FR-10)

Gate a producer so partial text is visible before EOF. Split a UTF-8 scalar and
CRLF across writes. Pause viewing, observe continued ingestion, search retained
history, and resume. Attempts to edit, save or replace active text leave it
unchanged. On valid EOF the same tab becomes editable. Make an edit, use Save
As, and verify exact bytes. In a separate run Quit without Save As and recover
the promoted scratch. Use `--follow` for append, rename/recreate, and observed
truncation, checking boundary status, retained/exported bytes and untouched
sources. Induce quota/space or producer failure and verify the retained prefix
and export. In separate trials Close the active stream while a companion tab is
open and Quit with active input. After confirmation both release the pipe
reader promptly. After Close the companion tab still accepts input in editor
modes. After Quit the shell terminal modes are restored. Cover idle and busy
input. Redirect interactive stdout or remove the controlling tty in focused
process tests. Verify that startup fails before document data are consumed and
that no controls reach stdout.

[Stage 08 streams](08_pipes_and_follow.md#acceptance-criteria)

### V07 — Configure and recover (FR-01, FR-03, FR-09, FR-10)

Remap Save and a surface-specific action using keyboard and mouse in Settings,
check effective shortcuts and relaunch. Edit the TOML and reload to verify
agreement with the UI. Exercise a conflict and a malformed file, confirming
last-good settings on reload and defaults plus a diagnostic on initial load.
Introduce a stale Settings draft through an external write. Verify Save
Settings preserves the external file and reload allows a fresh save. Launch
with `--safe-config` and confirm portable controls, normal editing and recovery,
and unchanged user configuration. Verify discoverability of commands and
effective bindings and coverage of all remappable command IDs against the
catalog.

[Stage 09 settings](09_configuration_and_keybindings.md#acceptance-criteria)

### V08 — Clipboard modes (FR-03 and FR-09)

Use a disposable config to cycle and save modes, relaunch, reload, and restore
Both. Verify a changed or reset draft takes effect only after save. Test a
malformed value, the legacy booleans, and safe startup. Verify that a System
copy doesn't replace existing private text, the terminal Paste action still
works, and a blocked Cut emits no frame or deletion. A real PTY must emit no OSC 52 write in Internal
mode, on an empty selection, or after a failed Copy or Cut. In enabled mode
decode the frame and compare the bytes with a selection containing a wide
character and newline. Check internal-only, request-sent, and skipped or
failed status messages. Copy from active stdin through its controlling tty.
Cut there is rejected and emits nothing. Check that Cut on an editable tab
publishes only after deletion commits. Simulate a rejected write before
transmission and verify internal Paste still recovers the text. Inject failure
after the OSC introducer and verify abort/recovery or terminal-loss handling
before unrelated output. Exercise the
payload limit at and above the boundary. Use controlled ordering to complete
a Copy started in Internal after selecting Both. Then hold a Both Copy while
switching to Internal and back. Neither held command may send a frame.
Exercise pending-write revocation on newer Copy and suspension. Run the client
clipboard sequence below with the actual frontend and tmux/SSH path.

### V09 — Empty session, tab strip, and picker (FR-03, FR-05, FR-10)

Launch with an empty state profile and no paths. Verify zero tabs, visible
Open/New actions, and no scratch created on Quit. Repeat with failed CLI
opens and confirm their errors in the welcome view. Relaunch with retained
scratches and confirm their order and view are restored without an extra
scratch. Repeat with failed CLI opens alongside successful scratch recovery.
Verify the errors stay visible and the restored scratch is active. Open and
create from the welcome screen with keyboard and mouse, using both defaults
and remapped shortcuts.
Close the last tab and repeat. At regular size and 40×8, switch tabs, inspect
dirty/error indicators, and access tabs outside the strip. In the Open picker
verify single-click selection, activation after a timed double-click of the
same file or directory, and keyboard activation. Spaced clicks select only.
A third click cannot activate again or carry a pending click into the next
listing. Verify no accidental activation across a resize, drag, or replacement
listing.

For changed data-protection guards, add or extend the focused regression first,
show the expected pre-fix failure, then record the passing test and the
deliberate guard break and assertion failure. Reuse existing focused tests where
they reproduce the regression. A review of source alone is insufficient for a
filesystem race or real terminal behavior.

## Terminal and transport checks

FR-01 and FR-03 require real frontend evidence in addition to executable PTY
tests. Use the following proposed reference matrix. A direct path has no tmux
layer. For remote tmux rows, tmux runs on the editor host. Record any additional
client-side nesting separately if exercised or documented as supported.

| Row | Client → editor | Required run |
| --- | --- | --- |
| T10-1 | macOS iTerm2 → local macOS | Base sequence in the portable profile. Repeat it in the enhanced profile during the final macOS Cmd check. |
| T10-2 | macOS iTerm2 → local tmux → macOS | Base sequence and tmux sequence in the portable profile. Test enhanced delivery where supported during the final macOS Cmd check. |
| T10-3 | Native Linux graphical terminal → local Linux | Base sequence and portable operation. Reference frontend may be xterm or GNOME Terminal, with actual version recorded. |
| T10-4 | Same Linux frontend → local tmux → Linux | Base sequence and tmux sequence. |
| T10-5 | macOS terminal → SSH → Linux | Base sequence and SSH disconnect/reconnect sequence using an allocated remote PTY. |
| T10-6 | macOS terminal → SSH → remote tmux → Linux | Base sequence, tmux sequence and fresh SSH connection followed by reattach to the same editor. |
| T10-7 | macOS terminal → mosh → Linux | Base sequence and interruption/resumption sequence, without tmux. |
| T10-8 | macOS terminal → mosh → remote tmux → Linux | Base, tmux and interruption/resumption sequences. |
| T10-9 | macOS terminal → Eternal Terminal → Linux | Base sequence and interruption/resumption sequence, without tmux. |
| T10-10 | macOS terminal → Eternal Terminal → remote tmux → Linux | Base, tmux and interruption/resumption sequences. |

Use profiles representative of intended operation. For each row record client
and editor OS/architecture, terminal application and version, terminal profile,
`TERM`, negotiated keyboard mode or portable fallback, tmux settings/version,
transport client/server versions as applicable, and any required upstream
settings. Record actual dimensions and the method and length of any connection
interruption. A version tested on one path establishes evidence for that path.

1. **Base sequence.** Enable IXON in the disposable shell before recording
   `stty -g`. Open a disposable text file and scratch in a window at least
   40×8. Verify portable editing, navigation, Ctrl+S saving with flow control
   disabled in ede, Find and Quit. Resize below 40×8 with unsaved text, then
   restore a supported size and check redraw, view state, and access to the
   unsaved-change prompt. Click, drag, and scroll
   after resize. Compare internal copy/paste with terminal clipboard paste and
   check saved bytes. Pause ede mouse capture, select using the terminal or tmux
   copy mode, then turn capture back on with Actions. Verify that keyboard
   actions and the internal clipboard remain usable if upstream clipboard or
   mouse delivery is unavailable. Suspend with Actions and repeat input after
   `fg` where job control is available. Verify the shell's exact termios,
   normal echo, cursor behavior, and release of mouse, paste and acquired
   keyboard modes on suspension and Quit. On native PTYs also check recoverable
   startup/exit errors and signal cleanup.
2. **tmux sequence.** With a dirty fixture and a selection, detach, change the
   window size and reattach to the same session. Check the retained document,
   dirty state, refreshed geometry, current input/capture mode, and save
   bytes. Exercise native tmux copy mode and return to ede. Use a disposable
   tmux server so the test cannot close a working session.
3. **SSH disconnect/reconnect.** Save marked named-file bytes and acknowledge a
   scratch checkpoint on the editor host. Add unsaved markers to both. Disrupt
   the disposable connection and establish a fresh SSH connection with a new
   PTY size. In direct SSH, verify working input and clean modes in the new
   shell and the last saved named-file bytes. Observe the original PTY and
   process from a companion shell with a bounded wait. If it outlives the
   interruption, record that behavior, then force definitive connection loss
   by ending the isolated SSH client and verify the original editor exits
   once its PTY is torn down. If transport teardown is inconclusive, exercise
   definitive tty loss separately on a controlled PTY. Once the old owner
   retires, launch ede in the fresh connection and verify the last completed
   scratch checkpoint. Observe checkpoint status or use a controlled pause in
   checkpoint publication to identify the exact recoverable revision. In the
   tmux row, reattach to the same editor through the fresh connection and
   verify its unsaved markers, current geometry and ability to save.
   Terminal-loss behavior follows
   [Stage 08](08_pipes_and_follow.md#close-quit-suspension-and-terminal-loss).
4. **Interruption/resumption.** During a live mosh or Eternal Terminal session
   with unsaved text, interrupt and restore connectivity without ending the
   editor's remote session. Change the client size while disconnected. On
   resumption verify the same editor resumes with its dirty text, redraws at
   the current size and handles keys, hit tests and paste. Save and compare
   exact bytes. In the tmux rows also exercise explicit detach/reattach. If the
   session fails to resume, record the failure and whether a fresh connection
   can recover the tmux session or the last scratch checkpoint.
5. **Client clipboard.** Try an ordinary Copy in
   Terminal.app over direct SSH to Ubuntu and in iTerm2 over direct SSH and
   through the reported tmux topology. Include an iTerm2 local baseline.
   Use distinct markers for the existing macOS clipboard and the selection
   in ede. In Internal mode, paste into a Mac GUI app to verify its old
   clipboard and use Ctrl+V in ede to verify the new internal copy. In Both,
   verify exact pasted text in the Mac app on a supported
   direct SSH path and a supported tmux path, including a committed Cut and
   a selection at the documented limit. Use the terminal Paste action to
   send a new Mac marker to ede and verify that it inserts once. Where the
   path discards OSC 52, verify internal Paste and a terminal-selection or
   tmux copy-mode route. Record the actual outcome before listing a path or
   payload size as supported.

A PTY that injects mouse or keyboard sequences can establish ede's decoding
and cleanup behavior. A passing row in this matrix requires the actual frontend
and transport and evidence for its stated actions. Mark upstream limitations
at the affected action, including the verified fallback. Record inaccessible
rows as blocked and resolve them or obtain explicit user approval for the
coverage change before stage acceptance.

## Performance and resource checks

Reuse the release probes listed in the [README](../../README.md#development-and-benchmarks).
Run the executable through a real PTY for measured local input and rendering.
Use storage and coordinator probes for phase timing and managed allocations.
Keep per-run output and label the scope of each observation.

### B10-1 — 1 GiB baseline

Run `large_file_interactive_benchmark` with `EDE_BENCH_MIB=1024` and
`EDE_BENCH_CORPUS=ordinary`, `dense` and `long`. Use a host or VM with about 8
GiB usable RAM, or an explicit effective 8 GiB limit with documented host/cache
conditions. Verify first text, editable Ready, tab switch, beginning/middle/end
editing, and save while accepting another edit. Check saved output and
dirty-state protection. Use `storage_bench` for undo/redo, full output
comparison, copy-path costs and clone results where available.

### B10-2 — Larger than RAM

Run a fully allocated ordinary-text file strictly larger than the RAM assigned
to a dedicated host or VM. Record usable RAM and disable swap in that test
environment. Use the interactive file probe or an equivalent executable PTY run
with an explicit MiB size. Verify open, navigation, beginning/middle/end edits
and save while preserving newer edits. Run literal Find, Replace All and Undo
using `large_search_and_replace_benchmark` or the same-size marked corpus with
a known match count. The opt-in search probe accepts
`EDE_BENCH_MARKER_STRIDE_MIB` to exercise the same file size within the
existing Replace All undo budget. Compare saved output in bounded chunks
against independently generated expected bytes, covering the entire edited and
replaced files including unchanged ranges. A cgroup-only run on a larger host
addresses the separate constrained-editor check.

### B10-3 — Constrained editor

On Linux, run the 1 GiB ordinary, dense and long interactive and search/replace
probes with the existing 256 MiB cgroup and zero swap. Check the actual applied
limits, completion without OOM, byte integrity and responsiveness. Include the
1 GiB recovery/refresh probes and the existing large highlighting/search
overlap. Verify fixtures and working files use disk-backed storage, not tmpfs.

### B10-4 — Sustained streams

Use `sustained_piped_capture_benchmark` with 1 GiB of generated ordinary, dense
and long-line text. Time first text with a gated producer, keep a companion tab
editable while capture runs, search the retained text before closing the
producer and time EOF adoption separately. Run the 8 GiB baseline and a Linux
256 MiB swap-disabled run. Cover sustained capture, raw partial input and
retention/error behavior with V06 in addition to this measurement.

Run the 1 GiB interactive, search and stream probes on both native platforms,
with the 8 GiB baseline and swapless checks documented separately. Record an
unavailable required environment as blocked. Include both first-pass and
repeated measurements for B10-1. State how cache conditions were obtained.
Just-generated fixtures are warm. Label uncontrolled cache conditions as such.
Use fully allocated valid text and bounded fixture generation and
verification. Provide enough filesystem space for source,
snapshots, indexes, edit/recovery storage, and output. Record snapshot clone
versus copy where observed, and exercise the forced-copy path.

- **Local goals.** First usable text within 1 second and ordinary input response
  within 50 ms on the baseline. First display runs from launch until a document
  marker is visible in the PTY. Record editable Ready, index, Save and
  search/replace completion separately. Ordinary input runs from the time an
  event is available at the PTY to its first visible effect. Use the interactive
  probe's 40 ordinary edit samples per run and record sample count, p50, p95,
  and maximum. Sample a ready companion tab during sustained ingestion and
  other long work and confirm it overlaps the sample interval. If ingestion
  completes before the samples, repeat with a controlled producer schedule
  and record its pacing separately from the full-speed throughput run.
  During indexing, full search, Replace All preparation and Save staging, verify
  visible progress and exercise navigation or editing in a Ready companion tab.
  Hold work at a known pre-commit boundary where the existing probes cannot
  prove overlap. Exercise Escape when cancellation is safe and verify that
  unpublished changes leave original bytes intact. Use these controlled runs
  to establish ordering separately from unmodified throughput runs.
  Compare maxima with the goal and report individual misses. Include
  representative navigation and mouse event-to-visible samples alongside
  typing. A change to an agreed goal requires explicit user approval.
- **Resource evidence.** Record CPU, architecture, OS, RAM, swap, limits and
  filesystem/storage for all results. Record corpus shape and allocated/actual
  bytes, terminal geometry, cache state, build profile, workload and sample
  cadence. Collect editor kernel-peak RSS where available and label sampled RSS
  separately. Record cgroup peak (including charged cache), CPU time, FD
  high-water, managed cache/queue/index/history charges relevant to the probe,
  and logical/allocated working-file high-water for snapshots, indexes,
  recovery and stream spools as applicable. Compare bounded resources against
  the existing stage budgets and explain any overrun. Report clone-sharing
  caveats with allocated disk counts. Evidence can come from complementary
  probes with their scope recorded.
- **Remote timing.** For each transport record connection conditions and a
  representative input/resize response at the client. Keep client-visible
  timings distinct from editor processing and PTY timings. Use client and
  host probes separately when available and state what the clocks measure.
  Keep the 1 s and 50 ms checks on the local baseline.

## Final terminal check — macOS Cmd shortcuts

Run physical Cmd shortcut checks after the integrated workflows, portable
terminal and clipboard checks, and performance runs. Verify the portable Ctrl
shortcuts in the earlier terminal checks with enhancements disabled.

- On a named, versioned enhanced macOS profile, demonstrate at least one
  direct Cmd/Super action through a physical shortcut. Exercise one Cmd-to-Ctrl
  mapping end to end in an isolated profile and document its exact settings.
- For each advertised Cmd alias, use the physical shortcut on the claimed
  path and check its corresponding portable behavior, including modifier and
  focus distinctions. Include Terminal.app → SSH → Ubuntu and the reported
  iTerm2 path through tmux. Record the tmux server location. Try Cmd+S with a
  dirty fixture and check saved bytes. If a delivered Cmd event fails to
  dispatch, reproduce and fix it.
- Record each shortcut the OS, terminal, or tmux handles upstream as a
  limitation of that path and document its tested Ctrl, Actions, or profile
  mapping fallback. For example, Terminal.app can handle Cmd+S by opening its
  text export dialog. Verify Ctrl+S or a tested mapping saves the file in ede.
  The user accepts upstream interception when a working fallback is available.
  Terminal and tmux settings remain under the user's control.
  [Apple Terminal keyboard shortcuts](https://support.apple.com/en-mn/guide/terminal/trmlshtcts/mac),
  [iTerm2 profile key mappings](https://iterm2.com/documentation-preferences-profiles-keys.html).

## Build and completion gates

1. On native macOS and Linux run `cargo fmt --all -- --check`,
   `cargo clippy --workspace --all-targets --locked -- -D warnings`, and
   `cargo test --workspace --locked` with the pinned toolchain. Run the opted-in
   benchmark and tmux cases explicitly. Record OS-specific skips and both CI
   job links with the tested SHA. The current draft-PR workflow provisions only
   macOS on a draft event. Obtain the full matrix for the candidate on the
   implementation PR's ready-for-review run or a subsequent update to that PR.
2. For every required check, record **pass**, **fail**, **blocked**, or a
   user-approved disposition. Keep earlier misses and the result of any rerun.
   Record broken behavior with the exact environment, trigger, and observed
   outcome. Fix regressions and rerun affected checks. If the requirement or
   approved coverage must change, record the user's decision and update the
   overview and affected stage contract. Missing equipment and an earlier
   stage deferral leave the Stage 10 check open.
3. Update the README with locked source-build prerequisites, first-run edits,
   saves and scratch behavior, pipe/follow examples, configuration paths and
   safe startup, portable controls, clipboard modes and fallbacks, known limitations,
   and the tested terminal/version/profile/transport combinations. Link to the
   evidence here. Verify the commands in a fresh isolated checkout and profile
   on each OS and the documented terminal-specific instructions in their
   respective matrix rows.
4. Use development prereleases during work as needed. Set the release candidate
   Cargo package version and corresponding lockfile entry to `0.0.1`. Verify
   locked release build, `ede --version`, the version shown by `--help`, and
   README version against that candidate. Record evidence against the exact
   final executable inputs. Rerun checks affected by later changes before
   recording completion.
5. Have an independent reviewer follow [REVIEW.md](../../REVIEW.md) against
   the candidate and feature base. Supply acceptance traces, test output,
   benchmark conditions, and guard-mutation evidence when applicable. Fix
   findings and have the reviewer inspect material fixes.

## Acceptance criteria

| ID | Completion evidence |
| --- | --- |
| MVP-01 | All V01–V07 observable outcomes have candidate evidence and trace back to FR-01–FR-10. Refresh of active and inactive clean tabs and protection of dirty revisions and external bytes are verified before release. |
| MVP-02 | Required T10 matrix rows pass or have an explicit user-approved coverage disposition. Native, remote, and PTY results identify the actual path and verified fallbacks. |
| MVP-03 | B10-1 through B10-4 have results with measured latency, memory and disk behavior. The 1 GiB / 8 GiB baseline and larger-than-RAM runs are evidenced independently. Missed goals or exceeded budgets are fixed and rechecked or resolved by user-approved requirement changes. |
| MVP-04 | Native locked checks, opted-in regressions, and independent review for the candidate are recorded. Required failure and controlled ordering evidence is present. |
| MVP-05 | A fresh source build and documented workflows work as described, and Cargo metadata, the executable and README agree on `0.0.1`. The validation record states supported paths, observed limitations, and approved dispositions. |
| MVP-06 | Record candidate evidence for V08, V09, and the client clipboard sequence. The README covers clipboard modes, the default, tested paths, and fallbacks. |

## Validation record

Implementation and automated validation are recorded below for the
`0.0.1-dev.10` build inputs in [draft PR #10](https://github.com/rapidclock/ede/pull/10).
Release acceptance is **open**. No coverage deferral has been approved.

### Design and pre-implementation baseline

- Independent design review against `dfacfe2` found gaps in terminal and mouse
  coverage, stream Close and Quit, refresh, syntax fallback, larger-than-RAM
  search, and validation of progress during overlapping work. Follow-up review
  of the material fixes found no remaining actionable findings.
- On macOS 27.2 arm64 with Rust 1.99.1, the merged Stage 09 baseline passed
  `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets --offline --locked -- -D warnings`,
  and `cargo test --workspace --offline --locked` with a 256-descriptor soft
  limit. All 313 enabled tests passed. Ten opt-in tests were skipped. The
  passing run used normal terminal permissions after the sandbox denied access
  to the controlling tty in seven pipe tests.
- Independent review of the proposed pre-release changes against `77157ae`
  checked clipboard authorization, incomplete terminal writes, empty-session
  errors, picker click ordering, and applicability of the new checks. Follow-up
  review of the fixes found no remaining actionable findings. Formatting,
  Clippy, and the 313 enabled macOS baseline tests passed again. Real clipboard
  and shortcut behavior remains to be verified on the reported terminal paths.

### Candidate builds and workflows

Rust 1.99.1 locked builds passed on macOS 27.2 (26B123, M1 Max, arm64, 32 GiB),
Ubuntu 26.10 (Linux 7.3.0-10-generic, arm64 VM, 9,395 MiB), and Debian 12
(Linux 5.15.49-linuxkit-pr, arm64 guest). Formatting and workspace Clippy
passed with warnings denied. Workspace tests passed with a 256-descriptor
soft limit. There were 325 enabled tests on macOS, 327 on each Linux build,
and ten opt-in tests excluded in each default run. Release-profile
`ede --version` and `ede --help` reported `0.0.1-dev.10` on macOS and Ubuntu.

| Workflow | Candidate evidence |
| --- | --- |
| V01 editing and navigation | Real PTY tests cover portable editing and paste, Unicode mouse drag and copy/paste, resize, picker and tab activation, and narrow-window prompt protection. Document tests check saved bytes, newline conventions and rejected inputs. |
| V02 guarded save and refresh | Document fault hooks cover conflicts, publication and uncertainty. Editor and real PTY tests cover active and inactive updates, missing and invalid replacements returning to valid text, dirty-tab protection and stale prepared refreshes. |
| V03 scratch recovery | Real PTY tests cover restart, ordering, views, configurable checkpoints and retained content. Recovery tests control catalog publication and concurrent owners. |
| V04 search and replace | Real PTY tests exercise Find, Replace All and Undo. Coordinator tests exercise cancellation and partial failure across tabs, read-only live input, and changes after stream promotion. |
| V05 highlighting | Native suites cover bundled grammars, embedded and unknown fences, reload and last-good behavior, timeout and stale results, and threshold equality. Real PTY tests exercise visible fallback and refresh recovery. |
| V06 pipes and follow | Process and real PTY tests cover partial input, capture and EOF promotion, read-only guards, follow rotation and truncation, Close and Quit with active input, and rejected tty or stdout configurations. |
| V07 configuration | Settings and real PTY tests cover remapping, effective bindings, malformed reload and fallback, stale Settings save, and safe-config editing and recovery. |
| V08 clipboard | Real PTY tests cover exact UTF-8 OSC 52 bytes, internal-only mode, committed Cut, live pipe copy, Settings save and safe mode, and the 8 KiB boundary with a paced reader. Controlled checks exercise authorization, revocation, write failure and suppression of output after an incomplete frame. |
| V09 welcome, tabs and picker | Real PTY and reducer tests cover explicit initial scratch creation, zero tabs after close, visible CLI errors and recovery order, tab styles and overflow, and double-click activation and invalidation across listing, drag and geometry changes. |

The welcome, CLI error, picker, clipboard and paced-terminal regressions first
failed before their fixes. Holding the PTY reader for 150 ms reproduced a
process exit when one deadline covered both initial admission and partial
writes. With separate deadlines the maximum selection survived the pause.
Five isolated guard mutations produced the intended assertion failures for
stale Cut retirement, clipboard configuration revocation, output after
incomplete OSC, writer admission after revocation, and slot recovery after
startup cancellation. Independent review followed [REVIEW.md](../../REVIEW.md)
against stage design head `e42ad16` and feature baseline `dfacfe2`. Findings
were fixed and reviewed again. The reviewer also ran a 17 MiB search fixture
with a marker in the final partial stride.

### Terminal, transport and client clipboard evidence

Portable PTY checks used `xterm-256color` at 80×24. Isolated local tmux editing
and job-control suspension tests passed on macOS and Debian. The frontend and
transport matrix remains open.

| Row | Result |
| --- | --- |
| T10-1 | **Partial**. Manual local iTerm2 copy and paste passed on retry as recorded below. The remaining base sequence and physical Cmd checks are open. |
| T10-2 | **Partial**. Native PTY and isolated local tmux input and suspension passed on macOS. iTerm2 with local tmux and the physical Cmd checks are open. |
| T10-3 | **Partial**. XTerm 379 under Xvfb on Debian copied the marker into the X11 system clipboard, resized, saved exact bytes with Ctrl+S and restored `stty -g` on Quit. The remaining base sequence is open. |
| T10-4 | **Partial**. XTerm 379 with tmux 3.3a forwarded the exact marker to X11 PRIMARY with `set-clipboard on` and `xterm*:clipboard`. CLIPBOARD retained its previous value. Internal Ctrl+V worked, native tmux copy mode retained the exact marker in its buffer, and resize, Ctrl+S, exact saved bytes and tty restoration passed. The remainder of the graphical sequence is open. |
| T10-5 | **Partial**. OpenSSH 10.5p1 on macOS allocated an Ubuntu PTY. Exact OSC 52 bytes crossed SSH. Ctrl+S saved exact bytes and Quit restored terminal modes. Killing a separate SSH client retired its editor. A fresh 96×32 PTY retained the last saved named-file bytes and reopened the last acknowledged scratch checkpoint, then restored its original termios on exit. Manual clipboard results by client are below. The remaining base sequence is open. |
| T10-6 | **Partial**. Isolated remote tmux 3.7b forwarded the OSC 52 payload with `set-clipboard on` and `xterm*:clipboard`. A fresh SSH connection reattached at 96×32, retained the unsaved marker, saved exact bytes and restored the inner tty. Manual iTerm2 clipboard checks passed as recorded below. The remaining base and tmux actions are open. |
| T10-7 and T10-8 | **Partial**. Mosh 1.4.0 on macOS arm64 and Ubuntu arm64 resumed after a 12 s bidirectional UDP drop in an isolated loopback relay, both directly and through isolated tmux 3.7b. The client PTY changed from 80×24 to 96×32 during the interruption. The same editor retained the unsaved marker, accepted bracketed paste, saved exact bytes and restored the inner tty. Graphical frontend and remaining base actions are open. |
| T10-9 and T10-10 | **Partial**. Eternal Terminal 7.1.1 on macOS arm64 used a checksum-verified Debian arm64 server package on Ubuntu after the Ubuntu package endpoint returned HTTP 503. The server and TCP relay were loopback-only, bridged by an isolated SSH tunnel. After a 12 s TCP disconnect and resize from 80×24 to 96×32, both the direct and remote tmux 3.7b paths retained unsaved text, accepted bracketed paste, saved exact bytes and restored the inner tty. Graphical frontend and remaining base actions are open. |

Manual Mac clipboard checks used the disposable fixture with system clipboard
copy enabled. These results cover copy and paste only.

| Path | Observed result |
| --- | --- |
| iTerm2 → local macOS, no tmux | Copy to TextEdit and paste from TextEdit via iTerm2 Edit → Paste passed on retry. The first copy attempt left the launcher command on the Mac clipboard. Cause unknown. |
| iTerm2 → direct SSH → Ubuntu | Copy to TextEdit and paste from TextEdit passed. |
| iTerm2 → SSH → isolated tmux on Ubuntu | Copy to TextEdit and paste from TextEdit passed. |
| Apple Terminal → direct SSH → Ubuntu | **Copy to Mac failed.** ede showed `Clipboard request sent`, but TextEdit pasted the previous launcher command. Paste from TextEdit into ede passed via both Cmd+V and Edit → Paste. |

A standalone OSC 52 write in the local Apple Terminal shell also failed to
replace the Mac clipboard. This reproduced the failure without ede, SSH or
tmux. An initial mouse-copy attempt with the local fixture rang the bell on
Cmd+C and left the previous launcher command on the clipboard. After disabling
Apple Terminal's Allow Mouse Reporting, mouse selection and Cmd+C copied the
visible `ede-stage10-copy-界` text to TextEdit. The existing ede selection
highlight remained visible in Terminal, and the pasted text retained the
background color. Terminal's Copy Plain Text shortcut (Option+Shift+Cmd+C)
passed locally without carrying over the background. The user also confirmed
copy and paste with Cmd+C and Cmd+V using Terminal selection. With mouse
reporting disabled, clicking ede's Quit button stopped working; re-enable
reporting to restore editor mouse input, or use Ctrl+Q to quit. The editor's
Ctrl+C failure remains open. Investigate whether Apple Terminal supports a way
for a remote application to set the Mac clipboard. Apple's Terminal guide
documents terminal selection and copy shortcuts but no application clipboard
write setting. The README documents the verified mouse-copy fallback. Any MVP
exception for direct editor copy requires explicit acceptance. The
intermittent local iTerm2 failure also remains open.

The user found it confusing that ede's Ctrl+V uses its internal clipboard
while the terminal's Paste command uses the Mac clipboard. They approved the
three clipboard modes above, with Both as the default.

Physical Cmd shortcut checks, maximum clipboard payload by path and the
remaining frontend workflows are still open.

The follow-up changes the status shown after OSC 52 transmission to
`System copy sent | unconfirmed`. The focused PTY test first failed on the
old status and then passed with the new wording and the same UTF-8 frame.
The paced-reader PTY test also passed with the new wording. Native macOS
validation with an isolated config passed all 334 enabled tests. Formatting
and Clippy passed. The README
documents the existing F2 Actions mouse capture toggle as an alternative
to changing the Terminal setting.

The V08 clipboard mode follow-up first failed the default Both PTY check
because the prior default sent no frame. It then passed with an exact frame
and a private paste. The same PTY saved System and Internal through Settings,
verified that System Copy left the prior private text available after returning
to Internal, and exercised terminal Paste, the System Cut guard, and
`--safe-config`. A separate PTY verified System's 8 KiB rejection and no
frame for an oversized selection. Parser tests cover all modes, malformed
values, conflicting keys, and legacy compatibility. An isolated deliberate
break in the System Cut guard failed the intended assertion and showed the
deleted text was no longer in the document. The independent reviewer
rechecked the spool retention, oversized selection preflight, and footer
fixes. Full native macOS validation passed 336 enabled tests with ten ignored.
Formatting and Clippy passed. The Apple Terminal OSC 52 failure remains open.

### Performance and resources

The release probes used 80×24 PTYs and warm, just-generated allocated files.
The Debian guest had 8,142,192 KiB RAM, three test CPUs and disk-backed ext4
at `/verify`. The baseline and larger-than-RAM containers had
`memory.max=8589934592`. The constrained containers had
`memory.max=268435456`. All Linux benchmark containers had
`memory.swap.max=0`. macOS runs used APFS.

- **B10-1 measured probes passed.** Two runs each of 1 GiB ordinary, dense and
  long corpora verified all saved bytes. On macOS first text was 56.340–64.935 ms
  and Ready was 356–3,991 ms. In each 40-sample run, p50 was 12.562–20.622 ms,
  p95 was 19.670–35.472 ms and maximum was at most 38.269 ms. Peak RSS reached
  154,368 KiB. On Linux first text was 27.287–31.838 ms and Ready was
  1,521–5,361 ms. The p50 was 12.741–14.501 ms and p95 was 16.128–28.213 ms,
  with maximum at most 29.954 ms and peak RSS at most 137,056 KiB. Linux
  forced-copy storage runs verified output and Undo/Redo, taking 2.695–6.680
  CPU seconds and 1,886–2,163 ms to save. The measured maximum managed cache
  was 67,174,400 bytes with about 1 GiB working text and 196,872 bytes working
  metadata. The 1 GiB Find and Replace All probes passed with maximum live
  edit latency of 28.533 ms on macOS and 29.921 ms on Linux.
- **B10-2 editing passed.** An 8,589,934,592-byte file exceeded guest physical
  RAM with swap disabled. First text appeared in 30.732 ms and Ready at
  22.266 s. The 40 samples measured p50 15.208 ms, p95 30.715 ms, and maximum
  34.560 ms. Peak RSS was 72,236 KiB. The 160.40-second run compared every saved
  byte and confirmed a later edit still required a quit decision. A search
  run with 8,192 markers reached the existing undo budget and replacement
  was refused. With 1,024 matches spaced 8 MiB apart, Replace All completed
  and guarded Save exceeded the probe's original 120-second wait. The rerun
  passed with the longer deadline. Find took 10.589 s, replacement 46.978 s
  and Save 133.835 s. All saved bytes matched, Undo restored the first marker
  and marked the tab dirty, and cancellation was observed. The 40 input
  samples measured p50 13.036 ms, p95 26.910 ms and maximum 30.661 ms.
  Peak RSS was 71,916 KiB and the cgroup reported zero OOM events.
- **B10-3 measured probes passed.** Under 256 MiB with zero swap, 1 GiB
  editing, Find/Replace All, streams, recovery/refresh with a 64 MiB scratch,
  and highlighting/search overlap completed. The highest edit and search
  input latencies were 28.936 ms and 16.786 ms. Peak editor RSS for edits was
  137,040 KiB. The refresh storage probe measured 4,295,438,336 allocated bytes
  including the fixture and working files and 979,200 bytes of managed refresh
  buffers. The scratch checkpoint used 67,112,960 allocated bytes.
- **B10-4 measured probes passed.** 1 GiB gated producer and companion-tab
  tests passed for all corpora. Maximum companion latency was 40.982 ms on
  macOS, 18.503 ms in the Linux baseline and 18.784 ms under 256 MiB.
  Full-speed capture across the nine runs was 154–544 MiB/s and EOF adoption
  was 20.915–30.111 ms. Linux observed 25–28 open descriptors at high water.

Client-visible timing, standalone mouse and navigation latency samples, and
complete per-workload allocation and peak measurements remain open.

### Review fixes verified on `01c3bb0`

A further independent review of `a76fa83` reproduced six findings. The
regressions failed before the fixes below and passed afterward.

| Finding | Fix and regression evidence |
| --- | --- |
| Duplicate recovery identity could lose scratch edits on normal Quit | Opening a retained scratch removes its cached startup entry after admission. `manual_recovery_and_startup_refill_keep_one_scratch_owner` fills 64 startup slots, manually opens an overflow scratch, cancels a loading CLI tab, checks one owner per identity and verifies retained bytes after Quit. Removing the new guard in an isolated copy fails the duplicate-owner assertion with two owners. |
| An overtaken Copy could transmit after its transfer left the pending slot | A shared generation remains attached to the transfer. Every first-byte attempt checks it under the same mutex as internal Copy/Cut commit, with readiness waits outside the mutex. `newer_oversized_copy_revokes_taken_transfer` forces an unsuccessful first attempt, then completes an 8,193-byte internal copy before retry. Disabling the generation check in isolation fails with Sent instead of Unavailable. `clipboard_first_write_serializes_authority_until_callback_returns` holds a synchronized writer callback. Releasing the mutex before the write in isolation fails its intended assertion. |
| Double-clicking a scrolled picker moved the target under the pointer | The picker retains its viewport while the selected row remains visible. `picker_double_click_keeps_scrolled_rows_stationary` uses real PTY input to open the clicked file after keyboard scrolling. |
| A release on another picker row left a prior click armed | A mismatched press/release target clears click history. `picker_mismatched_release_interrupts_double_click` confirms the interrupted sequence leaves the picker open. |
| SIGTERM during an incomplete clipboard write returned exit status 1 | The runtime preserves pending external signal status, and the poisoned-output wrapper preserves that result through cleanup. `clipboard_backpressure_preserves_signal_exit_status` holds a real PTY reader after the OSC introducer and checks SIGINT, SIGTERM and SIGHUP exit statuses and exact termios restoration. |
| A newer layout acknowledgment hid the actual clipboard delivery outcome | Clipboard feedback uses the originating Copy/Cut sequence independently of layout acknowledgments. `clipboard_feedback_survives_newer_layout_ack` verifies delivery feedback persists across layout updates and cannot overwrite a newer Copy's status. |

Additional V08 checks now cover an oversized request emitting no OSC 52 frame
while retaining all bytes for internal Paste and Save. The synchronized
`held_copy_respects_settings_and_suspension_at_delivery` check runs the normal
editor worker, holds Copy inside a real storage read, and overtakes it with
actual settings reloads or suspension. Disabled-to-enabled, disable/re-enable,
and suspension cases all suppress the obsolete system write and retain the
internal clipboard through a saved-byte round trip.

Final locked workspace checks passed on the same native macOS, Ubuntu and
Debian arm64 environments recorded above, starting with a 256-descriptor soft
limit. Formatting and Clippy passed with warnings denied. There were 334
enabled tests on macOS and 336 on each Linux target, with ten opt-in tests
excluded from each default suite. The optimized macOS and Ubuntu clipboard and
picker regressions and oversized clipboard round trip passed. The isolated
macOS tmux editing and suspension probe passed. Native macOS and Ubuntu release
binaries continue to report `0.0.1-dev.10`.

Initial full runs exposed a cycle in the new fixtures, which retained rendered
views while multiple editors shared the process-wide two-view budget. Thread
samples identified the wait in `LayoutLease::acquire`. Releasing unused test
views fixed it with the original five-second waits and production limits.
A piped-input check also inherited personal settings, which clipped its
expected coordinates behind the footer control. Its default test profile is
now isolated. Both full suites were rerun after these fixture corrections.

The final independent reviewer followed [REVIEW.md](../../REVIEW.md) against
`a76fa83`, inspected the six fixes and the fixture corrections, ran focused
clipboard and recovery checks, and independently verified the first-write
guard mutation. No actionable findings remained. Earlier frontend and release
acceptance gaps below remain open.

### Remaining acceptance

MVP-01 and MVP-06 have automated candidate evidence for V01–V09. Required
frontend workflows and exact client paste remain open. MVP-02 requires the
rest of the terminal matrix and the final macOS Cmd check. MVP-03 retains
the resource and timing gaps above. MVP-04 has native locked checks, independent
review and mutation evidence. The [macOS draft CI job](https://github.com/rapidclock/ede/actions/runs/44132989152/job/134214839862)
passed on `6b61405`. The full GitHub Actions matrix is pending.
MVP-05 remains open for terminal-specific instructions and the locked
`0.0.1` release build after validation.
