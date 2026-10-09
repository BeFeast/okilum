# Isolated T3 target acceptance fixture

This fixture uses the production Brain Runner and TCP service. A seed-only adapter
creates one fully settled synthetic historical operation with unknown origin; its
receipt, envelope, cursor, evidence and canonical records pass ordinary inventory
checks. A loopback fake T3 serves only discovery. Every other RPC is counted and
refused. This does not launch a GUI or access an existing brain/provider.

Prerequisites: Python 3 with `aiohttp`; the pinned Rust toolchain. Build the fixture:

```sh
cargo build -p okilum-brain --example t3-target-native-fixture
python3 scripts/t3-target-fake-provider.py --self-test
python3 scripts/t3-target-fake-provider.py --directory /absolute/fresh/fake
```

The fake prints its loopback URL and writes `candidate.json`/`observations.json`.
In a separate terminal start the real Brain service (use the actual Cargo target
path when `CARGO_TARGET_DIR` is set):

```sh
target/debug/examples/t3-target-native-fixture /absolute/fresh/brain http://127.0.0.1:PORT
```

The Brain fixture rejects existing directories and non-loopback endpoints. It
creates `fixture.json`, canonical records, durable runtime state, a public fixture
token file and `config/okilum/workspace.json`. The fixture's candidate uses this
file reference, so no environment credential or actual provider is needed.

Headless integration verification:

```sh
python3 scripts/verify-t3-target-native-fixture.py /absolute/fresh/brain
```

The probe verifies read-only preparation, a reviewed local-only historical
association, committed adoption, exact-request replay, the persistent local-only
history count, unchanged historical dispatch/events, and unchanged canonical
records/receipts. Its `probe.json` records results. Fake observations must show
only `server.getConfig` and `orchestration.subscribeShell`, with zero forbidden
calls. The fake self-test deliberately sends one forbidden dispatch as a positive
control in a separate temporary instance.

For native GUI acceptance, start another fresh Brain fixture. Its isolated
`XDG_CONFIG_HOME` is `/absolute/fresh/brain/config`; use an isolated HOME and the
approved GUI lease before launching the actual shell. In Connections, keep the
fixture URL/token/model settings, change Environment ID to
`synthetic-new-environment`, then explicitly review and adopt. Expect the
unknown-origin warning before adoption and the persistent local-only summary
after refresh. Verify that historical thread links remain unavailable. Keep
headless and GUI fixture roots separate so the GUI tests the actual adoption.

A headless PASS is service integration evidence only. GUI response-loss/restart,
focus and ordinary user-flow acceptance need their own recorded native run.
The production alpha apply/restart/deployment boundary is unchanged.
