# Find in the current managed note

Ctrl+F on Linux or Cmd+F on macOS opens Find in the currently loaded managed
Source or Live Preview note. The toolbar also has a Find button. It searches the
current buffer, including unsaved text, with literal **case-sensitive** matching.
Enter/Shift+Enter or Next/Previous selects and highlights a match and scrolls to
it, wrapping at either end. The count starts at `0/N` until explicit navigation selects a match.
Empty and unmatched queries have explicit states. Replace is unavailable.

Live Preview temporarily displays **Source · Find**. Close or Escape returns to
the previous presentation using the same editor and Undo history. Find restores
the original directional selection and scroll position if the source is unchanged
and the user has not moved its caret independently. After editing, the current
caret is retained. Search results never edit the source or initiate Save.

Typing into the source clears stale results immediately and starts a new search
without moving the caret. Search is available in loaded readonly notes. Loading,
navigation, Save/recovery transitions, source reset and owner changes invalidate
the search session. Opening, closing, refocusing and match navigation refuse to
interfere with active source or query composition; finish IME composition first.

## Implementation boundary

The shell uses the existing pinned `gpui-base::input::SearchMatcher` in a background
worker, passing a cloned Rope snapshot and returning only plain byte ranges.
There is at most one worker per view and one latest pending request. Owner,
source stamp, query and generation must still match before accepting results.
The matcher and its Rc never cross the await boundary; its range vector is moved
out without copying the complete match set. Completion never changes selection.

Find accepts the existing managed source limit of **8 MiB** and queries up to
**2048 UTF-8 bytes**. Larger inputs receive an explicit refusal, without truncation
or a partial count. The worst case is one range per source byte, or 128 MiB of
range entries at the accepted source limit; a focused test exercises that case.

The editor's built-in search overlay remains disabled: its private close/focus
path cannot enforce the shell's composition and ownership checks. Searchable and
Replaceable remain false. The shell owns only the query panel and navigation;
the existing matcher provides literal matching, with no vendor/core changes,
new search engine, backend API or search persistence. Projection is suppressed
while Find is active, including late classification results. Current-match
selection uses the public source byte-range API, while one reusable
TextDecorationCollection keeps that match visible with the query focused.
It clears before source/query invalidation, close and owner changes.
Selection ends edit coalescing but
adds no source mutation or Undo entry. Highlighting every match is deferred.

Historical reader issue #48 is separate from managed editing issue #277. See the
[native projection contract](https://git.oklabs.uk/BeFeast/okilum/src/commit/43abc7c1040b76d3ab9608e2390bd0897c42f808/docs/archive/managed-live-preview-native.md) for the explicit
Source fallback requirement.
