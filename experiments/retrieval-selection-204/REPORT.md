# Verified experience selection: measured diagnostic (#204)

> **Historical document.** Written before Tessera was renamed Okilum (2026-10-09). Names, paths and links are kept as they were then.

**Decision: Keep feature disabled.** Seven applicable cases improved, but one invalid baseline response blocks advancement under the frozen contract.

In c15 (obsolete callback incident), the baseline chose the ordinary `retry-submit` action but put `manual-pin` in `constraint_ids` and left `evidence_ids` empty. This is an unknown constraint and lacks supporting evidence, so it remains ambiguous/unsuccessful. The candidate response was valid. No label adjustment, retry or extra provider call was made.

This is a 24-case synthetic finite-choice evidence-selection diagnostic, not a measurement of open-ended agent reliability or production task completion. Runtime behavior and ordinary-workspace enrollment remain unchanged.

## Measured result

| Gate | Result |
| --- | --- |
| Actual provider calls | 48/48 |
| Complete provider/model/usage evidence | True |
| Applicable improvements | 7/8 |
| Applicable regressions | 0 |
| New false constraints in 16 controls | 0 |
| Total false constraints (baseline / candidate) | 2 / 1 |
| Wrong goal associations | 0 |
| Ambiguous outputs | 1 |
| Baseline c01 positive control | True |
| Invariant checks | PASS |

The applicability threshold was at least 3/8 improvements, no applicable regressions, no new control false constraints, and no ownership/staleness/packet mutation violations. Missing or inconsistent usage, wrong model identity, non-stop completion and ambiguous output block advancement. Scoring and labels were frozen before provider calls.

## Cases

| Case | Group | Baseline action | Candidate action | Baseline success | Candidate success |
| --- | --- | --- | --- | --- | --- |
| c01 | applicable | release-workspace | release-workspace | True | True |
| c02 | inapplicable | release-direct | release-direct | True | True |
| c03 | obsolete | release-workspace | release-workspace | False | False |
| c04 | applicable | export-default | export-linked | False | True |
| c05 | inapplicable | export-default | export-default | True | True |
| c06 | obsolete | export-default | export-default | True | True |
| c07 | applicable | repeat-interval | repeat-wall | False | True |
| c08 | inapplicable | repeat-interval | repeat-interval | True | True |
| c09 | obsolete | repeat-interval | repeat-interval | True | True |
| c10 | applicable | book-default | book-reserved | False | True |
| c11 | inapplicable | book-default | book-default | True | True |
| c12 | obsolete | book-default | book-default | True | True |
| c13 | applicable | retry-submit | lookup-job | False | True |
| c14 | inapplicable | retry-submit | retry-submit | True | True |
| c15 | obsolete | retry-submit (invalid fields) | retry-submit | False | True |
| c16 | applicable | migrate-online | pause-writer | False | True |
| c17 | inapplicable | migrate-online | migrate-online | True | True |
| c18 | obsolete | migrate-online | migrate-online | True | True |
| c19 | applicable | refresh-incremental | rebuild-vectors | False | True |
| c20 | inapplicable | refresh-incremental | refresh-incremental | True | True |
| c21 | obsolete | refresh-incremental | refresh-incremental | True | True |
| c22 | applicable | close-on-ack | read-decision | False | True |
| c23 | inapplicable | close-on-ack | close-on-ack | True | True |
| c24 | obsolete | close-on-ack | close-on-ack | True | True |

Both arms failed c03 (obsolete lease incident) by choosing the exceptional workspace-release action without applicable incident evidence. This is one false constraint in each arm, not a new candidate regression. The invalid c15 baseline is the second baseline false constraint. Zero **new** false constraints therefore does not mean error-free controls.

Success requires an executable labeled action, exactly the justified constraints and current supporting evidence. The c01 baseline already contains its reviewed incident. The other seven applicable baselines have an ordinary runbook but no historical exception; an unsupported guess cannot count as evidence-supported success. Controls have no applicable supplement.

## Provider and allocation

