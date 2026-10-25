# Reader opening must not verify filesystem links (#1700)

Mac Feedback #1704 on beta 17470 locates the pause in the background source
rewrite, not fonts or layout: two preparations take 905/927 ms, of which link
resolution takes 894/924 ms. The 194 recorded filesystem operations take
901/924 ms. The primary read is 7.28/0.37 ms; parsing is about 1.5 ms.

The copied note contains 32 distinct absolute Markdown-link destinations:
10 Markdown files, 21 JSON files and one text file. Repeated occurrences share
one resolution in `rewrite_source_links_collect`. On the old complete, live
snapshot path, each missing outside destination causes six filesystem calls:

| Destination | First outside-path check | Attachment pass | Final pass |
|---|---|---|---|
| `.md` | canonicalize vault root + destination | canonicalize root + destination, then reject `.md` as attachment | `resolve_markdown`: canonicalize root + destination again |
| Other extension | canonicalize vault root + destination | canonicalize root + destination | canonicalize root + metadata of the vault-root compatibility candidate |

Thus 32 × 6 = 192 link operations. Reading the selected note and checking its
macOS placeholder flag add two: **194**, matching both Feedback preparations.
The 10 `markdown_index_lookup` calls include their own 20 canonicalizations;
their 111/117 ms are not 10 slow hash-map lookups. The `/home/god/...` destinations
are foreign to macOS, where `/home` can trigger auto_home/autofs on metadata
access. Linux's ordinary local `/home` did not reproduce this cost. The trace
proves repeated path operations dominate; automountd attribution is the manager's
platform explanation, not a Linux profiler observation.

Rendering now uses an explicit `IndexedResolver` over the captured inventory.
It never calls the live resolver or path canonicalization. Markdown identity uses
the existing note maps and captured occupied/non-directory facts; attachment
suffix candidates are indexed once and shared by the image/link stages of a
source pass (`link_index_build` reports the cost), retaining ambiguity and
local-occupancy rules. Known lexical absolute paths inside the vault translate
to indexed identities. Known vault-root spellings retain that identity. Unknown
outside absolute paths produce an outside-file action without checking existence.
Observed aliases and parent traversal do not gain a guessed in-vault identity.

The existing separate `LinkPreparation::with_local_files` job still verifies
files and caches results per preparation generation, updating the emitted URL's
action/status. Click/hover handling retains the verified action. No external
file content is loaded merely to label a link. Actual selected-note/embedded-note
reads are distinct from link existence checks and remain necessary.

A portable regression renders 50 distinct native absolute paths, including one
real file and many absent files. The actual source-to-Reader pipeline must report
zero filesystem calls, 50 prepared identities and real parser/resolver activity.
Calling the live resolver on the existing file is a positive control proving the
filesystem counter detects work on the same OS. Additional cases preserve local,
root and relative Markdown identity, ambiguous notes/assets, occupied local paths,
and dangling symlink protection. It also preserves cached first-paint image
identities during provisional scans and verifies that a single-file sibling can
be discovered by the separate background verifier while its original rendered
URL remains actionable. Invalid CommonMark destinations with spaces do not gain
a link merely because an unverified outside path is possible; the pinned
conformance list is unchanged. Existing lazy-file tests retain existence checks.
