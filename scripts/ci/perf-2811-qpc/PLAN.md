# Step 2811 raw-QPC diagnostic controller

Status: LOCAL REVIEW ONLY. No Rust build, push or native capture has run.
The controller worktree is based on e4da51d3d; the compared feature source is
now pinned to 46194f96dcaf20e653843da66bcb292dfbdc1f25, main to
718d23d8b799bd50569ba0b8ff40e784afb4ed0c. Separate observer commits are prepared
from those exact bases. The local source refs are main `4ed83ebc3` and
toolbar `ae6dd4ea4`; both have only the same two-path observer patch. The feature
worktree and
the separate running-time gate proposal are untouched.

## Evidence and scope

Native run 43336080401 aborted after unmarked sample 22: wall 39.6195 ms,
draw 39.4572 ms, present 3.1542 ms, glyph_bitmap 0, coarse thread CPU 15.625 ms.
CodePreview layout/prepaint/paint sum is about 3.9912 ms (4.0303 ms with render).
Earlier samples have about 7 ms draw + 35 ms present. GPU/vsync Waiting is a
hypothesis, not an attribution. This harness creates Reader::new inside Root
and draws the whole window, including Reader chrome and side panels.

The old cc4 binary had no absolute frame stamps. Neither a later capture nor
this controller can retroactively classify its old miss or the new untraced
39.6195 ms observation. This plan classifies NEW source-pinned observations.

Main718 and feature46194f96 have byte-identical original toolbar_native.rs, SHA256
265473b112ec73a1cd68eeda723f5d8792b047f40491f5015c7a3a4f80f90a49, and identical
Rust1.100.0 toolchain, lockfile, release profile, fixture and backend patch.
Feature46194f96 differs from e4da only in cfg(test) Mac shaper fixture code;
the production shaping path remains unchanged.

## Source provenance and release configuration

After the manager pins the new final source, update SOURCES.toolbar and the
references file, commit the controller locally, and prepare two NEW source
worktrees with capsule.py. Commit only the common observer patch in each one:
toolbar_native.rs plus its new qpc_clock.rs module. Their parents are the exact
main/feature sources and their observer bytes are identical. Then record the
two actual diagnostic commit SHAs in references.json and commit the controller.
The two local refs were created only after that final source pin; no stale
e4da observer binary will be used.

Build metadata MUST name the actual committed diagnostic HEAD, with the base
SHA recorded separately. It never labels an instrumented binary unchanged.
provenance.py rejects anything except the two approved source paths, checks
their bytes against the controller, and records source/controller/input hashes,
complete observer diff, rustc/cargo versions, every CARGO_PROFILE_RELEASE_*
and every *RUSTFLAGS value (including target-specific and encoded flags), plus
the ordinary explicit build environment. Nonempty release-profile overrides
or Rustflags are rejected before cargo and checked again after build; none
are expected in this unchanged release configuration. --build-info, binary hash/size, checksum-pinned backend
manifest and default PE stack reserve/commit (1 MiB/4 KiB) are retained.

Use the original locked release build with data-view-native-harness. No changes
to release profile, gpui-pre core, backend, editor, shaping, thread/process
priority, affinity, stack, fixture, 100 ms open or 16 ms frame budgets.

## Observer and strict diagnostic semantics

Four QPC reads bracket the ORIGINAL Instant start and elapsed reads. Only two
extra QPC reads are inside the measured interval. Two manifest-free ETW string
markers run outside it; allocation/formatting and JSON recording also stay
outside. Keep the existing present/submission behavior. Log anchors before
asserting, so the last failed frame remains observable.

Diagnostic-only OKILUM_DIAG_COLLECT_ALL records every original open/frame budget
violation and completes BOTH 48-frame passes. It writes a separate diagnostic
receipt with all violations and strict_pass=false, then FAILS at the end. It
never emits the normal native PASS marker/receipt in this mode. Functional
assertions, actual row/viewport/glyph controls remain enabled: a functional
abort makes the census incomplete and therefore NO_EVIDENCE. Both the native
child process and controller exit nonzero if any original budget is violated;
the GitHub job remains red even when analysis/collection completes correctly.

Exactly five alternating main/toolbar pairs run on ONE windows-2022 runner,
using immutable binaries and fresh synthetic vault/state for every launch.
No retries or successful-run selection. Complete data is 480 frames per base,
240 unmarked and 240 marked. Builds are serial and use the same constant capsule
checkout path in both matrix jobs; compiler flags are unchanged. These
instrumented measurements remain separate from the original production run.

## Capture setup and preflight

