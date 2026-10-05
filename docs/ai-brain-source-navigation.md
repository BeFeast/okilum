# Returning from a managed note link

In **Source & preview**, follow a resolved note link in the rendered preview, or
choose a destination from its ambiguous-link list. Once the linked note opens,
**Back to previous note** returns to the previous saved note. Its tooltip identifies
the previous path.

There is one temporary return entry. Following A → B → C replaces A with B; Back
opens B and consumes that entry. It does not create a Forward entry. Clicking a
link to the current path, refreshing the same note, unresolved links and failed
reads do not advance it. Successfully opening an unrelated Source from another
product action clears it. Entries are not saved across app sessions or retained
for another workspace, endpoint, goal or editor.

Back re-reads the previous path. When its saved revision is unchanged, the source
editor restores the previous Source/Live Preview preference, exact directional
source selection and editor viewport. Scroll is clamped to the current layout; it
does not restore the outer details panel or the separate rendered preview. A
changed revision opens the current saved bytes in Source mode at the start with a
visible position-reset notice. Missing or renamed notes remain explicit failures;
there is no guessed rename target.

**Open link at caret** is available in both managed Source and Live Preview.
Place the caret inside a supported note link, or select text wholly inside one,
then use the button. A unique destination opens through the same Back flow;
ambiguous destinations show **Choose linked note** with title/path rows and
Cancel. The chooser stays visible in Live Preview. Moving the selection, changing
mode or leaving the source invalidates it. Returning to the current Source note
refreshes its owned preview so the explicit action works again without reloading
the note or resetting its selection, Live Preview mode or Context draft. Clicking
the already selected Source tab preserves the current proof.

The action requires clean saved source and a current preview/classification.
Finish Save, recovery, Find or text composition first and invoke it again; no old
caret-derived target is carried across a Save. Read-only notes are eligible.
Supported forms are plain wiki links and aliases (including cross-note ATX heading
links such as `[[notes/reference#Decision|Decision]]`),
and simple local Markdown links ending in `.md` (including `%20` spaces).
The existing conservative classifier limits this action to 64 KiB and accepted
paragraph/heading blocks. Embeds, reference links, nested labels and unsupported
containers retain the Source/rendered-preview fallback. Unsupported, unresolved
or stale results show a reason without navigation or queued retry.

For a heading link, the exact saved target must contain one supported top-level
ATX heading matching the Reader's case-insensitive, whitespace-collapsed heading
text. Formatting contributes its Text/Code content; punctuation is retained and
GitHub slugs are not substituted. Matching duplicate headings, including a matching
heading with unsupported inline syntax, refuse explicitly before navigation.
Missing, unsupported and over-64-KiB target headings also preserve the current
note and Back entry. The current target source revision owns the heading position;
subsequent selection, mode or owner changes cancel a late read/landing. Supported
Live Preview and existing Back retain their normal behavior. After the editor
paints the target and commits its ordinary caret reveal, one pending adjustment
positions the heading two line heights below the viewport top where existing EOF
clamping permits. A paint-origin deferred callback provides this phase boundary;
`on_next_frame` runs before draw and cannot establish completed editor layout.
Actual editor wheel, mouse-down or key-down input cancels the pending adjustment,
as do the existing source/selection/mode/owner fences. It never follows later user
movement, retries or adds blank padding. Same-note fragments, Markdown-link
fragments, block IDs and transclusion remain outside this slice.

The Live Preview editor still does not activate inline link clicks. The explicit
button uses exact source-byte ranges and joins the authored target to the current
existing backend preview result; it does not introduce another resolver.

Existing draft/recovery prompts remain authoritative. **Keep editing** cancels a
pending departure without consuming the entry; existing explicit Save/Discard
continuations preserve the return intent. Back itself does not save, discard,
modify Context or call a provider. While its private read is pending, the current
note and Find remain intact. Failed path/workspace/revision/UTF-8 validation happens
before resetting the source input. Changes of owner, source, Capture/collection or
other navigation make late replies inert.

Successful navigation clears the old Find and selected-Context preview through
their existing invalidation paths. It never restores an old Find query, decoration
or pending Add on another note. The same SourceInput entity and accepted lossless
multiline reset API are used; no vendor patch or new editor transaction layer is
introduced.

Implementation and checks are bounded to the managed Brain surface. The separate
v0 Reader is unchanged. There is no history stack, Forward, persistence, tabs,
keyboard shortcut, rename tracking or new backend API.

Current Context search hits also use validated passage navigation with a mutually
exclusive Context-origin entry in the same return slot. **Return to Context**
retains the existing form without another query or source read; a later successful
Source link supersedes that entry with normal Source Back. See
[Context search passages](context-search-passage.md) for ownership and refusal rules.
