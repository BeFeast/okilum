# Tessera P1 connector: wire and ownership

Implementation brief for [ok-gobot #80](https://git.oklabs.uk/BeFeast/ok-gobot/issues/80)
and [Tessera #134](https://git.oklabs.uk/BeFeast/tessera/issues/134). This document
freezes the implemented server transport boundary and matching client contract. Code and isolated
tests only; live transport and deployment belong to Tessera #127.

## Transport and authority

Use a separate, disabled-by-default loopback JSON-lines listener sharing the
existing Tessera `Arc<Mutex<Backend>>`. A fixed operator-controlled tunnel carries
this port to the bot host. Never send connector traffic to the native port, fall
back to native on authentication failure, invoke SSH per request, or expose the
unauthenticated native listener.

Server configuration pins: connector ID, token file, canonical workspace identity,
source instance ID, bot account ID, canonical actor ID, allowed Telegram sender ID,
and allowed `(chat_id, topic_id)` routes. The client pins the same workspace and
connector identity. Secret values are loaded at transport time, never stored in a
mutation payload, source Markdown, export, or log. Pending payloads bind a public
configuration fingerprint so a changed account/brain/route cannot retarget them.

A connector request uses a distinct schema and nested command:

```json
{
  "schema": "ai-brain/connector-v1",
  "id": "request-uuid",
  "expected_workspace": {"brain_id":"brain-uuid","root":"/example/brain","records_dir":"records","managed":true},
  "connector": {"id":"operator-configured-id","token":"loaded-at-transport","policy_fingerprint":"lowercase-sha256"},
  "telegram": {"sender_id":"123","chat_id":"123","topic_id":null,"message_id":"456","update_id":"789"},
  "command": {"op":"inbox_capture","operation_id":"mutation-uuid","text":"Original exact body"}
}
```

Read requests require the authenticated sender and route; `message_id` and
`update_id` are required only for mutations. Sender/route fields come from verified
Telegram updates, never tool arguments. The server checks sender and route before
reconstructing source identity: channel `telegram`, configured instance/account/
actor, validated chat/topic and update/message. Requests cannot supply `source`,
`channel`, actor/account/instance overrides, or arbitrary native commands.

Allowed operations: capabilities, inbox_list/get/capture, attention_list/get/reply/
ack. Attention reads always use the configured actor and `telegram` channel.
Capture and mutation bodies otherwise retain #124/#125 types; nullable `stage_id`
is explicit for reply/ack. Response correlation and `ok/data/error` retain the
existing service contract, including structured `attention_stale.current`.
Capabilities reveal only the connector's allowed operations and pinned workspace.
Unknown commands/fields, wrong or missing authentication/context, wrong workspace
or route fail before canonical or operational mutation. Native behavior remains
unchanged. No engine Start/prepare/cancel/retry, arbitrary source write, criterion
acceptance, goal/task completion or connector reconfiguration is available here.

Runtime code should factor validated source authority at the existing validation
boundary. Native entrypoints keep native validation. The trusted connector owns a
non-forgeable internal authority value and calls the same capture/reply/ack
transaction implementation; it must not duplicate intents, SourceStore projection,
receipt recovery, alias reservation or attention observation logic.

## Frozen configuration, responses and errors

Tessera CLI adds only `--connector-config <operational-json-file>`. Absence means
no listener. The file has `schema: "ai-brain/connector-config-v1"`, `listen`
(loopback address), `connector_id`, `token_file`, `workspace` (exact object above),
`instance_id`, `account_id`, `actor_id`, `sender_id`, and `routes` (nonempty array
of `{chat_id, topic_id}` with explicit nullable topic). Configuration is immutable
for the process lifetime. Bad configured input fails startup rather than quietly
starting an unprotected listener. Token contents must be nonempty and are not
canonical state. Client config mirrors endpoint, token_file, connector_id,
workspace, instance/account/actor/sender and routes for its local authorization
checks and public binding fingerprint. Secret contents and token_file path are
excluded from the fingerprint, so credential rotation preserves authority;
account/connector/route/workspace changes do not. The server loads the token at
startup, while the client loads it per transport attempt.

Success is `{schema:"ai-brain/connector-v1", id, ok:true, data}`. Error is
`{schema:"ai-brain/connector-v1", id, ok:false, error:{code,message,...}}`.
Parser failure retains an available string request ID, but never echoes raw input.
New codes: `connector_invalid_request`, `connector_unauthorized`,
`connector_workspace_mismatch`, `connector_route_forbidden`,
`connector_operation_forbidden`, `connector_policy_mismatch`, `connector_unavailable`. Runtime inbox/attention error codes are unchanged,
including `attention_stale` with `current` item or null. A transport failure or
runtime error is not proof that a mutation failed to commit.

Capabilities data is exactly the following safe projection (booleans reflect
current recovery and managed state):

```json
{"connector_id":"operator-configured-id","policy_fingerprint":"lowercase-sha256","workspace":{"brain_id":"brain-uuid","root":"/example/brain","records_dir":"records","managed":true},"actor":"configured-actor","channel":"telegram","inbox_read":true,"inbox_capture":true,"attention_read":true,"attention_reply":true,"attention_ack":true}
```

The same mandatory envelope applies even to capabilities: no public fallback path.
Read command parameters match native operation parameters except `channel` is
forbidden. Mutation commands match native parameters except `source` is forbidden
and is constructed from the authenticated connector and trusted Telegram envelope.

## Cross-language public policy fingerprint

Every request must carry `connector.policy_fingerprint`, checked against the
server policy before any observation or mutation. Capabilities include the same
`policy_fingerprint`. A server-only source account/instance/actor/route change
rejects old unsent and replayed requests with `connector_policy_mismatch`; it never
reinterprets the original payload under changed authority. A client must retain
that failure with the original payload/configuration binding.

Compute lowercase hexadecimal SHA256 over this exact sequence of typed UTF-8
fields. A string is encoded as ASCII `s`, its decimal UTF-8 byte length without
leading zeros, ASCII `:`, its exact UTF-8 bytes, ASCII `;`. A null is ASCII `n;`.
There is no Unicode normalization, JSON serialization, escaping or map ordering.

1. `ai-brain/connector-policy/v1`, connector_id.
2. Workspace brain_id, root, records_dir, managed as `true` or `false`.
3. instance_id, account_id, actor_id, sender_id.
4. Route count as a decimal string.
5. Each route's chat_id then nullable topic_id, sorting routes lexicographically
   by chat_id and then topic_id (null before any string). Duplicate routes are
   invalid configuration, not silently deduplicated.

The shared [fixture corpus](fixtures/connector-policy-v1.json) contains config
inputs, exact encoded bytes in hex and expected SHA256 for ASCII, non-ASCII,
reordered routes, changed account, and unchanged authority with a different
transport endpoint/token path. Endpoint, token values and token_file paths are
excluded. The bot separately binds its local endpoint/config fingerprint to
mutation intents and outbox/reply bindings.

## Bot data and deterministic interaction

`internal/tessera` owns the typed client and connector coordinator, with an injected
transport for isolated tests. `/capture`, `/inbox`, `/attention` and bound ForceReply
handling run before provider inference. Provider outage must not prevent capture,
list/read, exact-target reply, seen acknowledgement, or durable retry.

`internal/storage/tessera.go` adds mutation intents keyed by immutable upstream
identity and normalized payload digest. Commit a generated operation ID and exact
nonsecret request before transport. Retry sends that same request after timeout,
lost response and process restart. A duplicate identity with different text/target
is retained as a conflict, not a fresh operation. Only `attention_stale` certifies
noncommit; preserve draft and old target for explicit fresh review. Other errors
remain failed/uncertain and never silently retarget.

Attention delivery uses the existing outbox lifecycle. Add a durable binding table
keyed by `(brain, actor, chat, topic, attention_id, revision)`, referencing an outbox
row and retaining exact goal/stage/revision plus account/connector public configuration
fingerprint. A changed account/connector configuration refuses old pending delivery
and reply bindings rather than replaying them under new authority. Enqueue row and binding in one SQLite
transaction. Existing `origin` alone is not an event identity. Existing outbox lacks
topic ID and reply metadata: extend delivery metadata and retry routing additively,
with defaults preserving all prior callers. Successful send records message ID and
exact reply binding atomically. Retain uncertain/failed sends; lost Telegram send
acknowledgement cannot prove exactly-once visible delivery. Do not guess a binding
for a message whose successful send was never observed.

A reply is resolved by delivered message ID plus account/chat/topic/sender against
its retained target. Wrong sender/route/item preserves text and cannot write a
replacement target. Saving a reply creates unverified decision Markdown; seen ack
marks only that actor/channel/revision. Neither resolves blockers nor controls an
engine. Polling actively delivers blockers, decisions and finals; ordinary progress
remains in summaries.

Typed tools share the coordinator; arguments describe content and selected item,
never authority. Existing `ChatScoped.BindChat` is insufficient: it carries no
sender, topic or immutable update identity. Bind trusted immutable per-turn context
at Telegram ingestion and carry it through the runtime to tool execution. Do not
read a mutable "latest session route" to recover authority. A missing trustworthy
context rejects mutation tools. Durable invocation identity must be assigned
before tool transport and reused on retry; exact update replay must resolve the
existing intent before a regenerated model response could create another capture.

## File ownership and sequence

1. Tessera server author: connector config/listener and service allowlist; narrow
   runtime/inbox/attention validation refactor; server integration tests. Owns
   `crates/tessera-brain/src/connector.rs`, service and runtime boundary changes,
   `crates/tessera-cored` optional config wiring, and canonical wire documentation.
2. Bot author: `internal/tessera/*`, `internal/storage/tessera*`, additive outbox
   metadata, `internal/bot/tessera*`, config struct/schema/default/env loader tests,
   tool implementation and immutable runtime context propagation. Existing approval
   callbacks, forum routing and unrelated model configuration stay intact.
3. Freeze actual wire shape and config field names with both authors, then implement
   server and client independently against shared JSON fixtures. Merge/review each
   source before preparation under #127. No bot runtime build or deployment here.

## Acceptance evidence

Positive permitted capture and reply with provider unavailable; native regression
suite; wrong/missing token, connector, actor/sender, workspace and route; omitted
credentials trying native fallback; forbidden action and injected source fields;
no state mutation on rejection; duplicate update/lost backend response across
restart; exact nullable-stage/stale/wrong-user reply; delivery failure/recovery and
forum-topic retry; outbox/mapping atomicity; existing command/approval regressions.
Tests use isolated listeners and synthetic Telegram updates. They are explicitly
not actual phone acceptance or production deployment evidence.


## Server verification receipt

The scoped listener, authority validation and existing transaction reuse are
implemented for #134. The full brain suite passes 142 tests, including twelve new
connector tests. These exercise a real loopback connection for capture/replay,
provider-free reply and channel-scoped seen state, lost capture/reply responses
across backend restart, native authority remaining separate, exact nullable stage,
stale/wrong goal ownership, rejected route/credentials/commands with unchanged
journal and source bytes, invalid configuration, server-only authority changes, credential rotation and
cross-language policy fixtures. Clippy all targets with
warnings denied and formatting also pass. No new operational journal schema is
introduced. The attention-preserving maintenance fallback retains these receipt
payloads; a future fallback unit must omit the unsupported `--connector-config`
flag while retaining its operational configuration and credentials. No live
listener, tunnel, credentials, bot release, or phone acceptance was provisioned.
