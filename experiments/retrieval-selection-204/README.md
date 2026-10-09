# Verified experience selection experiment (#204)

> **Historical document.** Written before Tessera was renamed Okilum (2026-10-09). Names, paths and links are kept as they were then.

Repository-only diagnostic experiment inspired by the pinned OpenExecutive research
`3c379362809016ed117e597a74f083914e83fd3e`. No product/runtime feature is enabled.
It asks whether an explicitly scoped historical incident can improve a next-step
answer over the existing reviewed context path. This synthetic, finite-choice task
measures evidence selection and application, not open-ended agent reliability.

## Frozen corpus and independent labels

`corpus.json` has 24 synthetic goals: eight situations, each with matching, similar
but nonmatching, and retired-source variants. Each goal owns its incident and a
manual pin. `labels.json` defines the eight applicable cases and sixteen controls,
expected executable actions, required/forbidden constraints, and expected selection.
The selector cannot read labels. Frozen file hashes and source commit must be
recorded before any provider outputs. Evaluation labels never change afterwards.
The `c01` baseline manual pin already contains the applicable incident: this is the
positive control against an artificially empty baseline. The other baselines contain
an ordinary local runbook and goal context but omit historical exception evidence;
this is a deliberately narrow incremental-retrieval test, not a comparison to an
unlimited manual search or a different reasoning system.

The three variants are intentionally related, not 24 independent domains. The
specific local contracts are fabricated; findings must never enter ordinary brain
knowledge. Corpus action catalogs necessarily hint at possible exceptions. The
model may infer an answer without retrieval; measured baseline performance retains
that possibility and is not rewritten to manufacture an improvement.

## Selector and immutable context

Use actual backend `brain_search` with lexical mode, goal scope, limit 20, maximum
excerpt 2048 bytes, and topic query. Filter to `incident` sources with verified
metadata, matching goal owner, and a current source revision. Parse the explicit
incident JSON in the excerpt; require all `applies_when` key/value facts to match,
require verification `verified`, and reject when the `obsolete_when` predicate
matches the incident state. Missing facts, malformed/truncated JSON and unavailable
source revisions mean exclusion, never a guessed match. Stable order is existing
search rank followed by citation ID, deduplicated by source path. Keep at most two
complete excerpts. Never trim or rewrite excerpts.

Prepare and explicitly review one existing context packet per goal with the
manual citation pinned. Both arms supply exactly its reviewed text and citations,
plus the actual `goal_context_brief`. Candidate incidents are a separate, visible
supplement with source path/revision, citation ID and incident ID, outside the
immutable reviewed packet. Baseline supplemental selection is explicitly empty.
Preserve manual pins byte-for-byte; remove lowest-ranked whole supplemental excerpts
if the shared context byte cap would be exceeded. If mandatory base context alone
exceeds the cap, mark the case incomplete without a provider request. Check packet
and pinned source hashes before/after each arm. Chat appends its own conversations;
those authorized records are not considered packet mutations.

Before evaluation, prove actual API rejection of a stale citation and a citation
owned by another goal using `context_prepare`, with unchanged existing packet and
journal hashes. Include a successful actual prepare/review of a valid manual pin
as the positive control. Retired controls edit the incident through `source_write`
after its original citation was retrieved; stale original citation rejection and
current-but-retired filtering are different recorded checks.

## Provider contract and resource allocation

Use new actual `chat_start`/`chat_get` conversations for both arms, no history and
no external engines. Read only the saved Chat profile/reference selected by the
operator. A loopback recording relay forwards to exactly that upstream/model;
it records the final actual provider request and raw SSE response, omitting auth
headers. It applies identical `temperature: 0`, `max_tokens: 1024` and
`stream_options.include_usage: true` in both arms. These are experiment-only
transport controls; ordinary runtime source and saved settings are untouched.

