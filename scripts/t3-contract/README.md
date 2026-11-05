# T3 nightly protocol observation (#1789)

The independent GitHub `T3 nightly contract (report only)` workflow runs on
GitHub-hosted Ubuntu nightly or manually. It has no push/PR trigger and is not a
main or PR required gate. It does not use maestro, CT141, a real T3 home, an
installed desktop, provider credentials or a live user environment.

Current scope:

1. Select the newest published official `pingdotgg/t3code` nightly from one
   releases inventory. Download its Linux x64 server, verify the release asset's
   SHA256 digest, start headless on loopback with a disposable home/base directory,
   issue a throwaway pairing credential, exchange it and verify an authenticated
   session. Require the descriptor's version to match the selected release.
2. Download source from that same release tag. Install only its exact Effect
   runtime dependency in a separate temporary package, without lifecycle scripts
   or the upstream workspace dependency graph. Generate draft-2020-12 JSON Schema
   from the encoded upstream Effect contracts. No unsupported schema or absent
   export is replaced with a permissive placeholder.
3. Compare schemas semantically against the historical RPC `snapshot.json`
   (provenance in `baseline.json`) plus the separately attributed current3991
   decoded consumer snapshot. Removals, new requirements and narrowing fail;
   additive changes are informational. The original subscribeThread RPC output
   is deliberately unknown upstream, so it cannot protect stream fields. Export
   explicit stream-item and bounded-snapshot decoded schemas with `Schema.toType`
   as well, and reject opaque consumer roots. Their deterministic gzip baseline
   and exact source/Effect/digests are in `consumer-snapshot.json.gz` and
   `consumer-baseline.json`; decoded shapes do not substitute for wire runtime
   proof. Diff and actual schema remain artifacts even on failure.

Both hosted observation and the local updater gate now exercise the same real
`okilum_t3::stream::NativeClient` path. All five literal boolean proofs are
required: `pair`, `subscribe_thread`, `resume_thread`, `subscribe_shell`, and
`dispatch_command`. Thread and shell subscriptions must receive real completion
markers; a shell snapshot alone does not prove completion. Resume uses the
committed initial cursor, with no replacement snapshot or cursor change.

Fixture setup uses the matching official T3 CLI to issue an isolated MCP setup
session, then MCP creates one uniquely titled scratch thread **without a message
or attachments**. Its `runId` must be null; no provider/model run is requested.
The native client checks that exact scratch title and thread identity, empty
messages/runs, and a real protocol-v2 descriptor before any mutation. Its harmless
`thread.metadata.update` rename has the server's `expectedEmpty:true` race guard.
A new native snapshot must then contain the exact renamed title at a cursor at
least as recent as the command acknowledgement. An RPC acknowledgement alone is
not proof. Only the native client's public APIs count; fixture MCP setup never
substitutes for the shell or dispatch proof.

Each receive stage has a 30-second cancellation watchdog. The complete native
subprocess has a 150-second hard process-group deadline; setup and descriptor
workers are independently bounded. Child groups are killed and their leaders
reaped on timeout or interruption. Credential input uses stdin, never argv or
inherited environment. Child stdout/stderr remain private and are never included
in receipts or errors. Hosted server lifecycle remains confined to the disposable
nightly bootstrap, which terminates its server before removing private state.

## Local updater gate (existing candidate only)

The updater must create an **isolated candidate** state directory and start its
candidate server separately. Supply that candidate's matching verified official
server binary and exact exported schema, together with the current compiled
`nightly_contract` client example. This command never starts, stops, updates, or
replaces a server, touches a live vault, or launches a provider/model:

```sh
python3 scripts/t3-contract/probe.py \
  --url https://candidate.example \
  --server-binary /isolated/candidate/t3 \
  --server-base-dir /isolated/candidate/state \
  --candidate-schema /isolated/candidate/schema.json \
  --client-probe /build/examples/nightly_contract \
  --receipt /existing/private-directory/contract.json
```