The earlier successful capture used a preinstalled WPT in the standard Windows
SDK directory. wpt-setup.ps1 now resolves that directory or Get-Command, records
xperf path/file version/SHA and help, and verifies clocktype/stackwalk support
BEFORE starting any session. If missing, it installs only the SDK WPT feature
from the cached SDK BundleCachePath, falling back to the official Windows SDK
bootstrapper used by [runner-images provisioning](https://github.com/actions/runner-images/blob/main/images/windows/scripts/build/Install-VisualStudio.ps1).
The WPT-only [feature invocation](https://github.com/mvaneerde/blog/blob/a34c030d981897e5eba72241b6e732bbb45cb9ad/install/windows-performance-tools.ps1)
is OptionId.WindowsPerformanceToolkit. No tool installation has run locally.

Both immutable native binaries receive exactly one sanitized --build-info
preflight each BEFORE xperf starts. All inherited OKILUM_* fixture/control
variables are removed; real cases add only their own synthetic fixture and
collect-all mode. The traced group creates exactly ten native app launches,
without per-case --build-info helpers. Each launch checks file identity; after
ETW has stopped, postflight checks both full binary hashes/mtimes again.
Missing postflight invalidates provenance. WPT setup, native command dispatch
and Rust builds remain unverified until the authorized Windows workflow runs.

## Raw ETW and fail-closed analysis

Capture kernel PROC_THREAD+LOADER+CSWITCH+DISPATCHER with QPC/perf clock and
CSwitch/ReadyThread stackwalk. A second user session captures provider
{97057e99-44cf-4e25-aeb7-139ea0cdb951}, keyword1/level4. Always attempt to stop
both owned sessions, even if one stop fails. Preserve both raw ETLs and merged
ETL; tracerpt XML is a readability artifact, not the attribution clock.

export_raw.py uses OpenTraceW/ProcessTrace with EVENT_RECORD|RAW_TIMESTAMP
(0x10001000), explicit WinSDK x64 ABI layouts checked against pinned
windows-sys0.61.2. It retains raw payloads/timestamps, all stackwalk-eligible
scheduler origins, waker lifetimes and stacks. Both raw log headers must show
clock type QPC and the same frequency. Header, per-buffer and post-export loss
counters, callback status and raw/export hashes are validated.

Raw kernel stamps and native anchors already use QPC: no fitted offset or
stdout correlation is used. Every ETW marker must match its native PID/TID/ID,
be unique and present. For marker timestamp T and native bracket[L,R], the
offset constraint is[T-R,T-L]. Intersect ALL constraints, require a nonempty
intersection containing ZERO and apply zero offset. Reject malformed clocks,
wrong frequency, markers outside their surrounding frames, non-monotonic frame
brackets, missing/duplicate/wrong-PID markers and Instant/QPC disagreement.

Report outer/inner frame bounds, component ranges and observer bracket margin.
More than 0.1 ms boundary uncertainty is diagnostic NO_EVIDENCE; this is an
evidence-validity bound, not a replacement frame threshold. No measured overhead
is subtracted. Report the two additional in-interval QPC reads and marker-call
brackets rather than claiming zero observer cost.

Verify the CreateProcess primary PID/TID against an actual paired Thread Start
and End containing the complete case. Reject lifetime gaps/reuse, impossible
CPU migration, unpaired switches, uncovered states or missing Ready transitions.
Running is scheduled time. Ready requires observed Ready state or ReadyThread.
Waiting is only the observed interval before that transition. Unknown is never
silently relabeled Ready. Scheduled Running conservatively includes any DPC or
interrupt occupying that CPU; it is not claimed as exact user-code CPU time.

Causal StackWalk association is SEPARATE from proven thread state intervals.
Use exact event QPC, CPU and available owner identity; require a unique eligible
source event and a live paired thread interval. A ReadyThread stack belongs to
the waker, not the target UI thread. If a classic header hides waker identity,
a uniquely associated StackWalk owner with a matching lifetime is explicitly
labelled inferred. Unknown idle/DPC context, wrong/missing/ambiguous owner or
lifetime leaves the raw stack unassociated with causal NO_EVIDENCE. It does
NOT erase independently proven Ready/Waiting state coverage. Target lifetime,
clock, trace-loss or state-coverage failure DOES invalidate frame attribution.
Retain switch-out/waker stacks, raw wait reasons and module/loader records.
Release has debug=false, so private Rust symbols are not promised or fabricated.

## Review and launch boundary

Python parser controls cover positive observers plus missing/duplicate/wrong
marker identity, loss, corrupt clock intersection/order, unknown state coverage,
wrong causal owner/CPU, lifetime gaps, impossible migration, unassociated wakers,
and a complete diagnostic that must still FAIL both passes' original budgets.
Only Python checks run on maestro. Windows FFI/xperf and Rust compilation remain
UNVERIFIED until the manager authorizes the reviewed remote workflow.

The push-triggered workflow is toolbar-2811-qpc.yml. Once the manager approves
AND references.json contains the two committed diagnostic sources, push those
new refs first and the controller to a NEW diagnostic-toolbar-2811/qpc-controller-*
branch last. No force push. That final controller push is the capture command;
it must not happen during this review hold. The source SHAs are now pinned; the manager must still review runner,
provenance and capture details before launch. The exact launch command is:

```bash
cd /home/god/Developer/worktrees/okilum/2811-qpc-diagnostic-e4da
bash scripts/ci/perf-2811-qpc/launch-reviewed.sh
```

This command has NOT run. It is the explicit launch step after final review.

Publish every new miss with main/head counts, raw wall verdict, Running/Ready/
Waiting/Unknown ranges, uncertainty and separate causal status. No automatic
waiver follows from scheduler evidence. The native exception is still limited
to baseline issue2825; the separate gate proposal is not modified or merged.
