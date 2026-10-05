# Open a passage from Context search

Current Context search-hit cards offer **Open source passage**. The action validates
the saved note revision and cited line range, then opens the first cited line in
Source/Live Preview. The normal presentation preference is retained; a first Source
visit initializes Source mode. An already loaded target keeps its editor and Undo.
The existing after-paint positioning places the cited line in the upper reading
area where normal EOF clamping permits. It does not select the entire excerpt.

**Return to Context** changes the surface back to the retained form. Query, scope,
mode, search results, selected/excluded excerpts, pins, saved packet and unsaved
guidance stay in the same GoalContext and input entities. Return neither reconstructs
a form snapshot nor issues search, Source read, save or provider requests.

The existing single return slot holds either a Source origin or a Context origin.
A successful Context passage replaces a previous Source Back. A rejected passage
leaves it intact. A subsequent successful Source link replaces Context return with
ordinary Source Back; no hidden Context history resurfaces. Return is consumed once
and does not restore a previous source note. Goal/workspace/collection departure or
replacement of the retained form invalidates the transient Context return.

Each search response retains its client-owned original request. A hit must still
belong to that response and match the current query, folder, retrieval mode and
scope mode. Editing query/scope/mode invalidates old hit provenance until another
matching search completes. Opening captures the retained form entity identity and
a comparison fingerprint; changed owner, form, result or navigation intent makes a
late source response inert. The fingerprint is never used as restorable form state.

Busy Context operations, adoption/export/recovery, Source dirty/write/conflict/Find,
active link editing and text composition refuse explicitly. Unsaved Context guidance
is allowed. The existing workspace-guarded source_read validates path, brain,
original UTF-8 bytes and recomputed revision before any source reset, surface change
or return-slot replacement. LF/CRLF, BOM and multibyte text preserve original byte
offsets. Missing/stale notes or invalid line bounds request an explicit new search.

This behavior is limited to current search-hit cards, including hits already
selected. Selected-context, saved-packet, brief and other provenance cards retain
Open original. No backend, cache schema, provider, vendor or Reader changes are
required. See [managed Source navigation](ai-brain-source-navigation.md) and
[incoming references](managed-incoming-references.md) for the reused landing and
known-source passage contracts.
