# Note aliases (#1562)

`aliases` is reserved frontmatter metadata. It names the declaring note and is
not an outgoing relationship. Properties show its values as plain text chips;
legacy `"[[Alias]]"` values display as `Alias`. String scalars and lists of strings
are supported, including Unicode; blank, malformed or non-string declarations do
not add resolver identities. Commas within a string are literal.

Wiki targets match declared aliases throughout the vault, case-insensitively.
The note's canonical identity remains its root-relative path. Exact canonical
paths retain precedence, and explicit `./` and `../` targets remain path-only.
Otherwise alias and path-suffix candidates participate together: multiple distinct
notes produce the existing ambiguity UI, never an arbitrary winner. Alias names
are matched as whole names, not as path suffixes. Heading/block suffixes keep their
existing navigation checks. Ordinary Markdown file destinations retain path rules.

Quick Open matches aliases alongside titles and filenames, with one result per
canonical path. An ambiguous alias lists every matching note. No file reads occur
in alias lookup, property rendering or Quick Open matching.

The alias index and backlinks are derived from canonical source snapshots. Warm
startup retains aliases; changing or removing declarations re-resolves affected
cached referrers and invalidates link preparation, including incremental/delta
restart. The derived cache schema advances to rebuild older relationship graphs.
Aliases in frontmatter are excluded from rename/move link rewrite destinations.