| Arm | Actual input tokens | Actual output tokens | Actual total tokens | Median wall latency (ms) | Sum wall latency (ms) |
| --- | --- | --- | --- | --- | --- |
| baseline | 31531 | 2872 | 34403 | 8441.0 | 198559.2 |
| candidate | 34090 | 2045 | 36135 | 6435.1 | 169106.2 |

Requested model: gpt-5.6-sol. All calls ran serially on the development host in one session, alternating baseline-first/candidate-first by case. The local recording relay used the saved Chat upstream/reference, temperature 0, max_tokens 1024 and stream_options.include_usage true for both arms. No retry, fallback or extra scoring calls were made.

Each arm had the same allocation: 12,000 UTF-8 message bytes, 8,192 input tokens and 1,024 output tokens per case; aggregate ceilings 196,608 input and 24,576 output tokens per arm. Observed usage is returned by upstream, not estimated or padded. End-to-end Chat latency includes local polling and relay buffering, so these times do not measure first-token latency or general model speed.

## Provenance and validation

- Frozen source commit: `e80826ce6ea9bb41f273449170d447df0a91ba8c`.
- Backend SHA256: `8b8ff1768af2b41e22de56b9a671e6cdda6e6ab724ad4608d3311c371c298a6d` (existing 9a43a60 backend; no new runtime build required).
- Independent measured review: **PASS_EVIDENCE_REVIEW_KEEP_DISABLED**; all 48 request/response bindings, usage, scoring and guards independently recomputed.
- Independent pre-provider review: PASS; all six frozen source hashes, 104 evidence artifacts, 24 identical base-context pairs and 48 controlled request bindings independently checked.
- Five deterministic scorer/transport-gate tests: PASS. Controlled offline4: 48/48 fixture calls, 24 valid manual prepare/review controls, eight stale-source rejections, one wrong-owner rejection and cleanup PASS.
- Both measured arms use actual brain_search/context_prepare/context_revise/goal_context_brief/chat_start/chat_get APIs. Candidate selection is separate visible evidence; existing reviewed packets and manual pins remain immutable.
- The goal, current incident source, reviewed packet and manual pin are hashed before/after each arm; selected source revisions are checked immediately before use.
- Original offline1/index-readiness failure and offline2/3/4 artifacts remain preserved separately. Offline fixture usage is synthetic transport validation and is excluded from all measured totals above.

## Limits and recommendation

The eight themes each have three related variants, so the 24 cases are not independent domains. Action catalogs expose possible exceptions; a model can infer answers without retrieval. The baseline is deliberately limited to its reviewed runbook and goal context, not unlimited manual investigation. These constraints favor testing selection and application, and prevent general reliability claims.

A passing diagnostic supports only a separately scoped, independently reviewed implementation issue. A failed or incomplete gate keeps the feature disabled; no labels or scoring rules are adjusted after observing responses.

## Exact blocking response

The original c15 baseline response is preserved verbatim:

```json
{"goal_id":"948e3fc7-faf7-52bd-a3ff-29858aa74038","action_id":"retry-submit","constraint_ids":["manual-pin"],"evidence_ids":[]}
```

This control pair received identical baseline/candidate messages because no incident was selected. A single sampling difference with invalid fields is not evidence of a selection benefit. The predeclared ambiguity gate remains binding.

## Local evidence

Exact prompts, raw SSE outputs, API receipts, prepared packets, per-case scores, source hashes and cleanup receipts are retained under the isolated `measured1` directory. The evidence inventory records SHA256 for each file. Credentials are references only and auth headers are not recorded.

- [Run directory](/home/example/worktrees/tessera/day-20260907/openexecutive-experiment/measured1).
- [Pre-provider manifest](/home/example/worktrees/tessera/day-20260907/openexecutive-experiment/reviewed-manifest.json).
- [Independent pre-provider review](/home/example/worktrees/tessera/day-20260907/openexecutive-experiment/independent-prefreeze-review.json).
- [Independent measured review](/home/example/worktrees/tessera/day-20260907/openexecutive-experiment/independent-measured-review.json).
- [Summary](/home/example/worktrees/tessera/day-20260907/openexecutive-experiment/measured1/summary.json).
- [Evidence inventory](/home/example/worktrees/tessera/day-20260907/openexecutive-experiment/measured-evidence-inventory.json).
