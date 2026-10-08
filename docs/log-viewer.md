# Log viewer

Tessera can open structured log files and show them one record per row, with
the time, the level and the message lined up, and every field of the selected
record underneath. The viewer only reads: it never changes, moves or deletes a
log file.

Features marked **Coming** are planned but not in Tessera yet.

## Opening a log

Tessera opens files that end in `.log`, `.jsonl`, `.ndjson` or `.logfmt`
(upper or lower case). The contents decide how the file is shown, not the
name:

- **JSON lines**: one JSON object per line.
- **logfmt**: one record per line, written as `key=value` pairs.
- **JSON lines and logfmt**: a file that mixes both; each line is read in its
  own format.
- **Plain text**: anything else. Each line is a row.

Ways to open a log:

- Click **Open file…** and choose the log.
- From a terminal, run `tessera path/to/app.log`.
- In a folder opened in Tessera, click the log in the file list, or find it
  with Quick Open.
- From your file manager:
  - **macOS:** in Finder, Control-click the log and choose **Open With ▸
    Tessera**. Tessera is offered as an alternative app only; double-clicking
    a log still opens it in your usual app.
  - **Linux:** in your file manager, choose **Open With Other Application**
    and pick Tessera. Tessera does not add itself to the short "Open With" list
    for logs on Linux, because on many desktops that would make it the app that
    opens every `.log` file on double-click.
  - **Windows:** **Coming.** Tessera will appear under **Open with** in File
    Explorer without becoming the default app.

Tessera reads the file as it is when you open it. Lines written to the log
afterwards appear the next time you open it (see Follow below).

## Reading records

- **Rows.** Each row shows the time, a level badge, the logger name and the
  message, followed by the record's other fields in a lighter colour. Long rows
  end in "…"; the full record is in the detail pane.
- **Levels.** Common spellings are understood, such as `WARN`, `warning`, `err`,
  `crit` or the numbers used by pino and bunyan (10–60). A record without a
  level shows `---`. A line that is not a record in the file's format is kept
  and shown with a `RAW` badge: nothing is hidden or dropped.
- **Times** are shown in UTC. A time written without a time zone is shown as
  written.
- **Detail pane.** Select a row with the mouse or the ↑ and ↓ keys to see all of
  its fields in their original order and spelling. Nested JSON objects are shown
  as dotted names, for example `user.id`.
- **Copy raw line.** Press ⌘C on macOS or Ctrl+C on Windows and Linux, or click
  the copy button, to copy the selected line exactly as it appears in the file.
- **Status bar.** Shows the detected format, the number of records, how many
  lines could not be read as records, whether the file has no levels at all,
  and the file size.

## Filters

**Coming.** A filter bar above the rows will offer:

- a level filter (for example "warnings and above"), with a switch for
  records that have no level;
- a time range relative to the last record in the file (last 5, 15 or 60
  minutes, or a custom range);
- text search;
- buttons in the detail pane that add a field as a filter, such as
  `status = 500`.

A counter will show how many records match out of the total.

## Query syntax

**Coming.** A query field will accept a subset of the query language of
[hl](https://github.com/pamburus/hl), so most queries work in both tools.
Tessera already understands this syntax; the field to type it in is coming.

| You write | It matches records where |
|---|---|
| `status=500` | the field `status` is 500 |
| `status!=200` | `status` exists and is not 200 |
| `duration_ms>=800` | `duration_ms` is a number of at least 800 (also `>`, `<`, `<=`) |
| `msg~="timeout"` | the message contains `timeout` (`!~=` for does not contain) |
| `path~~="^/v1/"` | `path` matches a regular expression (`!~~=` for does not match) |
| `level>=warn` | the level is warning or more severe |
| `method in (GET, HEAD)` | `method` is one of the listed values (also `not in`) |
| `exists(request_id)` | the record has a `request_id` field |
| `user.role?=owner` | `user.role` is `owner`, or the record has no `user.role` |

Combine tests with `and` (or `&&`), `or` (or `||`), `not` (or `!`) and
parentheses. `not` binds tightest, then `and`, then `or`.

- `level`, `msg` (or `message`) and `logger` refer to the record's level,
  message and logger, whichever spelling the log uses (for example `severity`
  or `lvl` for the level). To test a field that is literally named `level`,
  write `.level`.
- Other field names are exact and case-sensitive. Use dots for nested fields:
  `user.id`.
- Put values with spaces or special characters in quotes: `msg="cache miss"`.
- A test on a field the record does not have does not match, including `!=`.
  Add `?` after the field name to include those records too.
- Unlike hl, Tessera does not read a list of values from a file
  (`in @file`), and does not support `like` patterns.

## Follow

**Coming.** A **Follow** switch will keep the view at the end of the log and
show new lines as they are written. Scrolling up will pause it, and a "new
lines" button will jump back to the end. If the log is cleared or replaced by
log rotation, a marker row will show where that happened, and earlier records
stay visible above it.

## Compressed logs

**Coming.** Logs compressed with gzip (`.gz`), zstd (`.zst`) or bzip2 (`.bz2`)
will open directly. Support for xz is still being decided. Until then,
decompress the file first and open the result.

## More than one file

**Coming.** You will be able to open several logs at once and read them merged
in time order, with a coloured stripe showing which file each row came from.

## Tessera's own diagnostic log

**Coming.** A **Help ▸ Open diagnostic log** command will open the log Tessera
writes about its own startup and scans, so you can attach it to a bug report.

## Limits

- Files up to 2 GiB (about 2.1 GB) open. Larger files are refused with a
  message; Tessera never shows part of a file without saying so.
- The whole file is read when it is opened. Very large files take a moment
  before the rows appear, and use memory roughly in proportion to their number
  of lines.
- The viewer is read-only. To change a log, use another app.
