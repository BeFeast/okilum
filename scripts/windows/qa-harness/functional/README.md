# Canonical copied-build functional driver: admission and lifecycle

#1808 second implementation slice; no native authority. This module is consumed
by the canonical VM150 driver, never a replacement for vm.sh/run-evidence.sh.
Original hosted Velopack portable bytes are required, including .portable and
original launcher/Update.exe files. Only current/okilum.exe is ever launched. No
Setup, installed app, package-cache injection or protocol registration is used;
actual startup must identify portable mode before any link or close action.

`copied_artifact.py MANIFEST NEW_STAGE_DIRECTORY` validates original saved hosted
run/artifact records, exact source/digest and the single portable ZIP. It retains
that ZIP unchanged, with every original payload hash. It rejects paths, symlinks,
duplicates, Windows reserved/device names and missing portable marker, package
version, executable or shaders. It retains all producer files without filtering. It performs
no download, GitHub request, build, repackaging or VM action. The manifest names
source, hosted_run, artifact_metadata and artifact_zip local files.

The fixed desktop lifecycle runs inside canonical run-evidence with lib.ps1:

- Root is a fresh functional-<nonce> path below the canonical QA directory.
- ReadCopiedContract verifies the original ZIP digest; AssertCopiedFiles checks
  every member, file count and reparse guards before any process action.
- StartCopiedApp refuses every existing Okilum process, uses an owned identity
  fixture vault and separate state/index (hence separate instance endpoint).
- AssertCopiedProcess binds session1/path/hash/PID/start-time, instance descriptor,
  actual startup version/build and whats-new full source before delivery/close.
- SendCopiedLink accepts only the committed Home/Landing and identity-vault URI
  vocabulary, invokes the exact admitted executable once and requires bounded
  forwarding exit0 plus unchanged primary owner. Navigation is separately observed.
- CloseCopiedApp sends ordinary Alt+F4 to an owned window and waits for real exit;
  it does not fake reopen by killing the application. A lingering process STOPs.

## Fixed execution

After review and explicit grant, invoke run-functional.py --execute MANIFEST
NEW_EVIDENCE GRANT_TEXT from the canonical checkout. It accepts one literal FREE
resource confirmation after owner/slot announcement. It uses only existing
vm.sh/run-evidence boundaries, with clean rollback/lock before and after, PS5
positive controls and session quiet lifecycle. It stages the original bundle,
checks its digest/files, and runs one fixed identity-driver.ps1 payload.

The same desktop step contains each real Copy and the write-failure sharing
handle; no cross-session fault simulation. Six exact menu/copy observations
require owner inspection of the exact PNG and labeled integer bounds. Full PNG,
window and process must remain unchanged before a single click. Clipboard starts
with a checked sentinel; unchanged/missing clipboard is failure, not fallback.

The fixed sequence closes/reopens normally, checks real root/note readiness,
renames only the owned fixture while closed, then observes valid-ID and legacy
Landing.md navigation and mismatch refusal from Home.md. No native tests run
from caller-supplied scripts or fixture receipt injection. Arbitrary URLs are
refused. Ordinary close requires exactly one owned Reader, and lingering process
means STOP. Cleanup may terminate only an exact recorded app PID/path/start time;
foreign processes or unprovable cleanup retain the lock. Session restoration,
raw archive, clean rollback/unlock and bounded app/task0 readback are required.

## Independent frame review remains mandatory

Executed is not accepted: the native payload deliberately leaves inspected frame
hashes and visible-refusal text unset in its scenario receipt. After downloading
raw.zip, independently inspect the retained source/result frame blobs and exact
metadata bytes for all six steps, set those inspection fields in a separate
reviewed receipt, and run ../scenarios/external_identity.validate. Keep original
raw receipt immutable. The validator returns only EVIDENCE_CONSISTENT; acceptance
also requires admitted source/provenance, correct full-frame actions, actual
visible mismatch refusal, clean rollback and postclean app/task0. Missing refusal,
unknown UI layout, false/missing positive control or first new blocker STOPs.

The outer work cutoff is45min and release deadline60min; actions/input waits and
all transport/desktop operations are bounded within them. The current attempt
cannot extend these deadlines. Manager remains the authority for native scope.
No signing/promotion. This is an offline candidate until ONE review and native
self-test; #1808/#1414 Windows gate stay open until the actual run passes.
#1788 WebView2/runtime/profile behavior needs a distinct fixed scenario.

## Offline checks

Eight original-artifact tests and seventeen metadata/mutation controls PASS.
Original retained5226 packaging artifact is a positive admission control, not the
#1414 package. PowerShell AST/compile checks validate syntax only. Windows process,
sharing, clipboard, UI/forwarding and observer behavior require native verification.
