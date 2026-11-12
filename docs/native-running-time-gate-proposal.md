# Proposal: scheduler-aware native draw gate on main

Status: design-only proposal for independent main review. Production measurement, thresholds, clocks, priority and stack reserve remain unchanged. The manager accepted the Windows census classification for toolbar PR 2839 and will merge it after its two remaining Find fixes and green exact-head agent checks, with the hosted wall gate red for the recorded scheduler reason. This proposal is separate from that decision.

The five main Running failures are tracked in [the native Running budget investigation](https://git.oklabs.uk/BeFeast/okilum/issues/2845).

## Evidence motivating a separate review

The completed fixed census in [the exact-source diagnostic run](https://github.com/BeFeast/okilum/actions/runs/43263144721) compares these production viewer sources with explicitly recorded observer/harness additions:

- Main: `774cef4abf61d39147069918f8ba8a1000008187`.
- Toolbar: `863bfdd3a50f699087f5d81d154dd59d1cdcd202`.
- Diagnostic controller: `b1d8796bf1871f444f869307fd49e65b575ea2e6`.
- Fixture SHA256: `3111144a60615a0a1870164cfc22dae16d60a5a7c0813c7e3e74d294ad47af88` (3.5 MB, 414 paths).
- Both use the merged staging DirectX atlas backend, the same hosted Windows runner, and the same scenario. Each launch records all 48 unmarked and 48 marked frames. Five main/toolbar pairs run in a fixed alternating order; a wall miss does not truncate the diagnostic census.

| Source | Launches with a wall miss | Unmarked misses | Marked misses | All frame misses |
|---|---:|---:|---:|---:|
| Main | 5/5 | 9/240 | 12/240 | 21/480 |
| Toolbar | 4/5 | 2/240 | 2/240 | 4/480 |

These are counts from one completed fixed census, not a statistical equality claim or a claim about all hosted runners. Main missed more often in this sample. The diagnostic census is not a green production gate.

The analyzer currently classifies all four toolbar wall misses as follows; the scheduled Running upper bound conservatively includes the small observer bracket margin:

| Wall ms | Scheduled Running upper ms | Ready ms | Waiting ms | Unknown off-CPU ms |
|---:|---:|---:|---:|---:|
| 16.8928 | 15.0258 | 1.8803 | 0 | 0 |
| 51.0172 | 8.5686 | 42.4539 | 0 | 0 |
| 46.0162 | 7.8617 | 38.1591 | 0 | 0 |
| 38.6553 | 7.0626 | 31.5965 | 0 | 0 |

Five main frames have a scheduled Running lower bound above 16 ms, even after excluding Ready. Those frames must still fail under this proposal. A new metric must not be designed to make the entire main baseline green.

Independent Windows audit confirms 960/960 native frames, EventsLost=0, BuffersLost=0, the source/harness manifests, and a maximum frame-bracket margin of 0.0268 ms. All ten distinct UI TIDs were verified against process identity and Thread Start/End lifetime; CSwitch and ReadyThread decode errors are zero. Mac CPU and scheduler evidence is available below; both Time Profiler and one bounded System Trace capture have completed. Original untraced 42.4662 ms Windows and 18.719083 ms Mac frames cannot be retroactively attributed by a later capture; the exact-source census supplies new observations.

## Proposed measurement and verdict

Retain and publish the existing Instant wall interval for every update/draw/arena-clear frame. Keep present/submission separately measured, as today. Do not silently rename wall time to CPU time.

On Windows capture kernel CSwitch and ReadyThread events over the complete native process lifetime. Record precise frame bounds plus PID, native TID, source SHA, binary hash, backend manifest, fixture hash, and harness/controller hash. Reconstruct:

- Scheduled Running: the thread is scheduled on a logical CPU.
- Ready: the thread is runnable but not scheduled. A switch-out in Ready proves this directly; a switch-out in Waiting needs a matching ReadyThread transition before the Ready portion can begin.
- Waiting: the observed interval before that ReadyThread transition.
- Unknown off-CPU: an off-CPU interval lacking sufficient transition evidence. Never relabel it Ready.

For each frame publish the raw wall duration and all four components. Use the conservative Running upper bound from the observed clock bracket. The proposed application-work guard is:

`scheduled_running_upper_ms + waiting_ms + unknown_off_cpu_ms <= 16 ms`

Also publish the standalone scheduled Running value and its <=16 ms verdict. Exclude only positively classified Ready time. Blocking caused by application code, synchronous I/O, a contended lock, or GPU synchronization must remain counted and must not become invisible because CPU time is low. Do not subtract unrelated process/thread CPU time or a nearby scheduling pause.

The number 16 ms is unchanged, but this is a deliberate metric change from raw elapsed time to application time excluding proven Ready. It requires separate review and authorization; it is not a threshold waiver inside PR 2839. Keep the raw wall result visible, even when the proposed work guard passes. Leave the existing 100 ms opening budget and the glyph-rasterization guard unchanged.

For a separately reviewed Mac implementation, count Running + Interrupted + Blocked + unknown against the same budget, and exclude only positively established runnable/descheduled Preempted/Runnable intervals. Require equivalent raw-loss and clock-validation controls before adopting it. Scheduled Running includes time during which an interrupt or DPC may occupy the scheduled CPU unless those events are captured and accounted separately. Initially count it conservatively rather than claiming exact user-code execution time. Do not substitute GetThreadTimes: the observed 15.625 ms quantization is too coarse for the frame budget. Do not convert QueryThreadCycleTime cycles directly to milliseconds under variable CPU frequency.

## Fail-closed evidence requirements

The analyzer must reject missing or incomplete evidence rather than returning PASS:

- Verify raw trace EventsLost and relevant buffer/lost-event counters, including exporter metadata. Retain the raw ETL and exported XML, their hashes, the analyzer version, and its output.
- Require matching PID/TID lifetime, monotonic ordered bounds, complete trace coverage before and after every frame, valid switch pairing, and a working ReadyThread observer. Handle TID reuse across launches.
- Require all expected 96 frames per launch, actual viewport movement and row construction, expected query results for marked frames, and the correct release/native configuration without GPUI test support.
- Validate the clock correlation and bound instrumentation error. An excessive bracket margin, impossible overlap, missing transitions required for attribution, or unexplained time becomes NO_EVIDENCE. Unknown time is counted, never excluded.
- Preserve a nonzero observer positive control. A trace that never records the measured path cannot pass through zeros.

Do not change thread/process priority, stack reserve, backend choice, fixture, rendering, editor/shaping, or production styles to obtain a favorable result. Do not repeat failed runs until one happens to pass. Use fixed run plans and retain every outcome.

## Controls required in the standalone change

Use actual native measured code paths, supplemented by analyzer fixtures:

1. A normal warm frame passes, with real draws/submissions and the existing glyph observer controls.
2. A compiled UI-thread busy-loop exceeding 16 ms fails scheduled Running and the application-work guard.
3. A compiled UI-thread Sleep exceeding 16 ms fails the application-work guard despite low CPU time.
4. A compiled contended-mutex wait exceeding 16 ms fails the same guard; use synchronization that proves the waiter was blocked.
5. A known Ready-only scheduler fixture is excluded only over its exact proven interval. Never require an uncontrolled hosted scheduler pause to serve as a deterministic positive control.
6. Remove ReadyThread or a necessary switch event, inject EventsLost, truncate coverage, change PID/TID ownership, or corrupt clock bounds: evidence fails closed. A Waiting interval without its transition remains unknown/countable.
7. Revert the Running/Waiting accounting independently and demonstrate that the corresponding busy-loop/Sleep/mutex control fails for the expected reason. Reject arbitrary unrelated failures.

The independent synthetic analyzer controls already saved beside this proposal are useful evidence for parser behavior, but they are not compiled/native timing controls and do not replace them.

## Mac scope

The exact-source Mac census in [the Mac diagnostic run](https://github.com/BeFeast/okilum/actions/runs/43264140890) recorded five normal launches / 480 native Metal frames: one wall miss, zero new scroll bitmaps, and maximum measured UI-thread CPU 15.132083 ms. The missed frame (marked sample 7, fifth launch) was 28.790750 ms wall, 3.728625 ms CPU, approximately 25.062125 ms off-CPU, with a valid 0.001250 ms bracket margin. One non-missed frame has excessive 0.113250 ms margin and is not used for CPU attribution; its raw wall result remains in the census. These new observations cannot retroactively classify the original untraced 18.719083 ms frame.

A separate Time Profiler capture is deliberately excluded from the normal census: it recorded four wall misses and maximum CPU 19.898 ms. Instrumented and unprofiled measurements must not be pooled. The raw Mac case metadata has a generic Windows backend string; the audited manifest corrects this to native macOS Metal.

A further [single System Trace capture](https://github.com/BeFeast/okilum/actions/runs/43269474664) reused that same Mac binary, with no rebuild, privilege change or retry. It provides scheduler state records. A new 16.962708 ms wall miss has approximately 6.600376 ms Running, 0.859332 ms Interrupted and 9.510292 ms Preempted, with Blocked/Runnable zero in its 16.970000 ms outer bracket. Running+Interrupted is 7.459708 ms versus the native thread-CPU counter of 7.460583 ms. These figures are from a separate instrumented case, not pooled with the five normal launches. Clock mapping uses `_rawWindowStartTime` from the trace's archived metadata; the human-readable TOC start-date is not the correct origin for this windowed trace. This classifies the new traced miss, not the original untraced 18.719083 ms observation. Full scheduler-audit details are retained alongside the raw trace. The System Trace archive also contains one literal RunIssues record, `Data stream: Time Mapping` (`ZTYPE=2`; severity meaning not inferred). No explicit kernel lost-event counter was exported, so Mac losslessness is not established. All 96 frame brackets have state coverage with unknown time zero; 95/96 CPU/state comparisons differ by at most 0.05 ms, the largest discrepancy is 0.224751 ms on a non-missed frame, and the missed frame differs by 0.000875 ms. Shifting the metadata anchor by +/-1 ms as a sensitivity test still gives 9.334792–9.510292 ms Preempted and zero Blocked for the missed frame; this is not a measured absolute calibration-error bound. These limits prevent treating this diagnostic as a fail-closed production-gate pass.

CLOCK_THREAD_CPUTIME_ID can measure CPU time and derive aggregate off-CPU time. It cannot distinguish scheduler Ready from application Waiting. Therefore Mac CPU-only measurements cannot waive the existing wall gate. Keep the Mac wall verdict until equivalent scheduling evidence and the same blocking controls are available and separately reviewed. Time Profiler sampling alone does not provide an exact Ready/Waiting interval decomposition.

## Delivery boundary

This document introduces no implementation or general exception. The manager's accepted hosted-wall classification for PR 2839 does not redefine the routine gate. The completed measurements and their limits are ready for manager review. Implement and review any adopted metric as the standalone main change, with its own native and compiled controls. Review its implementation and compiled controls separately, evaluate the eventual gate implementation independently of the toolbar PR.
