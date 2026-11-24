# View snapshot HTML profile 1

Each JSON file is a sealed C2 view snapshot. Its sibling HTML file is the
complete expected output of `okilum-cored render --profile mobile
--view-snapshot <file.json>`. The seven files named after `view-golden` inputs
preserve those results, with snapshot version 1 and a fixed UTC seal time.
The synthetic `activity` pair also covers section errors, cached section
time in daylight saving time, typed links, diagnostics and truncated groups.

Source navigation uses `okilum-note:<path>?revision=<rev>&line=<line>#<anchor>`.
Split the fragment and query before decoding the path and revision. The
one-based line and profile anchor are optional. Use the line with the exact
source revision. If the host opens newer bytes, it can try the anchor and
show that the source changed. Cell links with a known path use the regular
note profile URL. Ambiguous cells retain the label and show separate links
to the supplied candidates.

Excerpts resolve relative paths from the source row. Bare wiki names need
a vault inventory and appear unresolved in the sealed HTML. Task state
comes from projected `checked` values or the Markdown excerpt.
