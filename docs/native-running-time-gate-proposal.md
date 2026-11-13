# Proposal: scheduler-aware native draw gate on main

Status: design-only proposal for independent main review. Production measurement, thresholds, clocks, priority and stack reserve remain unchanged. The manager's individual hosted-wall decisions for toolbar PRs 2839 and 2852 do not adopt this policy. No classifier implementation is authorized before this document is approved; the eventual implementation and its controls require a separate review.

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

For each frame publish the raw wall duration and all four components. Ready is a scheduler state, not proof of an external cause: the application's own workers can starve a runnable UI thread. A frame with 8 ms Running and 40 ms Ready caused by those workers must fail, rather than passing an 8 ms CPU guard.

### Attribute Ready before considering an exclusion

Partition each UI Ready interval at the relevant CPU switch and ownership boundaries in the same trace. Reconstruct the running occupant on every logical CPU on which the UI thread was eligible to run, using switched-in TIDs and their PID/lifetime ownership. Record the UI thread's processor group, affinity/CPU-set eligibility and relevant changes; reject an unsupported configuration or incomplete eligibility evidence. Do not change those settings to make the measurement pass.

The application-owned process set includes the measured Okilum process, its workers and application-launched helpers/descendants, with launch and lifetime evidence. A different PID alone does not make an application helper external. Missing ownership or process-family evidence cannot authorize an exclusion.

A time slice is a candidate external Ready exclusion only when **every eligible CPU** is proven occupied throughout that slice by a live thread belonging to another, non-application process. The audit must retain each CPU, switched-in TID, owning PID, lifetime and QPC interval. It must not infer attribution from a sampled stack, aggregate process CPU usage or a nearby event. Charge the entire slice if any eligible CPU is occupied by an application-owned thread; never average that time across CPUs or subtract only an allegedly external fraction. Charge idle, unknown or unsupported occupancy too. Missing trace coverage still rejects evidence rather than becoming an external pause. Interrupt/DPC time is not proven external-process occupancy and remains counted unless separately supported and reviewed; this proposal adds no such exclusion.

This is a conservative eligibility rule for a bounded exclusion, not a claim that Ready plus a PID proves the ultimate cause of a scheduling delay. Existing captures without complete competitor occupancy and application-process-family evidence do not qualify automatically.

Use mutually exclusive frame components and conservative bracket bounds. The proposed counted-work upper bound is:

`Running + Waiting + Unknown + application-owned/non-excludable Ready + boundary uncertainty <= 16 ms`

Equivalently, subtract only the proven external Ready **lower bound** from the conservative outer wall bracket, counting every remaining tick once. Partition one consistent inner bracket and charge its entire outer-minus-inner uncertainty; do not mix component upper bounds and then add the same uncertainty again. Include all unknown state and ownership time in the counted upper bound. Use the external Ready **upper bound** for exclusion caps. Also publish the standalone scheduled Running bound and its <=16 ms verdict. Blocking caused by application code, synchronous I/O, a contended lock or GPU synchronization stays counted, even when CPU time is low.

### Bound the exclusion and retain non-passing outcomes

Proposed initial limits are deliberately conservative and require approval with the document:

| Limit | Proposed value | Accounting scope |
|---|---:|---|
| External Ready excluded from one frame | <=16 ms | Entire eligible duration, not a truncated allowance |
| Total external Ready excluded in a fixed run | <=32 ms | All measured launches for one source on one OS |
| Frames using an exclusion | <=2% of expected frames, rounded down | At most 1/96 or 9/480 frames |

Only a frame whose raw wall exceeds 16 ms and would otherwise pass the counted-work bound is a candidate to use an exclusion. A frame already within the raw wall budget uses none. Publish all Ready observations regardless. Sum the entire candidate durations, not merely the amount needed to reach 16 ms, and do not trim them to fit a cap. The fixed run plan and expected frame count are declared before launch; do not reset totals per launch, pool main and candidate, pool OSes, omit a failing launch, or change the denominator after observing results. Compare caps in integer clock units/conservative bounds; equality is allowed and any excess is not.

Verdict order:

