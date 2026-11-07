# Project views (#1892)

Oleg selected variant A on 2026-11-06: **Board, Decisions, Reports, Handovers**
are tabs in the project center. The existing Manager stays on the right. Files
restores the ordinary vault browser; Open in Notes uses the existing guarded note
navigation. Returning to Project views retains the Board entity, filters, cards
and Manager draft. Switching these tabs never sends a message.

These views read existing Markdown, without enrollment, conversion or writes:

| View | Project-relative directories |
| --- | --- |
| Decisions | `ops/decisions`, `decisions` |
| Reports | `ops/daily`, `ops/reports`, `reports` |
| Handovers | `handovers`, `ops/handovers` |

One file is one record. Its authored frontmatter `title`, or first H1, is the
list title; the filename is only a fallback. An authored `date`, or valid ISO
date prefix in the filename, supplies its date. Only authored `status` appears
as a status. The view does not infer resolution from prose, merge or delivery.
Identical titles remain separate records with visible source paths. There is no
automatic conflict resolution. Search covers title, source path and body.

The detail shows selectable read-only Markdown, source and date. Reports explicitly
describe a saved snapshot, not the installed build. Handovers carry a historical
context notice. **Обсудить в Manager** opens Chat in the existing dock and appends
the record title, project-relative path and SHA-256 of the exact read source to
its editable draft. Existing draft text and ordinary Undo survive. This neither
edits the file nor prepares/sends a message. An unpaired composer can prepare
context offline; an established native conversation requires its existing exact
visible-owner handoff. An unavailable/changed target reports refusal instead of
redirecting context to another conversation. Connecting later uses the existing
Manager draft restoration rules.

Discovery runs off the UI thread only while a record tab is visible. The existing
vault publication/write batches and explicit Refresh invalidate its single-flight,
generation-checked read; hiding the view revokes in-flight publications. No extra
watcher or timer is added. Board's existing polling pauses while records are shown.
Reopening a record tab re-reads sources. Each source tree is bounded to four nested
levels, discovery to 512 entries, a file to 256 KiB and a refresh to 4 MiB. Symlinks
are not followed, including directory ancestors. Invalid frontmatter, access
errors and limits appear as source problems beside healthy records. On an
incomplete refresh previous missing records are retained with an explicit stale
snapshot notice; a successful read confirms deletion. An absent source directory
is an empty view with its expected paths, not an automatic folder creation.

Native verification must exercise actual tabs, source opening, draft preservation,
offline handoff and the three-platform beta artifacts. Hosted GPUI tests alone
do not certify installed-app delivery. Release builds, video and tester evidence
are recorded in [#1892](https://git.oklabs.uk/BeFeast/okilum/issues/1892).
