# Future T3 target generations

The explicit future-target selection flow chooses a different T3 origin,
environment or project for **new stages in the same Brain**. It does not reconnect,
rename or migrate historical execution identity. Discovery is not consent.
Generic connector Save/Reconnect retain their existing routing guard.

## Authority and history

The Runner journal owns immutable generations, an active generation, append-only
operation associations and transition receipts. Each generation freezes the entire
T3 settings object, including credential **reference**, model/provider and execution
modes; no credential value is persisted. After adoption, changing those settings
requires another explicit future-target adoption. Generic Save/Reconnect may retain
the original connector settings and reread their unchanged references, but cannot
silently modify the active generation. Connections displays the effective future
route separately from this legacy connector configuration.

Legacy dispatch envelopes, operation/stage/goal/thread IDs, bindings, sequence and
replay cursors, immutable receipt files and saved outcome evidence URLs stay
unchanged. Existing application pins are not filled in or rewritten. New T3 stage
preparation associates its new operation with the active generation in the same
journal commit that persists the prepared intent. Retry/reconcile/poll resolve the
operation's own generation, never whichever endpoint is currently selected.
Historical links use that same route. A new stage gets a new operation and thread;
its goal identity stays the same.

Production startup (explicit configuration or saved settings), connector Save and
Reconnect preserve a missing provider pin when the selected goal already has
retained work. Current connection settings cannot supply that missing historical
origin. Before generation adoption, historical thread links and calls through a
configured legacy T3 connector require a retained matching routing pin. An empty goal receives its initial pin with its first provider intent (discussion
or task) or before its first legacy T3 preparation,
without changing another goal's unknown history. Existing prepared envelopes never
qualify as empty. Future generation associations remain
independent of that legacy pin.

Known historical routes require a consistent retained routing pin and terminal
proof. A missing historical origin can qualify only for
`historical_terminal_unroutable`: exact envelope/binding/immutable receipt and
persisted result linkage must still verify, with no pending provider, correlation,
source projection or acknowledgement. This explicit classification allows local
history only. It never gains a fabricated origin, inferred thread URL, network
adapter, reconcile, retry or fallback to a current route—even if the local receipt
later disappears. New work always receives a known route.

## Settled-work admission

Preparation inspects current and retained dispatches across every goal, including
unrelated empty goals. Active/prepared/submitting/indeterminate or unsupported
phases, running conversations, missing/conflicting terminal evidence, unresolved
correlation and pending source projections block adoption. A bare cancelled status
is insufficient: a correlated terminal outcome receipt and saved projection are
required. Unavailable original transport is not terminal evidence.

A durable `outcome_ready` result may still need human criterion/evidence review.
That UI review does not become provider uncertainty. Source review may change
supported evidence statuses, criterion evaluations and verification, and may append
human-acceptance evidence with its own durable linkage; it cannot rewrite provider
summary, sources or evidence identity. Adoption neither marks that review complete
nor certifies the result's quality. A later review can replace a criterion's
selected evidence while leaving earlier evidence statuses unchanged. The present
source schema has no append-only history for those superseded selections;
admission checks supported statuses and a valid owning saved evaluation, and does
not invent missing review provenance.

The candidate must be reachable through supported discovery and advertise the
exact candidate environment and existing selected project. A different verified
environment is a valid future target. Ancestry is optional provenance, not a
requirement or proof that the environments are equivalent.

## Typed API and explicit consent

All three operations require `ai-brain/workspace-v1`, a nonempty request `id` and
an exact `expected_workspace`. Unknown request fields are refused.

- `t3_target_get` returns `okilum-t3-target/v1`, the effective `active` T3 settings,
  `active_generation`, immutable `generations`, durable
  `historical_terminal_unroutable_count` and `future_only: true`. The count keeps
  local-only history visible after refresh and restart.
- `t3_target_prepare`, with `candidate: T3Settings`, performs read-only discovery
  outside the owner lock. Its typed review includes exact candidate, previous
  settings, identity differences, proposed historical associations and blockers.
  The guard pins operational revision, active generation, inventory digest and
  candidate digest. Preparation persists nothing.
- `t3_target_adopt`, with `request: {operation_id, review}`, explicitly accepts the
  exact reviewed selection. It repeats discovery and rechecks every guard under
  the owner lock. No thread, turn or provider job is created. A stale/blocked
  selection is refused before journal persistence.

