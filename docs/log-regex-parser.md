# User regex log formats (#1388)

The log view's Regex menu creates a named definition and explicitly applies a
saved definition to the primary file, including files with a recognized format.
Applying reopens the file in a background job and stops follow; it keeps the
previous view if opening fails. Follow can then be enabled again with the chosen
parser, including after rotation or truncation.

Exactly one saved definition applies automatically to files detected as Plain.
With several definitions, detection stays Plain until the user chooses one. No
regex wins by list order. Use detected format restores detection for this open
view. Settings → Logs lists the definitions and deletes them; deleting a saved
definition leaves already open snapshots readable.

Definitions live in `log_regex` in the same durable `reader-ui.json` file as saved
log filters and log editor settings (normally `$XDG_STATE_HOME/okilum`, falling
back to `~/.local/state/okilum`; isolated sessions use their state directory).
They never live in the deletable index directory. Per-name changes participate
in the existing locked, atomic settings merge across windows.

The exact named groups `ts`, `level`, and `msg` have standard roles. All other
named groups are fields available from Columns. A group need not participate in
every match. Timestamp and level values use the existing parsers. Regexes use
the existing Rust `regex` dependency, including its supported syntax and limits.
A successful regex match is counted even without a level or timestamp. Invalid
values remain visible as fields; they are not invented timestamps or levels.

Every physical line, including a blank or indented unmatched line, stays a row
in regex mode. The status always gives matched and total line counts, plus the
unmatched count when nonzero. Regex mode does not group multiline crash blocks.
The compiled parser is shared by index workers, field decoding, filtering,
exports and follow; it is never compiled per line.

The #1155 sample pack is at
`crates/okilum-core/tests/fixtures/log/samples`. Its 17 files already have built-in
formats. The adjacent small `regex-pipe.log` fixture therefore exercises an
unknown format, query fields, histogram and elapsed time without altering the
original pack. Tests assert the pack inventory and detection as a positive
control for that choice.