1. Invalid/missing evidence is `NO_EVIDENCE`, never PASS.
2. A counted-work upper bound above 16 ms or a failure of an unchanged functional, glyph or opening guard is `FAIL`, even if external saturation is also present. In particular, retain main's five Running failures.
3. If work would fit only by using eligible external Ready but any exclusion cap is exceeded, return **`RUNNER_SATURATED_INCONCLUSIVE` ("runner saturated — inconclusive, rerun")**, never PASS or a green required check. Preserve the raw wall failure, work bounds, all cap counters and the original artifact.
4. PASS requires complete evidence, all unchanged guards, counted work within budget and all three exclusion limits satisfied. Report any bounded exclusion explicitly alongside the still-visible raw wall verdict.

For example, 96 frames of 8 ms Running + 500 ms external Ready are saturated/inconclusive, not PASS; 8 ms Running + 40 ms application-worker Ready is FAIL. The historical 31–42 ms Ready observations above exceed the proposed per-frame cap if they were eventually proven external. They would remain non-passing under this policy, rather than being used to tune the cap upwards. The proposed saturation policy allows at most one separately recorded fixed rerun after approval of the CI policy; retain the original non-passing outcome and the second attempt separately. A second saturation result remains non-passing. Never retry an application FAIL through this rule, combine attempts or select a best attempt. This document authorizes no execution or automatic retry before approval.

The numbers 16 ms and 100 ms are unchanged. This is a deliberate, separately reviewed metric change for frames, not a threshold waiver inside a toolbar PR. Keep the **100 ms opening budget on raw wall with no Ready exclusion**, and keep the glyph-rasterization guard, stack reserve and production configuration unchanged.

For a separately reviewed Mac implementation, count Running + Interrupted + Blocked + unknown against the same budget. A Preempted/Runnable state alone cannot qualify: require equivalent competing-thread PID/process-family and eligible-CPU occupancy evidence, plus the same three caps and non-passing outcomes. Without that evidence, keep the wall gate. Require equivalent raw-loss and clock-validation controls before adoption. Scheduled Running may include interrupt/DPC occupancy; count it conservatively rather than claiming exact user-code execution time. Do not substitute GetThreadTimes: the observed 15.625 ms quantization is too coarse for the frame budget. Do not convert QueryThreadCycleTime cycles directly to milliseconds under variable CPU frequency.

## Fail-closed evidence requirements

The analyzer must reject missing or incomplete evidence rather than returning PASS:

- Verify raw trace EventsLost and relevant buffer/lost-event counters, including exporter metadata. Retain the raw ETL and exported XML, their hashes, the analyzer version, and its output.
- Require matching PID/TID lifetime, monotonic ordered bounds, complete trace coverage before and after every frame, valid switch pairing, and a working ReadyThread observer. Handle TID reuse across launches.
- Require complete per-CPU running-occupant coverage throughout each candidate Ready exclusion, including the live PID owners of switched-in threads, the application-owned process set and the UI thread's CPU eligibility. Ambiguous ownership is counted and cannot be inferred external. Missing coverage fails closed. Preserve proof for every excluded slice, not only the UI thread's own switches.
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
5. A known external Ready scheduler fixture is excluded only over its exact interval with complete competitor PID/lifetime and eligible-CPU occupancy evidence, and only within all three caps. A Ready-only fixture without that evidence remains counted. Never require an uncontrolled hosted scheduler pause to serve as a deterministic positive control.
6. Remove ReadyThread or a necessary switch event, inject EventsLost, truncate coverage, change PID/TID ownership, or corrupt clock bounds: evidence fails closed. A Waiting interval without its transition remains unknown/countable.
7. Revert the Running/Waiting accounting independently and demonstrate that the corresponding busy-loop/Sleep/mutex control fails for the expected reason. Reject arbitrary unrelated failures.
8. A compiled worker-oversubscription fixture creates application-owned CPU workers under the unchanged priority/affinity policy. Its trace must prove UI Ready overlapping those workers' CPU occupancy; require the measured path to fire, not a zero-duration success. An 8 ms Running + 40 ms own-worker Ready analyzer fixture must produce FAIL (48 ms counted). Include application-helper children with different PIDs, mixed own/external CPU occupants and PID reuse. A negative control that excludes all Ready, or treats a child as external solely because its PID differs, must make this control fail for that specific misclassification. Keep deterministic analyzer fixtures alongside the native observer proof; a native run without the required contention evidence cannot pass the control.
9. Exercise each exclusion limit independently at equality and one clock tick above it. Isolate the per-frame cap with one frame; isolate the 32 ms run total across a declared 480-frame plan while staying within the per-frame and frame-share limits; isolate the share cap with two small exclusions in 96 frames while staying below both duration caps. Excess returns `RUNNER_SATURATED_INCONCLUSIVE`, never PASS. Test the 96 x (8 ms Running + 500 ms external Ready) case and prove that removing any limit makes its independently isolated negative fail.
10. An inconclusive earlier launch cannot be dropped, reset at the next launch, pooled with a baseline or turned into PASS by keeping only a later clean attempt. A simultaneous >16 ms Running/Waiting failure remains FAIL, not saturation/inconclusive. Raw opening >100 ms still fails with external Ready, because opening has no exclusion.

