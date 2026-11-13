# First collections after setup-only failures

Producer run43351244577 built all four immutable binaries at controller
6d073b12ef12b43dc67d35af3794b5a83e886d55. Both subsequent collection jobs failed
before their first benchmark launch. Windows produced no ETL; Mac produced
no CPU receipt. These failures are retained as NO_EVIDENCE, never timing data.

The saved WPT10.0.26100.7705 start help says:

```text
-ClockType ClockType  Set the clock type; default: PerfCounter.
Type: Cycle, PerfCounter, SystemTime.
```

The repaired commands use exactly PerfCounter. WPT preflight also verifies
that value is advertised. No performance option, clock or budget changes.

Mac no longer assumes inherited nice equals numeric zero. It records inherited
nice/RLIMIT before assertions, persists failures, never sets priority or limits,
checks parent state unchanged before/after every app and at postflight, and
records child initial nice where getpriority permits. Reports compare both
sources' observed child nice values without inventing a normal-host value.

toolbar-2811-capture-only.yml has no build jobs. It downloads the four exact
artifact IDs from the producer run with actions:read; capture-inputs.json pins
names, archive/binary hashes, source SHAs and producer controller. The guard
verifies binary/source/diff identities and default stack policy. It records the
NEW capture controller separately, setup_attempt=2 and capture_attempt=1.
Sources remain main718 and feature680, not the newer comparator-only393 head.

The same five alternating pairs, 480frames per source/platform, all original
wall/glyph/functional guards and nonzero final budget verdict remain in force.
The original full workflow is not rerun. After real collection, any exporter
or analyzer correction must reuse those exact ETLs/receipts, never recapture.

Only root may launch after review, with launch-capture-only-reviewed.sh. It
pushes just the repaired controller to a NEW capture-only-* ref; it pushes no
source ref and triggers no original build/Mac workflow. Nothing has launched
from this repair branch during preparation.
