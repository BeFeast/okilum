# Embed file location and actions (#1622)

Local file embeds expose their captured vault-relative identity in Reader,
Source and Live Preview. Images retain the existing renderer; PDF, ZIP and
other attachments use a file row. Multiple files in one paragraph retain
individual action surfaces. Note transclusion and drawings keep their own
renderers.

Source hover uses the native editor hover lifecycle. It parses the captured
source and reads metadata on a worker, with the same bounded, revision-checked
image header reader as rendered hover. The hint includes the vault path, byte
size and image dimensions when available. A changed note or draft rejects a
pending result. Code examples and oversized source documents remain excluded.
Hover never fetches remote images or materializes iCloud placeholders. Native
editor hover is installed only in Source, since LSP overlays disable editor
projection; Live uses the rendered embed tooltips.

Live Preview adds a small muted vault-path caption under each rendered local
image. Missing or ambiguous embeds show a visible message with the authored
target. They retain a context menu with unavailable file operations disabled;
copying the target remains possible without inventing a resolved file.

Every file surface and Source embed menu shares the existing image action
implementation: platform Reveal, Open in default app, Copy path (vault-relative)
and Copy absolute path. Image-specific clipboard/save actions remain available
only for images. Cmd-click on macOS and Ctrl-click on Linux/Windows open the
captured file. Ordinary Live Preview presses retain source-entry behavior.

Reveal uses the existing platform service (native Finder/Explorer, Linux
FileManager1 with containing-folder fallback). Open and Reveal recheck canonical
vault containment on the worker. These interactions do not write source files
or widen the reader protocol. Existing explicitly requested image import, Save
image as and iCloud actions keep their prior authority.