Adoption returns a request-bound `okilum-t3-target-outcome/v1` with exact workspace,
request and status. `committed` carries a `okilum-t3-target-receipt/v1` receipt
(operation ID, generation ID, prior generation, request digest, future-only flag).
`not_applied` is a certified refusal for that request. Transport failure remains
unknown; the client retains and resends the **identical** request. A committed
identical duplicate returns the original receipt before stale-state checks,
including when another identical request commits during discovery. Reusing its ID
with a changed payload conflicts. A failure whose durable outcome cannot be read
must not return either a speculative receipt or a certified refusal.

## Persistence, restart and downgrade

Selection, association and transition receipt are one atomic journal replacement
with file and directory synchronization. Connector JSON is not a second authority.
A fault before commit leaves the previous generation; a fault after commit is
resolved by reading the recorded receipt. An unreadable durable outcome fences
further journal writes until recovery/restart. Startup does not resend provider
work because a selection changed.

The first adoption writes the journal under the `okilum-runtime/routes-v2`
wrapper. Older binaries require legacy root fields absent from this wrapper and
therefore fail closed. New code decodes legacy journals and this explicit version;
unknown versions are refused. Enrollment checks for unrelated Attention, Inbox and
Maestro journals use the same version-aware decoder. No derived index owns routing.

After new external work, only an explicit future selection transition retaining
both histories is supported; it does not undo external work. Restoring an old
`state.json` would discard durable identities and is not supported. An old executable unable to read routes-v2 is not a
supported deployment rollback after adoption.

## Read-only local eligibility inventory

The standalone tool exercises the same local terminal-admission verifier without
opening a Runner, recovering state, resolving credentials or calling a provider:

```sh
CARGO_TARGET_DIR="$HOME/.cache/okilum-recovery" cargo build --locked \
  -p okilum-brain --example t3-target-inventory
"$HOME/.cache/okilum-recovery/debug/examples/t3-target-inventory" \
  --operational /private/brain-state --brain /private/brain
```

It emits typed associations, fixed blockers and an inventory digest, without source
content or credentials. Exit 0 means local history is eligible; exit 2 means refusal
or unreadable/changed input. It does not prove candidate connectivity, authorize
adoption, install a service, send a prompt or certify desktop acceptance. Exact live
install/adopt, consistent backup and verification require a separately reviewed
operator package.

## Bounded historical serializer compatibility

A receipt whose fingerprint differs from current serialization is refused by
normal validation. An explicit `compatibility_manifest` on `t3_target_prepare`
may supply original historical bytes; the inventory example accepts the same
manifest with `--proof-manifest /private/manifest.json`:

```json
{"entries":[{"operation_id":"<original-operation-uuid>",
"artifact_path":"/private/original-state.json", "artifact_sha256":"<sha256>",
"provenance":"<auditable-original-backup-reference>"}]}
```

Each bounded, pinned artifact must contain one unambiguous original compact
envelope span whose exact SHA-256 equals the existing receipt fingerprint. Strict
lossless known-schema equality must then establish the same current envelope.
Only object-member order is compatible: arrays, values, numeric token precision,
identities, optional-field presence, sources and target fields remain exact.
Unknown fields, duplicate keys, unsupported extensions, missing/defaulted members,
changed values or ambiguous spans refuse proof. The raw current journal is checked
before startup persistence can discard unsupported fields. No serializer guessing,
receipt fingerprint replacement or operational migration is performed.

The review reports proof hashes, provenance and source locators. Adoption retains
the verified original compact envelope with the immutable route transaction, so
restart and ordinary receipt reconciliation use the same production verifier
without requiring the external artifact file. Proof supplies no transport origin
or environment ancestry. Unknown-origin associations remain permanently local-only
and cannot obtain an adapter or credential lookup from the current generation.

## Production startup regression

`cargo test --locked -p okilum-cored --test t3_origin` launches the actual cored
entrypoint on isolated settled-history fixtures. It covers explicit configuration
and saved settings, reconnect, unknown and known origins, preparation, adoption,
restart, immutable evidence and forbidden operation calls. A separate synthetic
provider instance supplies the detector's positive control. Checkpoints capture
state before startup and after startup, preparation, adoption and restart.

Set `OKILUM_CORED_TEST_BINARY` to an absolute executable path to run the same
regression against a frozen release binary. Development test success does not
qualify a deployed release; a future integrated backend artifact must pass this
actual-entrypoint regression at its exact build before acceptance.
