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
3. Compare canonical sorted JSON with `snapshot.json`. The snapshot covers the
   protocol-v2 subscribeShell, subscribeThread and dispatchCommand input/output,
   descriptor capabilities, server auth and pairing exchange contracts. The
   committed `baseline.json` identifies its source version. Diff and actual
   schema remain workflow artifacts, even on failure.

Enabled after #1805: the actual `okilum-t3` example calls `NativeToken::pair`
and `NativeClient::subscribe` for an initial thread snapshot, then reconnects
with the committed `ResumeCursor`. It requires the completion marker on both
connections, exactly one initial snapshot, no replacement snapshot on resume,
and an unchanged idle cursor. An unexpected domain event fails visibly instead
of being silently skipped. Each subscription has a 30-second cancellation
watchdog, and the complete client subprocess has a 90-second ceiling. The
receipt records source commit and actual compiled client binary SHA256.

Fixture setup is separate: official T3 CLI issues a temporary MCP setup session,
and MCP creates one scratch thread **without message or attachments**. Its
`runId` must be null: no provider/model run is requested. This is only server
state setup, never evidence for native shell/dispatch operations. A fresh
one-time pairing credential is passed to the Rust example over stdin; Python
no longer substitutes its own token exchange for the client pairing call.
The server, fixture and all credentials disappear with the throwaway home.

**Deferred until future merged client APIs:** native subscribeShell,
dispatchCommand/send/interrupt/approvals and capability-flag runtime checks.
The schema snapshot continues to cover shell/dispatch/descriptor contracts.
No production-client API, UI code or raw WebSocket substitute was added.
Only exposed real-client methods count as round-trip evidence.

Failure or drift opens/updates one marker-labelled `t3-contract` issue in the
GitHub mirror, using the normal Actions token. A later failure reopens the same
incident if it was closed; concurrent runs are serialized. The issue includes
T3 version, bootstrap/schema outcome, run URL and a bounded diff excerpt; the
artifact contains the full diff. No Actions run polling or account token is
used. Incident publication errors fail this standalone job visibly. Normal
protocol drift is retained as FAIL in the result/summary and incident, while the
probe step is report-only. No other CI gate is changed.

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
`baseline.json`. First real hosted native-client execution remains to be observed
after installation; static/unit and normal PR compilation are distinct evidence.
No server or cargo build/test was run on maestro during implementation.
