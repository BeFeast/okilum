# Date selections and Markdown reminders (#724)

Approved destination: a configurable vault-relative Markdown note, default
`Reminders.md`. Source notes are never modified. Tasks carry a source-note/heading
backlink and an ordinary Obsidian Tasks due date. Insert/create and receipt Undo
must use the existing revision-aware writer, refusing conflicts or active editors.
The full design and slices are recorded in issue #724 before implementation.

## Notification policy (manager decision)

- Default notification time: 09:00 local, configurable in Settings.
- A task for today created after that time does not notify immediately; it appears
  in Today only.
- Overdue/missed unchecked tasks generate one “N tasks overdue” summary. Clicking
  opens the list, rather than delivering a notification per task.
- Quiet hours: 21:00–09:00 local. Deferred notifications are combined at 09:00.
- Local OS desktop notifications only, silent by default. No server delivery.

## Parser slice

A pure core API receives the selected string and caller-supplied local date. It
accepts the whole selection, with surrounding whitespace ignored:

- ISO `YYYY-MM-DD`, with fixed widths.
- Day-first `D.M` or `D.M.YYYY` (one/two digit day and month).
- `D Month [YYYY]`: full English names, Russian nominative/genitive month names,
  or Hebrew Gregorian names (optional Hebrew ב prefix).
- English `Month D[, ] [YYYY]`, with an optional comma directly after the day.

Matching is case insensitive. No abbreviations, ordinal suffixes, slash-form
locale guessing, relative dates, Hebrew-calendar conversion, or arbitrary prose
extraction in this slice. Invalid shapes/dates return no action. Explicit years
are positive four-digit calendar years; an explicit past date is not rolled
forward. Without a year, today is eligible, otherwise use the nearest subsequent
valid occurrence, including leap days across non-leap centuries. At the supported
calendar boundary, return no action rather than overflow.

This PR supplies date parsing only. It introduces no menu, Settings UI, note
writes, scheduler, or notifications, and requires no new UI screenshots. Next
slices add Tasks formatting and guarded insertion/UI, then notification delivery
and the missed-reminders list with Linux light/dark evidence.

## Tasks-line formatting slice

The pure formatter receives visible sentence text, canonical vault-relative note
path, a caller-validated unique heading (optional), and the chosen due date. It
collapses sentence whitespace and escapes Markdown syntax so pasted visible text
cannot add links or a second checkbox. It emits an exact-path wiki backlink,
retaining the Markdown extension to avoid basename inference. Paths that cannot
be represented without changing wiki semantics refuse; an unrepresentable
heading falls back to the note link.

Before returning, run the generated line through the existing Tasks parser. It
must yield exactly one unchecked task, the selected due date, no scheduled/start/
done metadata, and default priority. Multiple due markers refuse, including ones
in the source path. The eventual editable preview can ask the user to adjust
text containing reserved Tasks metadata. This slice still performs no file IO,
notification scheduling, context-menu activation, or Undo.

## Lossless append-plan slice

An insertion plan keeps the complete before/after source. The existing prefix is
byte-identical, including BOM, CRLF and absence of a final newline; an appended
separator and task use the existing newline convention. Parsing the full result
must preserve all previously indexed tasks and expose exactly the new standalone
unchecked reminder at its expected line. An unclosed Markdown block that consumes
the insertion is refused rather than silently repaired.

Apply compares the whole current source with the captured preimage; Undo compares
with the saved postimage. Identical task text is never used to locate a deletion.
The plan itself does not write or constitute an Undo receipt. A later writer must
open FileEditor, recheck the source while holding its lock, save through its
existing conflict/recovery path, and only then expose Undo. Safe create uses the
separate exclusive destination path, not an assumed empty existing file.
