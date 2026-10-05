# Managed source integration (#220)

Managed `BrainView` chooses one `EditorState` at construction and starts in Source.
The Source/Live Preview button retains that entity, canonical Rope, focus, selection
and undo history. Legacy unbound source and readonly comparison panes keep their
Textarea implementation. Live Preview uses the same source surface and hides the
separate rendered preview, except where conflict comparison requires it.

`brain::source_input::SourceInput` is the narrow facade. Managed construction opts
into the accepted native projection hooks before any parse result. The shared
`CachedProvider` classifies original source; it never derives editable text from
Reader or the rendered preview. One background task and one latest pending request
are bound to entity, workspace, path, native source stamp and exact bytes. Results
compose against current native selection/IME. Above 64 KiB, classification falls
back to exact editable Source; neither the buffer nor recovery is truncated.

Managed `SourceMutation` replaces the former `InputEvent::Change` subscription.
Only the latest current stamp advances application input state, schedules preview
and protects the draft. Reset is an explicit domain transition; its queued events
and mutations from an older source stamp cannot protect an intermediate restore
base. Existing load/restore/adoption sites schedule presentation after completion.

Automatic Save captures the native source stamp when sending and requires it at
receipt alongside the existing input epoch, exact bytes, full recovery generation,
base/workspace and close binding. An edit and its reversal before queued callbacks
still invalidate adoption. Mode/reveal/layout changes do not. The acknowledged
child retains its true original base when newer visible source blocks adoption.
Exact retry, pending-child restore, explicit resolution and ordinary Save history
retain their existing authority and persistence formats.

The managed readonly state is set at authoritative freeze/release transitions using
`!source_editable || source_loading || busy || editor_closing()`. Local protection
alone does not prevent continued typing. Explicit resets still bypass readonly.

Linux startup resolves an explicit verified named Wayland socket and supplies the
accepted exact clipboard provider only to managed source. An inherited
`WAYLAND_SOCKET` is left untouched; because that connection's compositor cannot be
verified from the named endpoint, exact paste reports Unsupported. Missing provider
or capability never uses GPUI's normalized clipboard text. Cancellation and typed
provider errors appear beside the source controls. macOS uses a same-executable native pasteboard helper selected at startup. The
helper exits before GUI/config/backend initialization and reads only declared
`public.utf8-plain-text` bytes, with changeCount checked across the read. Its parent
runs on the background executor, limits output to 8 MiB, and kills/reaps on the
2-second deadline or cancellation. Invalid UTF-8, changed offers and oversize
payloads refuse without altering source; empty UTF-8 is distinct from no text.
Existing editor ownership, readonly and Undo fences still apply. Other platforms
compile with Unsupported capability; unrelated input fields keep their existing
clipboard path. Native adapter checks use a unique named pasteboard on the existing
M4 runner; they do not establish user GUI acceptance or modify the general clipboard.

Source tests cover actual Editor Edit/Preedit coalescing, reset filtering, queued
native source ABA, presentation during automatic receipt, immediate readonly
mutation rejection, missing-provider clipboard with an active-window positive
control, keyboard undo/redo across presentation, latest-bound classification, and
exact buffers at 64 KiB, 64 KiB+1 and 256 KiB. Existing recovery/merge/close tests use
actual managed construction; a separate legacy test asserts the unbound behavior.
These tests supplement, and do not replace, the integrated native acceptance matrix
in #220. Runtime acceptance and #221 isolated performance attribution remain
separate receipts.

## Opt-in native timing evidence

The combined candidate reuses #221's reviewed bounded recorder and actual component
spans. Default runs do not enable the recorder, bind the dump key, or produce trace
output. On Linux 64-bit, setting `TESSERA_MANAGED220_TRACE=1` enables tracing and an
F8 evidence action in the managed view. This action dumps the selected source's
entity/document/path/workspace, exact hash/size, selection, mode and queue state,
then the recorded scalar events and clock samples. Hashing, formatting, stdout and
buffer draining occur only during this explicit untimed action; they do not edit,
reset, refocus or notify source. `managed_trace_dump_complete` marks the end of the
output interval. The input controller must receive that marker before starting
the next sparse-input interval; its embedded timestamp precedes writing the final
marker line. Sparse-input timing must exclude the entire output interval.

The `source_mutation` event records the actual native stamp synchronously. An
`editor_paint` event identifies the actual prepaint layout and separately samples
the current identity at paint entry. A coherent paint requires matching document,
generation, presentation epoch and projected flag. Mutation-to-paint-end is an elapsed interval
that can include scheduling/frame gaps; only matched span durations measure their
instrumented work. A Wayland commit is associated only when the target surface and
generation-linked paint give a unique bounded association. It is submission, not
presentation or photon latency. Ambiguous/overlapping events and any recorder
overflow invalidate samples and must be counted as exclusions.

The disabled run proves no trace output or source/history effects. It has no
source-generation-linked paint endpoint, so it cannot establish quantitative
on/off overhead from arbitrary commits. Instrumented timing retains its tracing
overhead limitation until a valid matched endpoint exists. Standalone #221 timings
are not evidence for the managed application; each native receipt names the exact
combined source and binary hash.

## Large Source preview geometry (#224)

Native acceptance found that a 256 KiB single-line source stayed responsive with
the separate preview hidden, but stalled when the selectable markdown preview was
shown. The main-thread stack identified Inline paint repeatedly calling GPUI's
glyph-prefix position lookup for every character. Source loading and preview RPC
had already completed; the classification size limit did not bound this separate
preview geometry work.

Patch 0016 builds a transient position index from the current TextLayout once per
selectable Inline paint. Strict prefix maxima retain GPUI's first matching glyph
and inclusive wrap-end behavior, including duplicate and nonmonotonic glyph
indices. Masked hit bounds and active drag selection share that index; offscreen
selection anchors and the existing line-height rules remain intact. The index is
dropped after paint, with no persistent font or layout cache. Source limits,
editing, preview selection, and GPUI core are unchanged. Small differential
geometry tests and a comparison-count growth check supplement the native retake.
