# Guarded Windows signing helper

`okilum-sign-code.sh` is the source of truth for the standalone deployed
`okilum-sign-code` helper. Change this tracked file; installing a copy is a
separately authorized live operation. The guard is embedded so copying this
one file preserves it. A merge does not update the installed copy.

Before reading OTP or contacting the signer, the helper inventories nonterminal
`windows-diagnostic.yml` runs. `OKILUM_SIGN_CANCEL_ALLOWLIST` defaults to `[]`:
no cancellation is authorized, and any active run stops the operation.

An explicit grant can supply a JSON array (maximum 10 entries) with every field
below. These are placeholders, not authorized targets:

```json
[
  {
    "id": 123,
    "index_in_repo": 99,
    "workflow_id": "windows-diagnostic.yml",
    "ref": "refs/heads/main",
    "commit_sha": "0000000000000000000000000000000000000000",
    "owner": "BeFeast",
    "repo": "okilum"
  }
]
```

Only scheduled, non-fork runs from this repository and main are eligible. The
source SHA identifies the run being cancelled; it need not equal the payload
being signed. All approved runs must appear in the initial inventory. Every
cancellation gets a fresh complete inventory followed immediately by a detail
read and an exact identity check before POST. Unknown/new/foreign runs, identity
mismatches, missing fields, incomplete responses, API failures and timeouts stop
the operation. There are no retries. Inventory is bounded to 50 active runs;
more than one page stops. The entire guard has a 60-second deadline and each
request a maximum of 20 seconds. Cancellation still pending at the final
inventory stops before OTP; no signing or dispatch follows that failure.

Forgejo cannot atomically compare inventory and cancel: a run may appear after
the last read. The guard never cancels such a run; the final inventory stops
login when it observes one. This is not a concurrency reservation.

A signing grant must separately cover signer login/dry-sign, repackaging and
protected publication. Publication may update the moving beta Setup/feed when
the dispatched run's commit equals the selected source; otherwise it uses
archive-only publication. Stable promotion is separate. A cancellation allowlist
alone grants none of these effects.

Offline regression tests (also discovered by the Linux CI gate):

```bash
python3 -m unittest discover -s scripts/windows -p test_sign_code_guard.py -v
```
