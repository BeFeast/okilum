# Windows marker setup proof, before any new benchmark collection

Capture-only run 43358434634 completed the Mac census (10 launches), but Windows
failed before runner.py: xperf 10.0.26100.7705 rejected the braced provider token
with `Invalid flags (0x3ec)`. Windows measured launches: zero. The short kernel
ETL contains no benchmark and cannot attribute any historical timing sample.
The Windows artifact, job log and Mac results are retained unchanged.

The suspected correction removes braces from the xperf user-provider token.
Microsoft's [Xperf logging example](https://learn.microsoft.com/en-us/dotnet/framework/performance/controlling-logging)
uses an unbraced GUID; the adjacent Logman example uses braces. The saved actual
WPT help accepts `GUID:Flags:Level`. Documentation is not executable proof.

`toolbar-2811-marker-setup.yml` is an independent Windows-only, setup-only job:
no native source checkout, producer artifact download, build, runner.py census,
Mac job or Okilum launch. It runs only when root deliberately pushes a reviewed
controller to `diagnostic-toolbar-2811/marker-setup-*`. It is not a benchmark
retry or a native performance pass. Nothing automatically continues to census.

The synthetic Python probe requires the exact installed xperf SHA256 from the
failed setup. It first expects the old braced spelling to fail with Invalid
flags, then starts the same named user session with the unbraced spelling. It
emits one EventWriteString through the same provider GUID/level4/keyword1,
stops only its own named session, and decodes the retained ETL with the existing
ProcessTrace RAW_TIMESTAMP exporter. It requires exactly one marker with the
correct provider, PID, TID and UTF-16 payload, and a raw event timestamp within
the emission's QPC bracket. Frequency/clock domains must match; losses and
exporter errors fail. `proof.json` retains command argv/output/status and reports
zero benchmark launches even on failure. No priority, timer or stack setting is
changed.

The malformed spelling unexpectedly succeeding, a different WPT binary, missing
marker or decode error stops the proof; there is no fallback or benchmark launch.
Only after root reviews actual proof artifacts may a separate Windows-only
first benchmark collection be prepared and launched. The completed Mac census
must never be repeated as part of this setup repair.

Offline controls verify exact argument spelling and reject wrong provider,
PID/TID, payload, QPC bracket, duplicate/missing marker, clock and frequency.
They do not establish that actual xperf accepts the command. At preparation,
35 Python controls passed; the native synthetic roundtrip is NOT RUN.
