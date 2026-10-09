# Recoverable Maestro operation dispositions

Follow-up [issue156](https://git.oklabs.uk/BeFeast/okilum/issues/156), stacked on
[PR155](https://git.oklabs.uk/BeFeast/okilum/pulls/155). This extends local link and
unlink recovery; it introduces no Maestro control operation or execution framework.

## Durable meaning

Before a link GET or an ambiguous local mutation, persist the exact typed request
and a pending intention in the existing Maestro operation journal. Operation IDs
remain bound to one goal, one kind and one exact request. Different input under an
existing ID is refused and cannot replace its disposition.

| Status | Meaning | Native action |
| --- | --- | --- |
| `committed` | The matching local mutation and its receipt are durable. Canonical source projection may still need normal journal recovery. | Consume only the exact matching receipt; refresh linked work/source status. |
| `rejected` | A durable terminal disposition prevents this request from committing later, including an already-running GET. | Clear the exact pending request; rediscover or start a new operation if desired. |
| `pending` | The exact intention is durable but has no committed/rejected result. | Retain the request; query, retry the same ID/input, or explicitly abandon it. |
| `unknown` | No disposition can establish the operation's outcome. | Retain the request; absence alone is never proof that work did not start. |

No rejected disposition is inferred from a generic transport error, a missing
receipt, or an error string. Network failures retain pending intentions. Restart
does not silently retry or abandon them. Successful receipts replay offline.
Maintenance retains query/abandon/replay and the full disposition schema while
disabling new links. Pending intentions survive a maintenance restart; an explicit
retry under maintenance records terminal `new_links_disabled` before any GET.
Querying or abandoning an intention never requires provider availability.

## Query

The usual schema, request ID and expected workspace envelope applies:

```json
{
  "op": "maestro_operation_get",
  "operation_id": "02000000-0000-4000-8000-000000000156",
  "goal_id": "03000000-0000-4000-8000-000000000156"
}
```

Data has this shape:

```json
{
  "schema": "tessera-maestro-operation/v1",
  "operation_id": "02000000-0000-4000-8000-000000000156",
  "goal_id": "03000000-0000-4000-8000-000000000156",
  "kind": "link",
  "request": {},
  "status": "pending",
  "receipt": null,
  "rejection": null
}
```

`request` is the complete original LinkRequest or UnlinkRequest body, without the
transport envelope or `op`. Native recovery compares it structurally with its
saved request. `kind` is `link` or `unlink`; both it and `request` are null for an
unknown ID. `receipt` retains the existing link/unlink receipt shape. A rejection
has a stable machine `code` and a human `message`.

## Explicit local abandonment

```json
{
  "op": "maestro_operation_abandon",
  "kind": "link",
  "request": {
    "operation_id": "02000000-0000-4000-8000-000000000156",
    "goal_id": "03000000-0000-4000-8000-000000000156",
    "selection_guard": "sha256:the-original-guard",
    "project_id": "04000000-0000-4000-8000-000000000156",
    "project_name": "example-project",
    "repo": "example/project",
    "issue_number": 42
  }
}
```

For unlink, use `kind:"unlink"` and the original UnlinkRequest body instead.
The reply is a disposition with the exact request. Abandonment atomically records
`rejected` with code `abandoned`; an already committed matching receipt wins and
is returned as `committed`. Repeating abandonment is stable. The rejection can
also be recorded for an unknown operation ID so a delayed first request cannot
commit afterwards. If durable rejection cannot be recorded, no safe-abandonment
claim is returned. This never cancels or changes remote Maestro work.

## Existing commands and errors

Successful `maestro_link` and `maestro_unlink` replies retain their current receipt
format. Their errors add `error.maestro_operation` containing the current durable
disposition when the exact request is identifiable. Native code must use that
structured field or the query command; it must not classify error prose.

Fresh-selection, configuration, goal-link-history and stale expected-link
refusals become durable rejection dispositions before a definitive rejection is
reported. If persistence fails, the status remains pending or unknown. A source
projection error after a receipt was durably committed reports the committed
disposition, preserving normal source/journal recovery instead of replaying the
local action under a new ID.

The final maintenance build must preserve these dispositions. Older binaries
that only understand successful Maestro operation receipts are incompatible once
this operation schema is enrolled; the enrollment guard must refuse that downgrade.

## Required evidence

Use real held GET fixtures with positive request controls: persist intention,
query pending, abandon, release the GET and prove no link appears; restart and
prove the rejection still prevents a late/retried commit. Also prove committed
receipt wins after lost reply, same-ID changed input is refused, unlink receipt
replays after restart, and offline query/abandon require no provider request.

## Verification

The focused Maestro suite covers held GET abandonment, configuration/history
refusal, network-pending recovery, exact wire validation, restart/tombstones,
committed-receipt precedence and interrupted canonical source projection. The
full brain suite and cored integration suite include existing connector, source,
Inbox, Attention and export contracts.

`verify-maestro-dispositions.py` executes two actual binaries against an isolated
brain and a local GET-only fixture; optional `--predecessor` verifies that the
successful-receipt-only binary refuses the upgraded enrollment before changing
the journal. `verify-maestro-maintenance.py` separately preserves the existing
linked observation, generation, canonical source and offline receipt contract.
