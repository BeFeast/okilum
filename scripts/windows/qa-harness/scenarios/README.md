# #1808 external-file-identity-v1 scenario contract

This is the first offline implementation slice for #1414 / PR #1852. It defines
one finite scenario, a Windows write-failure fixture and evidence consistency
checks. It is NOT an executable native driver, a release gate, or a VM grant.
No native PASS may be derived from a fabricated JSON receipt or these unit tests.
The canonical scenario remains unavailable until the integration below is
implemented and reviewed. Keep #1808 and #1414's Windows gate open.

## Required fixed operations

1. Admit the original hosted exact-head Windows artifact using canonical source,
   workflow/run, archive and executable hashes. Use an isolated copied build for
   reopen: installed PR-seed startup can apply a pending release, invalidating
   source identity. Copied mode must never use the installed app or register its
   protocol as the user's default. CLI delivery to the exact copied executable
   tests single-instance URL forwarding; OS protocol registration is separate.
2. Clean snapshot, owner/expiry lock, PS5 controls, canonical quiet preparation.
   Stage two owned vaults IdentitySeed and IdentityFailure under a fresh
   `C:\Users\Public\okilum-qa\identity-<nonce>` root. Both receive the exact
   UTF-8/CRLF metadata bytes in external_identity.INITIAL and Home/Landing notes.
   Full-vault scope is required; quick-view Copy has different semantics.
3. In IdentitySeed, observe the real Note menu and personally inspect the exact
   frame for Copy Okilum link. Use a same-step approval, complete unchanged-frame
   recheck, action intent and one click. Read the actual clipboard and metadata
   bytes before/after, preserving unknown JSON values and the new stored ID.
4. In IdentityFailure, hold EnterIdentityWriteFailure's read-sharing handle in
   the SAME desktop step as real Copy. It permits reads and denies write/delete
   access. The fixture proves read, write denial and DELETE-sharing denial in
   the app's QA security context. Record actual legacy clipboard fallback,
   unchanged bytes and no leftover temporary metadata files. Dispose in finally;
   a failed positive control is STOP, not permission to try another fault mode.
5. Ordinary close/reopen of the exact copied executable with the same profile:
   prove old PID stopped, new PID/source/path/hash and same copied ID/metadata.
   Cleanup must guard exact path/hash/PID/start time and attributed child profile.
6. Move the owned vault to IdentityRenamed while the app is closed, then reopen
   it in full-vault scope. Deliver a valid-ID URI carrying IdentitySeed's old
   folder name to Landing.md: actual navigation must succeed in IdentityRenamed.
   Deliver legacy folder-name URI to Landing.md as a distinct positive control.
   Do not seed an enrolled vault ID or write metadata to make resolution pass.
7. Return to Home.md, capture navigation/metadata before, then deliver a losing-ID
   URI. Capture the visible refusal, unchanged current root/note, unchanged
   metadata and no new selected vault/window. Valid/legacy controls in the same
   session prove delivery/observation rather than merely observing nothing.
8. Retain per-step source/PID, exact inspected PNG hashes, actual link/clipboard
   values and source/result bytes. Run validate() only after provenance and frame
   review. It returns EVIDENCE_CONSISTENT, never native acceptance by itself.
   Stop owned processes, restore quiet session, collect raw evidence, rollback
   clean, unlock and obtain bounded postclean app/task0 readback.

## Metadata contract

#1414's production write_link_id reads the JSON object and write_value serializes
it with serde_json::to_vec_pretty. Unknown values must survive; whitespace/key
ordering is not promised. Preserve the original and resulting byte artifacts,
assert unknown values/type/nested fields, and allow only added id/version fields.
Failed writes, reopen and mismatch refusal require exact unchanged bytes.
Duplicate JSON keys are rejected by this fixture to avoid an ambiguous oracle.
The original design's suggestion of byte-preserving formatting is superseded by
this explicit source-backed interpretation, not a relaxation of no-overwrite.

## Remaining driver integration (STOP boundaries)

Canonical copied-build launch/admission, reviewed menu/clipboard actions,
single-instance deep-link ingress, same-head reopen, navigation/refusal observer
and mode-specific cleanup are not implemented by this contract slice. No generic
caller script, fixture mutation presented as product action, arbitrary clipboard
controller, ad-hoc coordinates, protocol registry edits or installed-app fallback.
Missing any operation remains #1808 gap/STOP. Do not request a VM slot for this
contract slice alone. #1788 still depends on the copied-build driver.

## Offline checks

`python3 -m unittest discover -s scripts/windows/qa-harness/scenarios -p 'test_*.py'`

The seventeen tests include mutation controls for unknown metadata, no-overwrite,
write-failure provenance, same-ID/new PID, mismatch navigation/metadata, both
positive controls, folder rename, link identity, exact head/frame hashes and path
escapes. Mock PNG bytes are deliberately labeled and are not native screenshots.
metadata-fixture.ps1's Windows sharing semantics remain native UNVERIFIED; parse
validation on PowerShell7 is not a Windows self-test.
