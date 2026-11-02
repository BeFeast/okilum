# Direct guarded bidirectional regression — 2026-11-01

This is executed Linux code/data regression evidence, not native macOS acceptance,
full-product validation, a release admission, or permission to change live data.
The previously failed broad baldr permission-fixture harness remains stopped.
This independent focused packet did not repair or rerun that harness.

## Source and execution

Integrated baseline: PR #2218 head `0d1fbe098a781b689a6f5487c3083fe422e35470`.
The implementation checkout safely merged that source at `607cd33de`; parent
Controller configuration access and bootstrap hooks were retained. Only the direct
protocol, transfer guards, protocol tests and documentation changed afterward.

Packet on baldr: `/home/god/work/qa-evidence/sync-v2-bidirectional-0d1/`.
`source/` is an isolated three-crate workspace containing the controller, Syncthing
boundary and network capability crate; it has no GPUI/vendor or installed-app
integration. Tests used the pinned repository Rust toolchain, `nice -n 19`, four
Cargo jobs, `/home/god/.cache/okilum-fast/build.lock`, and the repository nofile
helper. No Rust build/server ran on maestro. No CI route or dispatch was changed.

Executed commands:

```sh
cargo test --offline -j4 -p okilum-sync-controller --lib direct:: -- --nocapture
OKILUM_SYNC_DIRECT_SOURCE=<packet>/syncthing-linux-amd64-v2.1.7/syncthing \
OKILUM_SYNC_DIRECT_CLIENT=<packet>/syncthing-linux-amd64-v2.1.6/syncthing \
  cargo test --offline -j4 -p okilum-sync-controller --test direct_bidirectional \
  -- --ignored --nocapture
```

Results: **9 focused tests passed**, 189 unrelated library tests filtered out;
**1 actual two-daemon ignored regression executed and passed** in 8.65 seconds.
Compile phases took 7.17 and 5.12 seconds on this same host/session. The log is
`validation.log`, SHA-256
`15ceef126cdb3cad770678215debc99a99ef00b90b935d5c63983d06048b8ac0`.

Tested source SHA-256 values, relative to the repository:

| File | SHA-256 |
| --- | --- |
| `crates/okilum-sync-controller/src/direct/mod.rs` | `d7e43f66ae592c5b29601a2252b51c473028934fe6835adfd2f2b6a2648f8c14` |
| `crates/okilum-sync-controller/src/direct/transfer.rs` | `6c274596a102c925daba3ee26840be85b74ab8f9c2b77a89cb9ad5d2830e4416` |
| `crates/okilum-sync-controller/tests/direct_bidirectional.rs` | `078583955205c4ce8bd035cc1fbc82c64dc7ec8b0f3f51c112ab59eef1507cbe` |
| `crates/okilum-sync-controller/tests/support/direct_daemon.rs` | `e036b1c2f136df88d3b887a76c69e061754ff90fb94a646cee0632428c005dc4` |

After this execution, the only source tidy was removing the unused `Syncthing`
import from `direct/transfer.rs`; the three other tested files are byte-identical.
The test log also reports an existing unrelated dead-code warning in the macOS
bundle helper; it was not changed. Documentation was updated after execution.

## Disposable daemon provenance and isolation

Unmodified official Linux amd64 release archives and `sha256sum.txt.asc` were
fetched over HTTPS from the corresponding `syncthing/syncthing` GitHub releases.
Each archive matched its published checksum before extraction. This is checksum
validation against the HTTPS release asset, not a claim of independent signature
verification. REST independently confirmed source **v2.1.7** and receiver **v2.1.6**.

| Version | Archive SHA-256 | Binary SHA-256 |
| --- | --- | --- |
| 2.1.6 | `524ef4e1df1850b719e2378c8369956f4e5f2ab9a46fdf7f45f2b925012147aa` | `d72663e4f9efe0b8de5e1dacb13c1c8decc82e76623cf2fa47fd306f3e1d3a3e` |
| 2.1.7 | `2edc416e7ed136b7c858ed5f3f02a3b8316589605e54c380228ceea7acbf0142` | `f5c12679f2bad1b5eff619ba7b04ead1a457e439a1e55dc640d4d8995d7f4dbd` |

The regression generated two temporary homes, credentials, certificates and vaults;
all listeners were loopback and discovery/relay/NAT/upgrades were disabled in those
private fixtures only. Existing devices were addressed through fixture-owned
static transport endpoints. Children were stopped/reaped by fixture cleanup.
No user brain, daemon, peers, global configuration, installed app or service was
read for selection or modified. No second live daemon was started.

## Exercised boundaries

- Real encrypted URL pairing; old confirmation displays/binds the receiving
  Syncthing identity; authorized catalog precedes first receive.
- Unknown nonempty destination preserves its sentinel and does not mutate the
  source share. The empty destination starts receiveonly automatically.
- A subdirectory plus initial note is a positive control for 2.1.x counters;
  subtracting legacy 128-byte directory counters would fail this case.
- Local receive-only edits remain intact and hold promotion despite successful
  initial download and an authenticated connected peer.
- Receiver daemon and controller restart with a durable Promoting intent but no
  successful PATCH remain receiveonly until new direct observations are ready.
- Refresh automatically promotes the exact owned folder; both computers then
  create files and edit an existing note, with content verified at the other peer.
- A remote overwrite retains the previous actual note content under `.stversions`;
  the receiver retains its configured 30-day history. Existing source mode, label
  and versioning configuration remain unchanged.
- Lost successful PATCH acknowledgement recovers through exact owned read-back,
  even when the source application is unavailable. It does not repeat promotion.
- An unrelated existing receiveonly folder and its file/configuration remain
  untouched. Legacy records without ownership fields stay on a visible hold.
- Pure guard tests reject missing counters, local delta, daemon/controller
  generation changes, unexpected mode/label changes and replaced directories.

Concurrent conflict generation, deletion propagation, native macOS bootstrap,
listener restoration after source-process restart, symmetric catalog serving and
full-product UI/native behavior were not established by this packet. Existing
native/data release gates remain mandatory; parent owns the next exact source
integration and admission.
