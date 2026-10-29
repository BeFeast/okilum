# Generated vendor files and formatting

`scripts/vendor-setup.sh` recreates the pinned gpui-kit clone by applying patches
in filename order. `--verify` compares its exact tracked bytes against that full
stack using a disposable Git index. Formatting is not part of generation.

`cargo fmt --all` includes local path dependencies even though `vendor` is
excluded from the Okilum workspace. Without a fence it reformats patched input
and text sources, and the next vendor verification correctly rejects the drift.
Ordinary workspace formatting can also traverse generated files imported with
`#[path]`. Workspace exclusion alone does not protect either route.

Patch 0098 sets stable rustfmt's `disable_all_formatting` in the generated
clone's own config. Direct downstream vendor module declarations use
`#[rustfmt::skip]`. This is a local generation policy, not an upstream behavior
patch. Format intentional changes before producing a patch; never format the
assembled generated clone as a source-editing step. Vendor verification still
rejects arbitrary edits, including whitespace changes.

Both shared CI routes run `python3 scripts/ci/check-vendor-generation.py` before
compilation. It copies tracked source into an isolated directory, seeds an
unpatched clone from the existing local Git object store, then runs setup,
verification, ordinary and recursive formatting, repeated setup/verification,
and a deliberate source-tamper rejection. It neither compiles nor touches the
caller's generated tree or Cargo target. The regular checksum-pinned comrak
bootstrap remains part of fresh setup.
