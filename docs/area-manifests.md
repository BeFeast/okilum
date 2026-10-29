# Area manifests: TOML with a compatible transition (#1859)

An Area may contain `ops/project.toml` and its optional `ops/board.toml`.
Each is preferred independently over the corresponding `.yaml` file. Only an
absent TOML file permits fallback. Invalid syntax, unreadable files and symlinks
remain visible errors; they cannot select a stale YAML copy. Discovery and
existing project operations share this rule.

Project fields retain their existing meaning. Unknown project metadata is
preserved; Board fields remain validated. A minimal project is:

```toml
name = "Example"
kind = "knowledge"

[repo]
host = "git.oklabs.uk"
full_name = "BeFeast/example"
```

The [Board example](examples/okilum-board.toml) shows the ordered `[[issues]]`
tables. `notes` accepts area-relative Markdown paths and credential-free HTTPS
links. One malformed card, duplicate issue or unsafe related link produces a
problem card with its field and precise error; healthy cards still render.
Root parse/repository errors appear with their exact detail in the Board problem
line. Root and card diagnostics also go to the app log. The last valid source
snapshot retains its age and cannot masquerade as a successful new read.

TOML has no null: nullable mapping entries are omitted. In `t3.manager`, omitted
`project_id` and `thread_id` mean unbound. Legacy YAML keeps its explicit-null
validation. Existing icon and T3-link edits update the selected format only;
TOML comments and unrelated values are preserved, with revision checks and
same-directory atomic replacement. During the rollout, creating a new project
still writes YAML so older deployed builds can discover it. No read converts or
deletes a file and no normal app operation rewrites both copies.

## One-shot conversion

The manager runs the converter on the real vault **only after every Mac, Linux
and Windows beta/Stable channel Oleg uses supports these readers**. Until then,
keep the existing YAML files. The executor tests conversion on a separate copy.
Vault-side scripts and skills migrate in a separate manager step. Conversion or
deletion of real vault YAML is not part of shipping this reader change.

During rollout, #1862 blocks removing the last card from a legacy YAML Board:
older readers reject an empty `issues` array and would show “Board unavailable”.
The refusal leaves the file unchanged and explains that another card can be
added first. Other legacy edits/removes remain available; TOML Boards may become
empty. Remove this temporary guard together with the manager's vault migration
after every deployed reader supports TOML and empty Boards. Do not convert a
Board or write both formats to bypass this guard.

```sh
python3 -m venv /tmp/okilum-manifests
/tmp/okilum-manifests/bin/pip install -r scripts/manifests/requirements.txt
/tmp/okilum-manifests/bin/python scripts/manifests/convert.py /path/to/copied/Areas
# Inspect the dry-run JSON before explicitly creating TOML in that copy:
/tmp/okilum-manifests/bin/python scripts/manifests/convert.py /path/to/copied/Areas --write
```

Dry-run is the default. `--write` atomically creates absent TOML files, preserving
YAML bytes, permissions and modification times. An existing semantically equal
TOML is unchanged; a different file or a symlink is refused, including a
concurrent creator. The converter never deletes YAML. It reports each omitted
null, numeric key converted to a string and date converted to an ISO string.
Original YAML comments are retained with their field context in the TOML header;
omitted nulls retain an explicit comment. Comment placement may change, while
literal `#` text in descriptions remains data. Unsupported null array elements,
cycles, duplicate/colliding keys and nonfinite numbers fail explicitly.

The converter checks parsed TOML against the normalized original before writing.
It skips symlinked directories and service/cache folders. The report lists every
selected manifest and its result; an error yields a nonzero exit code. The
shared CI gate runs the converter regressions with pinned Python dependencies.
