# Linux live Manager projection (#1787, slice 3)

Linux can enable live updates with its own T3 native-client pairing. The saved
native session is origin-bound under `/ws` and reused on restart/reconnect;
expiry, revocation and incompatible protocol remain explicit. The existing
Manager connection and durable send journal continue serving Send until slice 4
moves actions to native dispatch. macOS keeps its embedded web UI.

The native background worker holds one live subscription, applies complete
chunks atomically to a retained projection before ACK, and reconnects with its
committed cursor. Network backoff defaults to 1/2/4/8/15 seconds (the local
`delay` policy); authorization/protocol errors require explicit reconnect or
pairing. Closing the Project view cancels the socket and owned task. Epoch
checks prevent an old receiver from mutating a newer view/session.

Projection/visibility follows upstream `orchestrationV2Projection.ts`,
`orchestrationV2Timeline.ts` and `threadHistoryMerge.ts` (audited export
71a283aed2). Local rolled-back runs, cancelled queued input and superseded
plain-steer interrupt results stay hidden. Inherited rows keep their source
thread/item keys. Bounded-window watermarks prevent streamed updates from
reintroducing unloaded old history; unfinished queued runs remain visible.
Unknown event/item types are skipped without discarding the known thread.

Rows retain stable identities and share unchanged allocations. A streaming
text flush changes one row rather than cloning the conversation. The existing
MessageScroller keeps tail-follow and the reader's scroll choice. Reasoning
starts collapsed; command/file/tool activity uses compact summaries. Full
activity, approval/question controls and other unsupported actions open T3
until the actions slice supplies their native controls.

Older history uses the server's opaque cursor and protocol-v2 HTTP endpoint.
The page merges into the current model, preserving any row already updated by
the live stream and avoiding resurrection of hidden rows. A response belonging
to a replaced snapshot or old cursor cannot change the new timeline. Partial
snapshot budget metadata remains visible instead of implying complete history.

The hosted contract probe (#1789) already exercises the public native pairing
and subscription APIs. These new projection/controller paths add focused Rust
and GPUI regressions; live-server/human acceptance is a separate observation,
not implied by loopback tests.
