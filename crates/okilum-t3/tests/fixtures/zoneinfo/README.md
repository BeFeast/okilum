# Native rotation timezone fixture

`Asia/Jerusalem` is the compiled IANA tzdb 2026a zone, copied from the
system tzdata distribution (public-domain timezone data), not a synthetic
replacement for the DST schedule. Size 2388 bytes; SHA256
`254b964265b94e16b4a498f0eb543968dec25f4cf80fba29b3d38e4a775ae837`.

The rotation recovery tests use this explicit database root on every OS rather
than assuming a Unix `/usr/share/zoneinfo` directory on Windows. They retain
the real 2026 spring gap/autumn fold, normal-date, path-traversal and coverage
assertions. Production timezone selection is unchanged.

To regenerate, compile the `asia` source from IANA tzdb 2026a using `zic`, then
copy only the resulting `Asia/Jerusalem` file here. A tzdb update must retain
these dated assertions or explain an actual schedule change.
