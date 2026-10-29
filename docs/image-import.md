# Image paste and import

Approved scope: #1743, 29 October 2026. Applies to images pasted into notes,
file paste/drop, and local, data and downloaded images in pasted rich text.

Settings → Editor holds one app-wide policy for every vault:

- Convert images to WebP: on by default.
- Image quality: 85 by default, bounded to 1–100.
- Keep original images: off by default. When enabled, a separate
  `name-original.ext` attachment preserves the incoming bytes.

The WebP replaces the incoming attachment copy. The external source file and
files already linked inside the vault are left untouched. Existing attachments
are never rewritten. Names are collision-safe, and the note links to the actual
published file. An optional original must be saved successfully before the link
is inserted. The encoder receives the configured lossy quality and preserves
alpha; EXIF orientation is applied before encoding.

Already-WebP files, GIF/APNG animation, vectors, multi-page TIFF and formats
without a supported decoder retain their original format and bytes. This avoids
silently flattening or discarding content. JPEG XL is a separate follow-up.
Invalid supported images and failed writes produce no embed. Encoded input is
limited to 64 MB and decoding has dimension and allocation limits.

Encoding and disk publication happen off the UI thread. The policy is captured
when paste starts, so a Settings change cannot alter an in-flight operation.
Note Undo removes the embed, not its attachment; Redo can restore the same link
without losing the image or its optional original.

Before merge, native acceptance must cover both paste and import into an
isolated real vault, matching the disk file with the saved note link, and Undo
and Redo without losing image bytes. Unit/GPUI tests supplement that gate.
