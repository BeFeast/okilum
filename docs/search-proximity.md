# Adjacent search terms and complete snippets

For ordinary multiword content queries, adjacent stemmed terms receive an
additional score independent of note length. The original conjunctive query is
still required; path restrictions and advanced query syntax keep their meaning.
Existing positional postings support the preference without an index rebuild.
The body phrase bonus is 12 and the title phrase bonus is 18, leaving BM25 as the
remaining ranking signal. This is a preference, not a guarantee that every exact
phrase beats every possible title/repetition score.

When the literal query words occur together, snippet selection prefers their
source paragraph over an earlier isolated common word. Tantivy still produces
the highlights and the existing Markdown snippet renderer preserves alias text.

The reported Finance note was present in the Mac index. A copied 7916-byte note
also appeared in native Linux. The first exploratory corpus contained the exact
phrase in all 40 short controls, which is not a proximity comparison. The
regression instead compares the public issue alias in an 8 KB note with 40
shorter documents containing the same words separated. Baseline puts a short
control first; the change places the adjacent alias first, highlights both words,
and preserves path-scoped exclusion. Fresh and reopened indexes are checked.
No ranking exception for a named note or folder is introduced.
