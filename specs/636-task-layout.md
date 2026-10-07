# Declarative Tasks layout (#636)

Parse the namespaced tasks_view mapping after Tasks view selection. Combine the
existing section/query parser with validated density, grouping and a permutation
of section order. App-owned defaults apply only to omitted values. Fail back to
Markdown for invalid shapes, unknown settings, ambiguous heading references and
unsupported queries. Preserve every original section and its source line.

Test duplicate headings, one-based occurrence selection, partial ordering,
unknown/malformed fields, Unicode headings, CRLF source positions and query
fallback. This core slice changes no UI and enables no writes; #699 remains a
native-write activation dependency. Owner vault remains untouched.
