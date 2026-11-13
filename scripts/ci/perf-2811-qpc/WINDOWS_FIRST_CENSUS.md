# First Windows benchmark collection after proven setup

The standalone marker-only [proof run](https://github.com/BeFeast/okilum/actions/runs/43359355912)
passed at controller `fca5762a39adca596cb378952b08e30e64a29c3b`.
WPT SHA256 was exactly `6e0c15d303e9369a8756b7f9b63978220ba3c8f9b9471a00b0e63958cc095561`.
The old braced token failed with Invalid flags 0x3ec; the same arguments with an
unbraced GUID started/stopped successfully. ProcessTrace RAW_TIMESTAMP decoded
one correct provider/PID/TID/UTF-16 marker within the emission's QPC bracket,
with matching 10MHz frequency, clock 1 and zero losses/errors. Benchmark launches 0.
This proves setup syntax, not app performance or historical scheduler state.

The Windows-only workflow downloads exactly the original main/toolbar producer
artifacts and the retained proof artifact 14006973394. It verifies the proof's
exact bytes SHA256 `d9763d624b6904e8fbe9d620ba9a383be0f19e0e471be8b2e640dad9d16950bf`
and requires the capture worker's actual xperf hash to match the proof. There is
no binary rebuild or native source modification. Provenance records setup 3,
capture 1, both failed setup jobs, the setup proof and new capture controller.

Root alone may push a reviewed controller to
`diagnostic-toolbar-2811/windows-first-*`; nothing auto-launches this workflow
from a setup proof. No Mac job exists here. The completed Mac collection is
retained and must not be repeated. The earlier Windows setup failures each
measured zero benchmark launches, so this is its first benchmark census.

The only trace-command correction is removal of the provider GUID braces.
Default QPC clock, scheduler/stack flags, buffers, priorities, stack reserve,
10 alternating app launches, 480 frames per source, all original functional and
wall/glyph budgets and strict nonzero exit are unchanged. Two sanitized
build-info probes finish before trace start. Strict budget failures still
complete collection and leave the job red.

After collection, retain and reuse its raw ETLs for exporter/analyzer repairs;
never recapture to fix analysis. Uploaded artifact paths contain diagnostics/
and proven-setup/ so the original setup proof remains alongside the collection.
At preparation 37 Python controls pass, and the real frozen Windows binary/stack/
observer-diff guard passed offline without execution. The Windows census is NOT
LAUNCHED at preparation; root review is still required.
