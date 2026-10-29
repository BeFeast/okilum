# Canonical VM150 harness change: #1808

The canonical harness is a separate local Git repository at
`/home/god/.cache/okilum-qa/b1115d33/vm150`, with no remote. This patch is its
reviewable change artifact, not a second driver. Apply only to the exact baseline
`d86e330` (the corrected PR-seed ordering), after review/merge and with a clean
canonical checkout. `git apply --check` must pass before applying. Do not apply
it twice: the current local branch already includes these changes.

The patch includes the previously reviewed session-scoped toast/OneDrive
preparation and restoration, bounded post-rollback boot observer, and the new
nested-error control correction. No snapshot, credential or service changes.
Session preparation remains before step01, hence before step04. A failure
restores the original per-user values or retains the lock; no force-kill fallback.

## Retained r3 evidence and diagnosis

Original receipt: `/home/god/work/qa-evidence/1698/pr-native-r3/ASSESSMENT.md`.
Success child completed in 5.8 seconds. Nested child started at guest UTC07:35:31.93,
entered catch at07:35:56.04 (24.1 seconds later), and persisted a complete error
report before collection. Parent exceeded its 30-second process deadline; exit
was null. Stdout/stderr were drained after the owned child was killed.

This proves neither handler deadlock nor wrong-process waiting. The acquired
process handle belongs to PID2272, the PID in the nested error report. The late
catch leaves less than six seconds of the original budget for formatting,
persistence and process teardown. VM scheduling/startup slowness is plausible;
GPU involvement is not established. Retained evidence lacks report-completion
and exit-request timestamps, so exact phase duration remains unknown.

The child budget becomes120 seconds, with first-control desktop budget330
seconds for two sequential children plus overhead, within the existing outer
60-minute attempt cutoff. New diagnostic phase records distinguish entry,
handler start, report persistence and requested exit. Phase records never count
as numeric exit or acceptance. Missing/null exit, timeout, incomplete streams,
wrong exception chain and false success remain failures. No gate retry is added.

## Validation and authority

Offline PowerShell7 controls cover success, nested/write/bootstrap/parse/log/trap
errors and retained phase/PID evidence. Session quiet tests mock registry/process
operations and cover restoration and failures. Actual Windows PowerShell5.1
behavior remains UNVERIFIED until a separately granted harness self-test.

No VM/reservation/build is authorized by this patch. Request a slot from Manager
when review is complete. #1698 r4 requires this fix merged plus explicit Manager
GO; never infer authority from a passing local test. Keep original seed5226 /
8113ab0a bytes. No signing/promotion or #1667 closure.

#1808's copied-build and #1414 external-file identity scenario are subsequent
work; this patch does not implement or claim their native acceptance.
