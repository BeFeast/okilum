# Current-root presentation publication (#1640)

This core-only layer publishes immutable in-cap presentation from the **same
canonical Classification** as an accepted BlockIndex. It is Reader-unwired and
provides no native-frame proof, large-note admission, fragment replay or global
piecewise mapping.

A publication contains its accepted root binding (exact document, generation,
bytes, closed options context and root request), validated canonical active ranges,
the existing RetainedPresentation/RevealSnapshot/Projection and an owned opaque
publication identity. That identity identifies an immutable view; it is never
used as an options/callback semantic token. Mapping/reveal queries check root,
context, active ranges and publication identity before delegating to the existing
validated projection. There are no cached editor rows or hitmaps.

Only the structural session's current **accepted** request can begin publication
work. A root retry, including identical bytes/generation, remains pending until
its own complete authority adopts. The publication session separately tracks its
owner and each input attempt. Partial, superseded, foreign-owner and stale-root
completions cannot become current. Even refused active/context input supersedes
older pending work. Old held publications remain readable only with their own
exact snapshot, active and identity; they are not current-complete evidence.

For the identical accepted root and active ranges, a completed new request can
reuse the same immutable publication. A valid active change prepares a new reveal
and identity while retaining the same-root validated base. A newly accepted root
always derives a fresh base, including a root retry with identical source bytes.
No canonical/fragment parser or reference lookup runs in publication work or
adoption. Canonical recompute remains the root-authority path; the full-parent
fragment-cache key stays unchanged.

Input units are explicit (source bytes or UTF16). Conversion and validation reject
surrogate, UTF8/grapheme and CRLF interiors and reversed/out-of-range selections
or compositions. Existing RetainedPresentation/Projection validation, source,
range and work limits remain authoritative. Parser prefix-stripped definition
and lookup offsets are never used as editor ranges.

Tests compare against fresh monolithic canonical retained/reveal projection on
every source/display byte boundary, including both mapping biases, Unicode/CRLF,
active changes and raw-scope decisions. Test-only counters at actual retained-base
construction, reveal preparation, projection indexing and canonical parse sites
distinguish cold work, actual repeated work and immutable reuse. Failed adoption
does not parse, prepare or charge lookup fuel. Unknown/stateful options retain the
earlier fresh observation-only path; they cannot produce an accepted root here.

Remaining backend step: integrate a bounded current-root publication consumer
against the real backend/source/config, with actual publication/presentation epoch
ownership and complete intended mapping. Global joins/viewport and Reader/native
PresentationProof need separate scope. The current root still admits at most64KiB;
this foundation is not a130KB backend or a measurement candidate. Ordinary64KiB,
native256KiB,4096-range and2MiB-work caps remain unchanged. No performance acceptance
or cross-edit work benefit is claimed by same-root publication reuse.
