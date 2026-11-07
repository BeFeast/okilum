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

MP3 and WAV files and local embeds use the same streaming rodio 0.21.1 /
Symphonia 0.5.5 decoder and CPAL output (ALSA/CoreAudio/WASAPI). Only MP3 and
PCM/WAV features are enabled. Play/Pause, seek, volume and replay share the
same sink implementation. The output device opens on Play, never on selection;
closing the view disconnects its command channel and releases output. The Linux
AppImage explicitly bundles libasound.so.2 (excluded by linuxdeploy by default);
ALSA device configuration and audio drivers remain native to the host. Decoder
and device errors are visible and can be retried. Seek replaces the source on
the worker rather than waiting for a potentially disconnected output callback.
Audio samples are synthetic three-second tones, not external recordings.

JSON Canvas 1.0 opens as a native read-only scene with text, file, link and group
cards, directed edges and labels. File paths name exact vault-root identities;
note navigation retains heading/block intent through the ordinary Reader
resolver. Missing/unsupported targets stay visible. Link cards open only http(s)
on explicit activation. Group background images are identified but not previewed.

Fit to content, zoom buttons, Ctrl/Cmd + wheel and pinch control zoom. Ordinary
scroll and middle-button dragging pan. Offscreen cards are culled. Loading and
validation run off the UI thread with an 8 MiB source limit, at most 4000 cards
and 8000 edges, unique IDs and finite positive bounded geometry. Broken edge
references stay visible as a warning. Replacing/deleting a selected Canvas
uses the existing directory watcher and never leaves an editable source view.

Hosted native source checks are available in `viewers-native.yml` on
`viewers-native/**` refs or workflow dispatch. They execute decoder/mixer controls,
shipping image decoding, Reader dispatch, Canvas input/navigation and core fixture
tests on Windows and macOS. These complement the required exact-HEAD gh-check;
installed beta playback and visual evidence are recorded separately in issues
#2687–#2690.
