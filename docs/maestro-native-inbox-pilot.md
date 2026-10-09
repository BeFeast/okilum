# Native Maestro worker Inbox pilot (#728)

Status: design and acceptance runbook, before implementation or live provisioning.
The accepted [#601](https://git.oklabs.uk/BeFeast/okilum/issues/601) pilot used a
dedicated durable consumer. This follow-up must show an answer reaching an actual
contained native harness worker. A successful bridge POST, a mailbox read by a
host script, or the existing consumer's acknowledgement cannot establish that.

The pilot uses one isolated project, one admitted worker attempt and one
question. The worker produces only a disposable, local result from the answer.
It receives no approval, merge, launch or deployment authority. Okilum's Reader
and real vaults do not participate. Maestro remains the execution owner; the
Okilum repository contains the design and Inbox-side evidence, not a substitute
native launcher.

## Audited source and implementation boundary

The source audit used Maestro main
`0812fd6159cf19b84fa84719aaafe84341c44f45` and these authoritative contracts:

- [Guarded Inbox bridge](https://git.oklabs.uk/BeFeast/maestro/src/commit/0812fd6159cf19b84fa84719aaafe84341c44f45/docs/inbox-bridge.md):
  worker grants bind one project, worker, generation and thread; grants load at
  server construction. `maestro question` supports publish/get/wait/withdraw/ack.
- [Native containment](https://git.oklabs.uk/BeFeast/maestro/src/commit/0812fd6159cf19b84fa84719aaafe84341c44f45/docs/native-role-containment.md):
  root-owned profiles bind the namespace, complete nft digest, runtime hashes,
  service lease, mounts and finite environment. Existing output permits only
  the managed inference listener and Forgejo, plus namespace-local loopback.
- [`containedNativeEnvironment`](https://git.oklabs.uk/BeFeast/maestro/src/commit/0812fd6159cf19b84fa84719aaafe84341c44f45/internal/aiexecution/containment.go):
  the generated child environment does not inject a question credential or
  endpoint. Ambient host variables are not a provisioning mechanism.
- [Okilum bridge](../inbox/bridge/README.md) and
  [`Bridge::from_config`](../inbox/crates/okilum-inboxd/src/bridge.rs): the Inbox
  deployment supports a T3 scope and one independent Maestro scope. It is not a
  general collection of arbitrary new Maestro scopes.

These are real source gaps, not an HTTP connectivity failure. The required
Maestro changes belong to its owner: finite mailbox endpoint admission,
per-attempt credential delivery, and recovery binding to the admitted native
attempt. Existing production profiles must continue to work without mailbox
access. No profile `build_environment` workaround, host database mount, generic
proxy or global firewall exception is part of this proposal.

## Frozen attempt and separate credentials

Before launch, the Maestro controller must durably bind the actual source
instance, stable project ID, worker ID, worker generation, source thread ID,
native session ID and service lease. The native session and source thread are
distinct identities. A probe cannot invent a generation merely to make an
authorization check pass. A new scheduler attempt receives new credentials and
identities; a consumer restart within the same admitted attempt retains them.

The operator provisions three independent credentials:

| Principal | Scope | Destination |
| --- | --- | --- |
| Native worker attempt | Exactly one project/worker/generation/thread; `publish`, `read`, `withdraw`, `ack` | Only the admitted native worker |
| Maestro question bridge | Exactly the isolated project; `read`, `reply`; no approval grants | Private bridge process beside Maestro |
| Inbox bridge | Exact owner/project/source-instance binding; ingest/replies, no launches or approvals | Private bridge process; authenticated Inbox machine boundary |

The worker receives the selected secret through the native launch's private
stdin envelope and generated environment, consistent with the existing native
credential mechanism. The proposed child variable is `MAESTRO_QUESTION_TOKEN`;
the CLI already reads that variable. Credential bytes never enter argv, prompts,
source files, evidence JSON, operation payloads or the unified source database.
The launcher redactor must use the same immutable selected secret snapshot.
Supervisor and reviewer roles receive no worker mailbox credential.

The proposed admission binds the endpoint, entire origin identity tuple and
credential selection to the existing native registration/lease. It must reject
missing, changed or mismatching bindings before child launch, including token
rotation between validation and execution. A caller-supplied endpoint or ambient
environment cannot override the pin. Credential digests establish selection,
not server authorization; live wrong-scope probes establish the latter.

Source grants currently require a restart to add/remove credentials. This pilot
does not propose a general grant-management API or hot reload. Its provisioning
and revocation require the separately approved Maestro maintenance window.
Principal identity remains stable while reconciling an existing operation.

## Containment endpoint

The Maestro owner supplies a worker-reachable mailbox listener or fixed local
transport. The child's `127.0.0.1` belongs to its own namespace, so the host's
loopback listener is not automatically reachable. Do not claim readiness from a
host-side `curl` alone.

For a routed listener, the new optional profile admission must pin one concrete
IPv4 destination and TCP port plus the configured mailbox origin. Only this
pilot profile receives the additional nft output rule; bind the complete
read-back ruleset digest as today. TLS is required outside a separately reviewed
trusted local transport. No DNS, arbitrary HTTP CONNECT, redirects, environment
proxy, Internet route or alternate port is admitted. Source authentication still
restricts methods/resources; the network rule alone grants no mailbox authority.
The listener must not expose the daemon's privileged dashboard or management
surface as a side effect of adding this route. Its finite routing and rejected
paths need independent live controls.

If the installed finite profile cannot support this without weakening the
existing boundary, keep the pilot held and change the source design. Do not
launch an uncontained worker to make the delivery demonstration pass.

## Native consumption and acknowledgement

The actual native worker publishes a Blue/Green question through the shipped
`maestro question` CLI with the frozen identities and a retained question ID.
It waits, receives the structured answer, then makes one explicit local
consumption transaction. Native transcript evidence must show the original
worker receiving the answer and initiating that transaction. A supervisor or
host helper must not perform these steps on its behalf.

The pilot's retained operation store is private durable state for the admitted
attempt, separate from deletable scratch/cache and outside any real vault. Its
path and lifetime must be admitted by the Maestro owner before launch. Preserve
it across the tested consumer restart; do not assume ordinary `/tmp` or scratch
survives. A single transaction stores:

- The complete source/question/attempt identity and original operation ID.
- The exact structured answer and its deterministic payload fingerprint.
- One pilot result derived from that answer, keyed by the same identity.
- A consumption receipt with commit time and the native session identity.

Use a uniqueness constraint for the full identity plus operation ID. Same ID
and same payload returns the original result/receipt; changed payload or changed
origin fails without another effect. The result and dedup receipt commit together
with durable SQLite settings, or an equivalent verified durable primitive.
There is no interval with a committed effect but no dedup record. The consumer
must validate the full mailbox identity before that transaction.

Only after successful durable consumption may the worker invoke `question ack`
with the original operation and currently observed opaque revision. A failed or
uncertain local commit sends no ack. After a lost ack response, read the exact
question again: `pending_operation_id` means ack is still outstanding;
`answered` plus the matching `answer_operation_id` means it succeeded. Do not
consume again or generate a replacement operation. If an ack revision changed,
re-read and validate the same reserved operation rather than inventing a new
answer. Missing/mismatching state is a hold, not proof of non-delivery.

| State | Evidence required | Permitted next step |
| --- | --- | --- |
| Question published | Exact native question identity and revision | Wait/read the same question |
| Answer accepted | Source `pending` with original `pending_operation_id` | Native receipt and local durable consume |
| Consumed | Committed result and dedup receipt; native transcript | Ack the original operation |
| Ack uncertain | Original consume receipt retained | Read/reconcile; retry the same ack only if still outstanding |
| Delivered | Matching source `answered` and `answer_operation_id` | Inbox bridge reconciles the original operation |

Exactly-once acceptance here covers the pilot's transactional result. It does
not establish exactly-once provider callback execution, stdout delivery, arbitrary
tools or external side effects. Automatic integration for other worker runtimes
is a later source contract; this pilot installs no global prompt hook.

## Manager-approved window checklist

Prepare exact source SHAs, installed profile/runtime hashes, native worker
instruction, private journal paths and rollback command files before requesting
the window. Maestro owner performs source deployment/profile/grant changes;
Inbox owner handles its existing scope and adapter. Do not start another bridge
journal for a scope already managed by the #601 adapter.

Prefer a separately isolated pilot project. The current Inbox config cannot add
a second Maestro scope without a source change or temporarily replacing the
existing scope. The operator must choose explicitly: a separate disposable Inbox
deployment with legitimate owner enrollment, or an approved pause/scope switch
and subsequent restoration of the existing pilot. Retain original configuration
bytes, credentials and journals. No automatic rebind of #601's journal and no
expanded project allowlist to bypass this decision.

#729's `restart.py` only orders ingress stop, Inbox restart, healthy check and
ingress start. Its pre-mutation guard checks the old environment, command,
mounts and IP; it does not compare file contents at the same configuration
path, so it neither detects nor authorizes a scope change. A scope switch and
its restoration are a separate Inbox-owner procedure: record before/after
configuration digests and the exact rollback, and never bypass the guard. A
CT119 restart inside that procedure still uses the established deploy
invocation and the #729 ordering.

Restoring the #601 pilot means its original configuration, credentials and
binding running against the *current* journals. Never copy an older DB,
mailbox, bridge journal or operation-store snapshot over newer consume/ack/
receipt records: that loses them and can re-deliver answered work. Backups are evidence and recovery input for a damaged
store, not a rollback mechanism. #729 rollback covers only nginx configuration
and container images.

1. Back up source images/configuration, source mailbox, bridge journal and native
   operation store consistently; verify integrity and original bindings. Record
   which operator owns each mutation and its exact rollback command. These
   backups are evidence, not restore targets for steps 7 and later.
2. Verify Inbox public HTTPS root and WebAuthn challenge with `publicKey`, plus
   source readiness. Owner uses the normal passkey flow; fixture/API replies are
   not a substitute for the required Inbox UI answer.
3. Admit one isolated native worker. Record PID, UID, netns inode, service lease,
   runtime/profile hashes and frozen source identities without credentials.
   Run the allowed/denied endpoint controls from inside that same worker boundary.
4. Publish the native question, confirm the matching Inbox card and choose Blue
   or Green in Inbox. Record the original operation ID and source acceptance.
5. Observe the original native worker read the answer, commit one result, ack,
   and produce its final result. Verify one stored consumption/effect and one
   original delivered operation in source and Inbox.
6. Exercise the restart and lost-ack cases below with fresh question IDs in the
   same admitted attempt. Retain each original operation throughout recovery.
7. Stop the isolated attempt and verify its cgroup/service termination. Revoke
   its grant using the approved source restart, then demonstrate old-token denial
   while the independent bridge/control scope still works. Restore any paused
   pilot configuration, credentials and binding over the current journals and
   verify public readiness and its exact binding.

Revocation does not delete operational receipts or erase an uncertain question.
Reconcile/withdraw unresolved records only through their original authority.
After revocation, worker reads/acks are expected to fail; do not recreate the
credential to make an uncertainty disappear. Preserve the evidence for operator
reconciliation. A new native attempt cannot inherit the revoked grant.

## Required acceptance matrix

| Probe | Required observation and positive control |
| --- | --- |
| Native path | Actual harness transcript, native namespace/lease, publish/read/consume/ack and final result; host-only consumer deliberately cannot satisfy this row |
| Allowed endpoint | Successful mailbox request from the native boundary; live forbidden alternate-port and management listeners remain unreachable |
| Scope | Wrong project/worker/thread/generation and unsupported reply/approval/launch actions fail; correct exact-scope request succeeds in the same session |
| Stop before consume | Kill the consumer after read, before transaction; source remains accepted/pending; restarted consumer commits exactly one result |
| Stop after consume | Kill after durable commit, before ack; same attempt/store resumes, result count stays one, ack advances original operation |
| Lost ack response | Test-only drop proxy forwards one ack then drops its reply; source audit proves it received the request; restarted consumer reads delivered and creates no second result |
| Repeated read/ack | Same operation/payload retains identical receipt/result; changed payload is refused; no replacement reply ID |
| New generation | Old identity/token cannot access the new attempt; explicitly issued new grant can access only its exact new tuple |
| Revocation | Actual old token returns authorization failure after removal; an independent current control grant still works, excluding listener outage |
| Rollback | Profiles/config/credentials/binding restored exactly with matching before/after digests, no wider fleet access, source and public Inbox readiness verified; current journals retained and no older store snapshot applied |

Fault injection belongs to the isolated test path. A positive drop-response
control records upstream acceptance without exposing tokens; it never adds a
general production proxy. If containment termination ends the scheduler attempt,
it is not a same-generation restart: test consumer-process restart within the
admitted service, and record whole-attempt termination separately.

Retain a sanitized evidence index with exact SHAs, bindings, actor/UTC times,
native transcript reference, operation IDs, source/Inbox statuses, transaction
counts, fault checkpoints, scope/revocation controls and backup/rollback receipts.
Keep raw credentials/configuration and private backups outside the repository.
Source fixtures and hosted CI can validate protocol code before the window, but
cannot close #728. Closure requires manager acceptance of the real native path,
all recovery/scope controls and scoped revocation.
