# Automatic disjoint Save evidence (#206)

> **Historical document.** Written before Tessera was renamed Okilum (2026-10-09). Names, paths and links are kept as they were then.

The implementation reuses the existing bounded exact-line merge helper and source
CAS/replay API. [The contract](../../docs/archive/ai-brain-disjoint-save.md) defines the
fresh Save boundary and late-input behavior.

Focused automated coverage lives in `brain/editor_recovery/auto_resolution.rs`
and `brain/editor_ui/auto_save.rs`. It covers one retained child, original/child
lost ACK, failed local publication, second conflict, generation races, merged
receipt adoption, typing before and after adoption, and close coordination.
The `A/B → X/Y` late-input regression preserves visible `X+/B` with its true old
base and blocks the next ordinary Save. Its positive control confirms that the
same Save action queues work after the conflict guard is cleared.

## Exact predecessor compatibility

Run from the repository root with a new output directory:

```sh
python3 experiments/source-disjoint-206/compatibility.py /tmp/tessera-206-compat-evidence
```

The harness obtains the predecessor recovery helper directly from commit
`7ed3d9d`, copies the current helper and existing merge policy, and compiles them in
an isolated Cargo project. It creates a real old v1 draft and a real new v2 child
with newer unreconciled `X+/B` text. It verifies:

- New reader accepts both without rewriting v1.
- Exact old reader reports v2 as an identity problem and omits it from recoverable
  drafts. Forced old retention, discard and acknowledgement also refuse it.
- Both record files remain byte-for-byte unchanged, and the new reader still
  recovers the v2 record. The harness contains no source RPC module or transport.

`source-hashes.json`, `build.log`, `result.json`, the exact helper copies, Cargo.lock
and fixture records remain under the selected output directory. No live brain,
provider or desktop configuration is touched. Native acceptance and actual
backend CAS/replay checks are separate evidence; this harness does not claim them.
