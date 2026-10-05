# Todoist Inbox picker (#230)

The Task view can browse an existing Todoist Inbox, inspect tasks by title and
details, and explicitly link one exact task to the current goal without copying
its external ID. Todoist remains task authority. Browsing and linking send only
GET requests; a link saves the existing `TaskBinding`/`TaskObservation` through
`task_save` and the canonical application checkpoint. No competing task catalog,
automatic import, background sync, remote mutation or new journal field is added.

## Provider contract

Verified against current public Todoist documentation on 2026-09-08:

- [User Info](https://developer.todoist.com/api/v1/#tag/User/operation/user_info_api_v1_user_get):
  `GET /api/v1/user` supplies authenticated `id` and authoritative
  `inbox_project_id`. Never infer Inbox from a localized project name.
- [Get Tasks](https://developer.todoist.com/api/v1/#tag/Tasks/operation/get_tasks_api_v1_tasks_get):
  `GET /api/v1/tasks?project_id=<inbox>&limit=50[&cursor=...]` lists active tasks.
  The response has `results` and `next_cursor`; null means the final page.
- [Pagination](https://developer.todoist.com/api/v1/#tag/Pagination): cursors are
  opaque, account-specific and tied to the same filters/page size. Concurrent
  changes may duplicate or omit rows; this is an observation, not a transactional
  provider snapshot.

The saved connector/account pin is reused. A picker page reads User Info through
its captured adapter and requires the same saved account and Inbox. A selected
link checks that account and Inbox again and reads the exact task. Both user and task
responses use the existing 1 MiB parser bound and typed HTTP/transport errors;
there is no automatic retry or separate authentication flow.

## API

Capability `todoist_inbox_picker:true` advertises protocol support; the existing
`todoist` capability and connection state identify provider availability. Picker
requests require `ai-brain/workspace-v1` and the exact `expected_workspace`.

`todoist_inbox_list {goal_id, session_id?:string}` starts a new session when the
session ID is omitted/null. Supplying the returned ID explicitly loads the next
page. It returns:

```json
{
  "schema": "tessera-todoist-inbox/v1",
  "session_id": "opaque session UUID",
  "goal_id": "explicit goal UUID",
  "account_id": "verified saved Todoist account",
  "inbox_project_id": "authoritative provider Inbox ID",
  "items": [{
    "task_id": "exact task ID",
    "content": "Task title",
    "description": "Task details",
    "due": {"date": "2026-09-09", "is_recurring": false},
    "labels": ["example"],
    "priority": 2,
    "url": "https://app.todoist.com/app/task/exact-task-id"
  }],
  "complete": false,
  "can_load_more": true,
  "limit_reached": false
}
```

Items are cumulative in first-seen provider order and deduplicated by exact task
ID. Later duplicate rows never replace the first frozen observation. `due` can be
null; its optional `string` is the provider's human date text. Priority retains
provider semantics. The URL is generated using the existing official task-link
format; it is not a provider-supplied field.

`task_link {goal_id, task_id, picker_session_id?:string}` keeps its existing
`TaskReply` shape. The native picker supplies the session ID. The backend requires
that exact task to exist in the frozen session, reads it again, and rejects changed
typed task fields, deletion/completion or departure from the observed Inbox. A
new observation timestamp alone does not make the task stale. Known-ID callers
can omit the session ID and retain existing completed/deleted/non-Inbox semantics.
When a saved account pin exists, direct linking verifies it without requiring an
Inbox ID. Task views add `provider`, `instance_id` and `goal_id` from the actual
persisted binding. They do not invent a historical `account_id` absent from that
binding; the account pin remains in saved connector settings.

## Bounds, errors and ownership

One ephemeral session is retained per backend, with one request in flight. A
refresh replaces it. Bounds: 50 tasks/1 MiB per provider page, 500 unique rows,
10 successful pages, 8 MiB per session (each row charges the larger of serialized
typed Task or exact wire item, including duplicated ID in its URL), 4 KiB cursor.
User/Inbox identity is bounded by the 1 MiB User Info response, keeping the full
cumulative reply below the ordinary 16 MiB RPC bound.
Reaching a bound retains the observed rows and returns `limit_reached:true`,
`complete:false`, `can_load_more:false`; an exhausted cursor reports complete.
An empty nonterminal page remains incomplete. Repeated/malformed cursors and
malformed, overfull, other-project or inactive-task pages are errors. Rows/cursors
are not persisted or fetched automatically.

A failed page retains earlier rows, cursor and counters and releases the in-flight
permit, allowing explicit retry. Failure never clears the current task or native
draft. Distinguish loading, complete-empty, partial, bound-hit, stale selection,
auth/permission, provider unavailable, rate limit and malformed response. Errors
use `code,message`; `todoist_provider_error` additionally includes
`todoist:{kind,http_status,retry_after_secs}`. Local codes include
`todoist_selection_stale`, `todoist_account_changed`, `todoist_account_unverified`,
`todoist_unavailable`, `todoist_configuration_changed`, `todoist_picker_busy` and `todoist_pagination_invalid`.

Capture workspace identity, explicit goal, existing task generation, current and
saved configuration, saved account pin, connector epoch and session under the
owner lock. Existing saved/active configuration drift refuses before the first
GET. Release the owner lock for provider GETs; recapture and compare before installing
rows or committing the link. Successful reconnect invalidates the epoch/session,
including a reconnect to equal settings. The known-ID service path also performs
its GET outside the lock. A backend cannot observe native navigation that emits
no request: the native controller must invalidate its request epoch on goal or
workspace navigation and reject late display; the server always routes the
explicit goal and refuses mismatched sessions/generations/configuration.

Successful linking consumes the session before the network-free checkpoint.
Concurrent duplicate selection is refused while busy, and later reuse is stale.
If the checkpoint durably accepts the link but canonical source projection fails,
the original pending writes and binding remain recoverable; the old selection
cannot create a second link. Recovery/reopen uses the existing source intents and
retains mutation/reconciliation history. Restart discards picker state but keeps
an accepted task binding.

## Verification scope

Focused fixture tests cover duplicate-title selection, pagination/first-seen
freezing and explicit retry, bounds, stale task/target/account/session, owner
responsiveness during delayed list/direct/picker GETs, known-ID compatibility,
concurrent duplicates, durable pending source recovery and reopen. Provider method
capture has a fixture-only POST positive control and verifies the product flow
sends GET only. Native acceptance must additionally exercise browse → inspect →
select → link → reopen/Refresh with no ID copying and reject navigation-stale UI
responses. No authenticated live Todoist acceptance or runtime configuration
change is implied by source tests.
