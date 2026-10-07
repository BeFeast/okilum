# Revision evidence for displayed Tasks (#636)

Local prerequisite for native dashboard actions: retain full-source revisions in
the incremental Tasks index, and create an edit Target only from the immutable
index snapshot that produced the displayed occurrence plus matching source bytes.
Do not read files on the UI thread or capture a fresh revision for an old row.

Tests cover unchanged task labels after a prose edit, retained old index snapshots,
repeated task text, BOM/CRLF identity, removed sources and invented occurrences.
No native actions are activated; #699 remains a write-activation dependency.
