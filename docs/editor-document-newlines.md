# Document newline insertion (#848)

Enter and clipboard paste in the shared editor use the first LF-terminated
line's delimiter: CRLF when preceded by CR, otherwise LF. Documents without an
LF default to LF. Mixed files retain their existing bytes; new input follows
the first delimiter. A standalone CR is preserved as data.

Only the inserted text is normalized. Loading, revision-aware replacements,
IME composition and Undo/Redo replay are unchanged. Both ordinary clipboard
paste and the revision/focus-validated exact clipboard path apply this policy;
the latter reports the number of bytes actually inserted after normalization.
Detection reads the rope up to its first LF without flattening the document.

The Linux baseline native probe appended LF to a CRLF document on Enter while
preserving the original prefix. The pure regression corpus covers mixed input,
Unicode, lone CR, borrowed fast paths, and exact surrounding bytes. A rendered
editor regression exercises Enter, clipboard paste and Undo in hosted CI.
Native Reader/save acceptance remains required before claiming the full issue
verified; pure tests are not evidence for the clipboard bridge or disk save.
