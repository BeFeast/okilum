# Canonical comrak reference evidence (#1640)

This is parser API groundwork. Reader remains unwired, the fragment cache keeps
its full-parent-byte key, and this change provides no large-note admission,
fragment replay, cross-edit reuse or performance acceptance.

`scripts/comrak-setup.py` reconstructs the entire upstream **comrak 0.47.0**
archive and applies `scripts/comrak/reference-audit.diff` in the generated
`vendor/comrak-0.47.0` directory. The manifest pins archive checksum, patch checksum
and the complete resulting source tree. `--verify` refuses missing/changed source;
`--archive PATH` uses a local exact archive without network. Setup reads the Cargo
archive cache but never modifies registry sources or global configuration.
`scripts/vendor-setup.sh` runs this independent setup before the existing GPUI
verification. An existing mismatched generated parser is refused, not repaired.

The workspace `[patch.crates-io]` deliberately replaces the comrak source, keeping
version, manifest and transitive requirements unchanged. The corresponding
intentional lock change removes only its old registry source/checksum. Temporary
test lock resolution must restore this new committed graph, not the old source.
The original checksum is retained in the manifest and must match Cargo.lock's
previous registry checksum.

`parse_document_with_reference_audit` runs the same canonical parser as
`parse_document`, with read-only evidence enabled for that parse. Its immutable
report owns the exact root source bytes and contains:

- Ordered accepted definition events, including unused definitions and duplicates;
  canonical normalized labels, cleaned URL/title and the actual first-insertion
  disposition. Consumers retain the first inserted event for each normalized
  label; exporting a hash map is not the API.
- Paragraph source positions and definition offsets/raw bytes within the parser's
  prefix-stripped content. These are explicit parser context, **not validated
  editor ranges**. A later structural adapter must prove any root-range conversion.
- Ordered reference attempts: missing/exhausted/resolved/not-attempted canonical
  lookup, original/normalized labels, callback outcome and before/after/max fuel.
  Inline offsets/line are parser context, not authority for a source mutation.
- Final fuel from the same root state. Audit getters do not perform a lookup.

Definition and lookup sequence numbers share one canonical event order. Data is
sampled at existing parser operations. The lookup formula, limits, charging,
callback precedence and default parse behavior remain canonical. An exhausted
lookup may still resolve through the existing BrokenLinkCallback; that fallback
does not charge canonical lookup fuel. No fragment receives a new/reset budget.

The report is **not** a reusable semantic-context key: options, external callback
identity/state, document identity and generation are not certified by root bytes.
There is no replay or adoption API. Until a later adapter proves complete identity,
budget/outcome equivalence and current-root rebind, recompute conservatively.
Actual cold/hit controls remain in the previously tested fragment cache; this
parser-only slice does not claim cache-hit or replay work savings.

Targeted CT141 tests compare XML AST (including positions), HTML and CommonMark
outputs of the default patched parser with an exact upstream test-only receipt,
then compare default and audited paths on the same corpus. Further API regressions
cover unused-to-used/forward/duplicate definitions, NBSP/Unicode normalization,
clean URL/title, frontmatter/fences/containers, cumulative fuel below/at/above the
canonical limit across paragraphs, and exhausted/missing callback fallback with
one invocation and no extra charge. Readback cannot consume fuel. This does not
claim a complete production classifier corpus proof or fragment replay proof.

BlockIndex/ReferenceAuthority integration, structural 20% invalidation, stale
async results, composed joins, viewport behavior, Reader/PresentationProof and
size admission remain later slices. The 64 KiB Reader wall, 256 KiB native guard,
4096-range and 2 MiB-work bounds stay unchanged. No release/diagnostic candidate,
hosted dispatch or native pass is part of this API/test stage.