The independent synthetic analyzer controls already saved beside this proposal are useful evidence for parser behavior, but they are not compiled/native timing controls and do not replace them.

## Linux policy

Linux keeps the existing raw Instant wall gate: <=16 ms per measured frame and <=100 ms to open, with unchanged glyph/functional guards and stack reserve. This proposal adds no Linux Ready, CPU-time or scheduling exclusion. Optional scheduler diagnostics may explain a failure but cannot turn it into PASS. Any Linux replacement would need its own reviewed evidence contract, attribution controls, saturation limits and explicit approval.

## Mac scope

The exact-source Mac census in [the Mac diagnostic run](https://github.com/BeFeast/okilum/actions/runs/43264140890) recorded five normal launches / 480 native Metal frames: one wall miss, zero new scroll bitmaps, and maximum measured UI-thread CPU 15.132083 ms. The missed frame (marked sample 7, fifth launch) was 28.790750 ms wall, 3.728625 ms CPU, approximately 25.062125 ms off-CPU, with a valid 0.001250 ms bracket margin. One non-missed frame has excessive 0.113250 ms margin and is not used for CPU attribution; its raw wall result remains in the census. These new observations cannot retroactively classify the original untraced 18.719083 ms frame.

A separate Time Profiler capture is deliberately excluded from the normal census: it recorded four wall misses and maximum CPU 19.898 ms. Instrumented and unprofiled measurements must not be pooled. The raw Mac case metadata has a generic Windows backend string; the audited manifest corrects this to native macOS Metal.

A further [single System Trace capture](https://github.com/BeFeast/okilum/actions/runs/43269474664) reused that same Mac binary, with no rebuild, privilege change or retry. It provides scheduler state records. A new 16.962708 ms wall miss has approximately 6.600376 ms Running, 0.859332 ms Interrupted and 9.510292 ms Preempted, with Blocked/Runnable zero in its 16.970000 ms outer bracket. Running+Interrupted is 7.459708 ms versus the native thread-CPU counter of 7.460583 ms. These figures are from a separate instrumented case, not pooled with the five normal launches. Clock mapping uses `_rawWindowStartTime` from the trace's archived metadata; the human-readable TOC start-date is not the correct origin for this windowed trace. This classifies the new traced miss, not the original untraced 18.719083 ms observation. Full scheduler-audit details are retained alongside the raw trace. The System Trace archive also contains one literal RunIssues record, `Data stream: Time Mapping` (`ZTYPE=2`; severity meaning not inferred). No explicit kernel lost-event counter was exported, so Mac losslessness is not established. All 96 frame brackets have state coverage with unknown time zero; 95/96 CPU/state comparisons differ by at most 0.05 ms, the largest discrepancy is 0.224751 ms on a non-missed frame, and the missed frame differs by 0.000875 ms. Shifting the metadata anchor by +/-1 ms as a sensitivity test still gives 9.334792–9.510292 ms Preempted and zero Blocked for the missed frame; this is not a measured absolute calibration-error bound. These limits prevent treating this diagnostic as a fail-closed production-gate pass.

CLOCK_THREAD_CPUTIME_ID can measure CPU time and derive aggregate off-CPU time. It cannot distinguish scheduler Ready from application Waiting. Therefore Mac CPU-only measurements cannot waive the existing wall gate. Keep the Mac wall verdict until equivalent scheduling evidence and the same blocking controls are available and separately reviewed. Time Profiler sampling alone does not provide an exact Ready/Waiting interval decomposition.

## Delivery boundary

This document introduces no implementation or general exception. The manager's accepted hosted-wall classification for PR 2839 does not redefine the routine gate. The completed measurements and their limits are ready for manager review. Implement and review any adopted metric as the standalone main change, with its own native and compiled controls. Review its implementation and compiled controls separately, evaluate the eventual gate implementation independently of the toolbar PR.