The URL follows the native `Environment` policy: HTTPS origins, or HTTP only for
localhost or an explicit private/loopback IP. Credentials, paths, query strings
and fragments are rejected. The existing receipt parent directory must already
exist. The caller owns disposal of candidate state, including the idle scratch
fixture and temporary auth records; this harness does not delete server state.
It inherits no provider credentials, proxy credentials or live T3 configuration.

Exit 0 requires compatible schemas **and all five actual native proofs**;
otherwise exit 1 writes FAIL. Compatibility uses the default explicit consumer
expectations and the digest-validated historical plus decoded-consumer baselines
from `compatibility.load_baseline()`. Removed fields (including optional fields), RPCs or variants, newly required
fields, narrowing, and unknown constructs fail; safe additions remain informational.
The atomic mode-0600 JSON receipt includes the five booleans, sanitized descriptor
version, exact server/client/schema SHA256 digests, canonical merged-baseline
SHA256, and compatibility counts. It never contains credentials, peer exception
text, server logs or child output. PASS binds those exact inputs; it is not a
blanket approval of a different binary or schema.

Legacy direct Rust callers may still provide only `{origin, credential,
thread_id}` on stdin. They receive the old read-only pair/snapshot/resume proof,
with shell/dispatch false. They cannot satisfy either full gate. Both official
Python callers always supply the unique `expected_title` and require every proof.

Failure or drift opens/updates one marker-labelled `t3-contract` issue in the
GitHub mirror, using the normal Actions token. A later failure reopens the same
incident if it was closed; concurrent runs are serialized. The issue includes
T3 version, bootstrap/schema outcome, run URL and a bounded diff excerpt; the
artifact contains the full diff. No Actions run polling or account token is
used. Incident publication errors fail this standalone job visibly. Breaking protocol changes or runtime/harness failures remain FAIL in the
result/summary and incident; additive-only drift is INFO with PASS. The hosted
observation step remains report-only. No other CI gate is changed.

Never upload the disposable server home or server logs: startup may print
pairing credentials. The server process group is terminated before temporary
state is removed. Artifacts contain only version/digest/outcome/schema/diff.

## Updating the baseline

Review the full generated diff and the consumer compatibility implications;
never auto-accept it. Fetch the exact release source, install the matching Effect
dependency in an isolated package and make it resolvable at the upstream root.
Then generate (no server required):

```sh
node scripts/t3-contract/schema.mjs /path/to/upstream /tmp/actual-schema.json
diff -u scripts/t3-contract/snapshot.json /tmp/actual-schema.json
python3 -m unittest discover -s scripts/t3-contract -p 'test_*.py' -v
```

Update `snapshot.json` and `baseline.json` together in a reviewed PR. Metadata is
separate from schema so a version-only nightly change does not create drift.
The initial snapshot was generated from the official release tag recorded in
`baseline.json`. Static/unit and normal PR compilation remain distinct evidence from actual
hosted or isolated-candidate execution. A baseline update cannot substitute for
the full native proof.

Changes to the separately attributed decoded consumer baseline also require
review: regenerate both decoded roots from the exact inspected tag, use
deterministic gzip (mtime zero), and update its source commit, Effect version
and compressed/decoded SHA256 metadata together. Do not fabricate decoded
roots for the historical3814 RPC baseline.

## Execution hosts

Never build Rust/Okilum, run Rust tests, or start a T3 candidate/probe server on
maestro. Build the client artifact and run candidate servers/tests on baldr or CI.
On baldr build in an `okilum-slot` build slot: it sets `nice -n 19`, the Cargo
job count and the slot's warm target, so do not set your own `CARGO_TARGET_DIR`.
The old shared `build.lock` recipe is retired.
The `--client-probe` argument always names a **prebuilt artifact from CI/baldr**;
it is not a request to compile locally. This CLI never builds that artifact.

Run the full Python wrapper where the isolated candidate state and matching CLI
are available (baldr/CI). Maestro may run only the ready-made native probe artifact
against an existing candidate, using private stdin prepared by the candidate
owner; it must not start a candidate server or share/copy live server state.
The hosted nightly bootstrap belongs exclusively to CI.
