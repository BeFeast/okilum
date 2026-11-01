# Exact-source hosted Mac verification

This artifact-only route does not release PR2218's native/install HOLD. Production
shipping still requires main ancestry. No workflow here publishes a feed.

## Admission and source transfer

The trusted-main authorization file pins the exact internal PR number and source.
The Forgejo bridge verifies the current open internal PR head and exact Linux fast
success before transfer and again afterward. It transfers Git objects through a
bare repository to an invocation-specific `okilum-verify-source/` GitHub ref; no
product checkout, hooks or build scripts run with bridge credentials. This ref
does not match the existing `forgejo-pr/**` unsigned build trigger.

The existing bridge GitHub App records a commit-status attestation whose digest
binds repository, PR, source, producer, attempt, trusted tooling SHA, mirror ref,
fast result and issuance time. Hosted admission requires that App's dispatch
actor and authenticated status creator, the exact trusted-main pin, the matching
digest and mirror SHA, and evidence no older than two hours. Invalid/missing or
changed evidence fails before key import. API failure stops; there is no Forgejo
network fallback or substitution of main. The bridge App requires commit-status
write permission; the hosted job token has only contents/status read permissions.

Hosted source retrieval uses the authenticated GitHub mirror ref and verifies the
checked-out SHA. Product code receives the existing closed build environment,
without bridge/Actions/API tokens, key passwords or publication credentials.
Signing, notarization, package provenance, runtime version and artifact validation
remain required. Only `macos-verify-*` artifacts can result. Their receipts are not
production publication inputs or native bootstrap/install/bidirectional evidence.

## One future attempt (requires explicit new attempt authorization)

The earlier attempt18627/GH41327647562 stopped during hosted Forgejo admission
before key import or compilation. It must not be rerun or relabeled as new evidence.

After this correction is merged and mirrored to GitHub, independently read back:

1. The trusted-main pin and PR2218 head both equal
   `17ace291abaa5bc7c8a0f5f186c6e63d4e86728f`; required fast is success.
2. No queued/running/completed verify attempt already exists for the new authorized
   producer. Keep18627 as failed historical evidence, not a reusable artifact.
3. The existing protected bridge App can write commit statuses. If permission is
   absent, stop and identify that exact missing permission; do not alter credentials.

Then, only under a concrete new ONE attempt authorization:

```json
POST /repos/BeFeast/okilum/actions/workflows/macos-native-verify.yml/dispatches
{"ref":"main","inputs":{"verify_only":"true","pr":"2218","source":"17ace291abaa5bc7c8a0f5f186c6e63d4e86728f"}}
```

Record the original Forgejo producer and corresponding GitHub run/artifact digest.
Stop at the first failure; no rerun, second dispatch, native harness repair, M4,
installed app action, feed write or Stable promotion follows implicitly.

## Rollback

Stop issuing verify-only dispatches. Revert this route-correction commit to restore
the previous fail-closed route (whose hosted Forgejo reachability remains blocked).
Do not change production ancestry/publication requirements, running jobs, runner
services or user applications. An expired/abandoned invocation-specific mirror ref
and admission status grant no authority without matching trusted pin, App dispatch
and fresh evidence; their later cleanup is separate from this delivery.
