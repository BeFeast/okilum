# Inline PDF viewer (#477, slice 1)

Selecting a PDF in the reader shows its pages inline instead of the file card.
Engine choice and the slice plan are in
[research/477-inline-pdf.md](research/477-inline-pdf.md); this note records the
decisions slice 1 made.

## Shape

- `crates/tessera-shell/src/pdf_engine.rs` is the only module that knows hayro.
  It opens a document, reports page sizes and renders one page to a BGRA bitmap.
  A panic inside the engine becomes "unreadable" or a failed page, never a crash.
- `crates/tessera-shell/src/reader_pdf.rs` is the GPUI viewer:
  - every page slot is sized from the page dimensions as soon as the document
    opens, so the scrollbar is right before any bitmap exists;
  - one worker thread per open document renders the visible pages first, then
    one page of margin on each side. Its queue is replaced on every frame that
    changes what is wanted, so a fast scroll never builds a backlog;
  - bitmaps live in a least-recently-used cache of 128 MB (CPU bytes; the GPU
    atlas mirrors them). Pages the viewport needs are never evicted. Every
    evicted bitmap is removed from the GPU atlas with `Window::drop_image`, and
    leaving the PDF drops the worker and all atlas entries;
  - the file is read into memory, not memory-mapped: a file truncated under a
    map would crash the process. Files over 512 MB are not opened.
  - when the vault watcher reports changes, the viewer compares the file's size
    and modification time and reopens it if it changed.

## Reading

- Fit to width: the widest page fills the pane minus 24 px a side; all pages
  share one scale so mixed page sizes keep their proportions.
- Zoom steps from 50% to 300% of fit width: ⌘/Ctrl +, ⌘/Ctrl −, ⌘/Ctrl 0, and
  the same as glyph buttons in the document header. Bitmaps are capped at
  4096 device pixels a side; beyond that a page is scaled up, not re-rendered
  (tiles are slice 5).
- The page and zoom of each PDF are kept for the session, so returning to a
  document lands where reading stopped.
- Password-protected and unreadable files show an inline message; Quick Look
  (macOS) and **Open with default app** stay in the header throughout.

Not in this slice: find, text selection, links, outline, embeds.

## Not verified here

The tests run headless on Linux. Native GUI behaviour, frame pacing during a
fast scroll and the acceptance timings belong to the owner's Mac and the 24
linked PDFs (research doc, "Acceptance gates for slice 1").
