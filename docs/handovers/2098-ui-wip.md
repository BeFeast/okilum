# Linux Manager workspace WIP (#2098)

Handoff to Editing, per manager instruction. This branch is preparation only:
no PR, no full-shell compilation or runtime acceptance.

Base: `918493c61a6d75c9706e61308e787fb6274e4aba` (PR #2095 Features, pending when prepared).
Integrate current main and retain the optional-feature lease guards before reuse.

Prepared code:

- `okilum-t3::stream::shell`: real WS v2 `orchestration.subscribeShell`, validated
  snapshot/update batches, ACK after apply, fresh snapshot on reconnect, explicit
  Manager/executor project filtering, archived/deleted removal, cancel handle.
  Shared RPC frame/heartbeat reader extracted from the existing thread transport.
- Linux `project::threads`: live/searchable virtualized thread picker, server run
  status, separate cached ProjectView/composer per origin+thread, return to declared
  Manager, no registry/manifest rewrite on selection. Child subscriptions stop on
  hide, origin change and Agents disable; lease revocation blocks new requests.
- Production ProjectView enables the new directory and uses WS for normal history
  and sends, retains existing per-thread durable action journal and Tier 1 controls.
  Legacy executor/model/launch controls remain on their old gateway surface, rather
  than pretending a native bearer is an MCP credential.
- Added stream-ready guard so retained action state cannot send before a fresh live
  response after reconnect. Existing manually constructed native fixtures explicitly
  mark their synchronized state; inspect these updates while integrating.
- Transport tests: scoped directory replacement, malformed atomic rejection,
  real ticket/WS/ACK flow and disabled-client no-new-request control.
- GPUI draft/selection test prepared in `project/threads.rs`; it uses a denied
  transport lease so it cannot connect, while transport positive controls are separate.

Validation: scoped rustfmt/diff checks PASS. Isolated small-crate
`cargo check -p okilum-t3 --tests` PASS on the transport sources. No shell check,
Clippy, executed Rust tests, live T3 pairing, native send or Linux UI PASS claimed.

Known work remaining:

1. Compile/check the full Linux shell on CT141/baldr/CI, fix type/lifecycle issues,
   execute stream and GPUI regressions. The new shell UI is not compiler-verified.
2. Review reconnect, thread-removal/read-only/expired status, separate drafts and
   retained commands, origin-switch lifecycle, and missing Manager pointer behavior.
3. Pair/verify on Linux with an isolated T3 project, never develop this task inside
   the product under test. Explicit Agents-off positive control must prove zero new
   T3/MCP/network connections. Existing transport and action scopes must not broaden.
4. Update the approved Manager docs, then PR + auto-merge per current process.

#2099 is separate PR #2103; its Linux UI check found a hidden-document scroll toast,
fixed in `51e219c6`. It is not part of this WIP. Build only on CT141/baldr or CI;
idunn is Oleg's active machine and accepts only a ready AppImage.
