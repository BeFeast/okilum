# Read-only media viewers

Video files (`mp4`, `webm`, `mov`, `m4v`, `mkv`, `avi`) and local Markdown
embeds show their filename, size and **Open in system player**. Selection and
rendering never launch an external application. The explicit button uses the
existing vault-checked file action and the OS default file association; codecs
and playback remain the system player's responsibility.

Public synthetic JPEG/BMP and virtual-log examples live in
`crates/okilum-core/tests/fixtures/viewers/`. The image examples cover baseline
and progressive JPEG, EXIF orientation 6 and 24-bit BMP. The `.okilog` sample
selects two relative log sources without a clock-dependent time window. Open
its definition and press **Start**; expect four interleaved records. These
fixtures also seed the QA vault showcase. They contain no private data.

Delivery and native evidence are recorded in issues #2687 and #2689. A passing
unit test or a merge does not by itself certify native beta behavior.