Each arm receives the same maximum context allocation: 12,000 UTF-8 bytes for the
experiment message, 8,192 actual provider input tokens, and 1,024 output tokens per
case; aggregate ceilings are 196,608 input + 24,576 output tokens per arm. Equal
budget means equal allocated ceilings, not padded/equal observed token consumption.
The relay checks the actual request messages are at most 24,000 UTF-8 bytes before
forwarding. Record actual input/output/total tokens returned by upstream; do not
estimate them. Missing usage, unexpected model/transport, exceeded ceilings, invalid
output or incomplete stream produces incomplete evidence, never an invented pass.
No provider fallback, automatic retry, post-output relabeling or extra scoring calls.
Exactly 48 authorized calls in the full run; alternate baseline-first/candidate-first
by case number, same host/session, serialized calls. Offline transport validation
uses a controlled fixture and is explicitly not provider evidence.

## Frozen deterministic scoring

The downstream response must be a JSON object with exactly `goal_id`, `action_id`,
`constraint_ids`, and `evidence_ids`. The answer chooses one action from the public
catalog and lists only needed constraints and supporting evidence IDs. Unknown IDs,
wrong association, duplicate IDs, malformed/missing fields, or clarification instead
of an executable action are invalid/ambiguous and cannot count as improvement.
No prose outside JSON and no model self-score are used as evidence.

The scorer records per-arm `allowed_supporting_ids`: applicable candidate incident IDs,
plus `manual-pin` only for c01; controls use `manual-pin`. For the seven other
applicable baselines, guessing the special action while citing the ordinary
manual pin is not evidence-supported success.

An applicable case succeeds only with the labeled executable action, exactly the
required constraint, and at least one current supporting incident/manual evidence
ID. A control succeeds only with the labeled ordinary action and no constraints;
only `manual-pin` evidence is available there. A control's special action or any
constraint is a false constraint. An applicable improvement is unsuccessful baseline
and successful candidate; regression is successful baseline and unsuccessful
candidate. Baseline `c01` must succeed or the evaluation cannot advance. Ambiguous
outputs are reported; separate blinded human/agent review may explain them but may
not turn them into improvement or alter the deterministic result.

Advance only with 0 wrong goal associations, 0 stale excerpts presented as current,
0 hidden reviewed-packet mutations, at least 3/8 applicable improvements, no
applicable regressions, and no new false constraints across the sixteen controls.
Offline invariants and provider completion/usage gates must also pass. A failed or
ambiguous gate keeps the feature disabled. A passing result supports a separately
reviewed implementation issue only, not automatic rollout.

## Reproduction and evidence files

Run the deterministic scoring/gate tests:

```sh
python3 -m unittest discover -s experiments/retrieval-selection-204 -p test_run.py -v
```

Run controlled transport against the existing backend binary in a new isolated root:

```sh
python3 experiments/retrieval-selection-204/run.py --binary /path/to/tessera-cored --output /path/to/new-offline-run
```

The output contains `freeze.json` with the source commit, source file hashes and
backend hash, actual RPC receipts, prepared packets, rejection checks, per-call
requests/responses, results, summary and cleanup receipts. Offline usage is visibly
synthetic fixture data and cannot establish provider performance. Preserve failed
runs and never overwrite an output directory. The independent reviewer creates a
manifest with the same freeze fields plus `independent_review: "PASS"` before the
single measured run. Provider mode requires that manifest:

```sh
python3 experiments/retrieval-selection-204/run.py --binary /path/to/tessera-cored --output /path/to/new-provider-run --saved-settings /path/to/connector-settings.json --frozen-manifest /path/to/reviewed-manifest.json
```

Record paths refer only to the isolated run. Auth values are never in request
recordings; the runner reads only `config.chat` from the saved profile and copies
its credential reference to isolated backend settings. It never edits that profile.
Final evidence requires an upstream `stop` finish reason, consistent nonnegative
integer usage totals, per-call and per-arm ceilings, and matching provider model.
Each arm checks the frozen goal, incident source, reviewed packet and manual pin
against their pre-provider hashes; selected citation revisions are revalidated.
