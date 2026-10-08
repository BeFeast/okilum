# Canonical source for typed views (#636)

Keep the exact primary-file Markdown snapshot (frontmatter, BOM and line endings)
with its prepared Reader document. Derive it from the same read as rendered input;
never reopen the note on the UI thread. HTML and an empty selection carry no
canonical Markdown capability. Closing a note clears the snapshot.

Reconciliation compares canonical bytes as well as rendered input: frontmatter-only
changes must publish fresh properties and view-selection input, even when the body
is unchanged. A failed or stale preparation keeps the prior accepted snapshot.
No dashboard controls or writes are activated in this slice; FileEditor is unchanged.

Test exact source through rendering rewrites/BOM/CRLF and native frontmatter-only
reconciliation. Existing navigation generations remain the stale-completion guard. Inventory
reconciliation uses a separate generation so a source refresh cannot cancel an
in-flight user navigation; reverting to accepted bytes also cancels older work.

Shared-window integration: when the shared worker has detached its mutable source
baseline, use the existing background document-read path and retain canonical
bytes from that same read. Inventory-driven updates still render their accepted
source snapshot directly. Both routes share the reconciliation generation and
publication guards; neither reads the file on the UI thread.
