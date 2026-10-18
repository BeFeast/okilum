# Bounded local log windows (#1389)

Approved stage-6 scope: open the end of a large local log without scanning its
prefix, and bound Follow's retained data instead of raising the old 2 GiB limit.
This extends the reader's read-only log view. No source writes, remote inputs,
full-file streaming search, history browser, or new formats are introduced.

## Acceptance contract

Use the deterministic 1 GiB and 5 GiB Kibana/ECS fixtures from the measurement
report in issue #1389. Baseline and comparison run on each of muninn, VM151 with
passed-through Iris Xe, and Hedva, using one source revision across the builds.

- A 5 GiB file paints its last real rows within 5 seconds of launch. Measure
  painted rows in the native window; `LOG_READY` alone is not this measurement.
  The read probe must reject a reader that scans the whole file.
- Across open, enable Follow, and append, peak memory growth above an empty QA
  copy is at most 512 MiB. Measure RSS on Linux/macOS; measure both Working Set
  and private committed bytes on Windows. Resident-page trimming proves no cap.
- During ten minutes of continuous append, old data is evicted, recent complete
  rows remain visible, and memory does not grow with cumulative input size.
- Test record boundaries, unfinished and oversized records, UTF-8, absolute file
  offsets, stale reads, truncation, rotation, and append. Include negative controls
  for bounded IO and memory accounting. Do not weaken these gates after failures.

## Implemented storage and budgets

The initial large-file read is at most the last 64 MiB, plus a constant-size
format sample and boundary/identity guard. It seeks directly to that suffix;
there is no backwards search beyond the budget to recover an arbitrarily long
record. The first loaded record must have a proven head. An oversized or
unfinished record cannot trigger an expanding read buffer.

Raw bytes alone are not a sufficient budget: a 64 MiB file of tiny lines can
create an index larger than the process limit. Bound physical lines before
building an index (at most 262,144), and bound individual
record decoding. Keep real absolute byte origins separate from window-relative
index offsets. A line number counted inside a suffix is not a file line number.

Follow uses a sliding current window with bounded temporary reads. It must not
retain every prior window or every rotated incarnation. Background filters,
counts and exports must share immutable bounded snapshots where practical;
queueing or cloning unbounded generations is not an escape from the budget.
Small full-file behavior remains unchanged within the budget. Crossing the raw,
index or retained-content budget automatically switches to a tail window.

The implementation follows #1388's parser contract: the selected regex parser
travels with the index, filters, export and rotation. `LogSnapshot` shares raw
bytes, index and window origin through `Arc`; changing a retained generation
requires a new reservation before copying. The process-wide pool is 320 MiB,
charged for raw capacity, four index-sized working sets and 8 MiB parser/IO
scratch per snapshot. IO chunks reserve from the same pool before allocation.
Pool exhaustion defers Follow without advancing its cursor. Old background
readers retain their reservation until they finish; UI queues coalesce rather
than retaining an unbounded sequence of generations.

Records are limited to 64 KiB, including continuations. Before constructing
fields, a borrowing preflight bounds flattened keys, decoded values and field
vector capacity to 1 MiB per record. An offending record and its prefix are
excluded whole, leaving a contiguous suffix with an explicit scope notice.
Filter decoding uses at most four workers. Follow reads at most 4 MiB per poll
and seeks a fresh bounded suffix when a backlog exceeds the raw budget.
Rotation history has at most six recycled slots, restricted to 2 MiB source
segments / 12 MiB reservations; pressure discards history before the next poll.

Small compressed logs keep their existing decoding support. Compressed streams
that expand beyond 64 MiB are refused explicitly: seeking their suffix would
require scanning the prefix. Decoded offsets are labelled separately from
canonical file byte offsets. The zstd decoder window is capped at 8 MiB.
Static windows also use bounded owned reads. No mapping exposes canonical
files to truncation faults, and opening does not allocate a second mapped copy.

## Honest UI scope

Tail mode has a persistent, readable explanation: the end of the file is loaded;
older records were omitted or evicted; search, filters, counts and export cover
only that loaded window. Show oversized-record exclusions explicitly. Reset or
remap selection, marks and pending asynchronous results when eviction changes
their identities. Never label a window-relative line as a known whole-file line.

## Native evidence

Native acceptance for the integrated implementation is still pending. Unit
and integration results do not establish the native memory or paint gates.

Record build source, fixture hashes, empty-copy baseline, peak memory, first
painted-row timing and the ten-minute follow trace in the PR and issue. Keep the
positive controls visible: expected tail markers, confirmed on-disk appends,
working frame/row detection and an intentionally rejected full scan.
GUI sessions use whole-session locks and take place after night QA; they must
not consume the 22:45 QA window. Two meaningful attempts that cannot meet the
budgets require a blocker with the cause and retained evidence.
