# Kubernetes CRI parser (#1384)

The local Log view detects raw CRI envelopes:

```text
2026-10-10T06:00:00.123456789Z stdout P hello
2026-10-10T06:00:00.123456790Z stdout F world
```

The parser exposes `time`, `stream`, `tag` and `msg`. It accepts zoned RFC3339
nanosecond timestamps, `stdout`/`stderr` and `P`/`F`; a stream is not a severity.
Adjacent same-stream P fragments and their terminating F form one record, with
payloads concatenated without added whitespace. The first timestamp, source
line and byte offset identify the record. Raw copy/export retains every envelope
and original line ending. The assembled tag is F only after a terminating F.
An EOF partial stays visible with tag P. Malformed envelopes remain unparsed rows.

The index stores a contiguous source range per entry. Consequently a stream
switch, malformed envelope or blank physical line ends a fragment group; the
preceding unfinished group retains P. Interleaved streams are never concatenated
through each other. Full noncontiguous per-stream assembly requires an explicit
source-range model extension and is not delivered here. CRI indexing currently
uses one worker to keep fragment boundaries deterministic; other formats retain
parallel indexing and existing multiline behavior.

The existing multiline line limit and snapshot byte/decode budgets still apply.
No Kubernetes connection, remote ingestion or write operation is added.
