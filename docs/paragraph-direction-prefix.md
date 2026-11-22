# Borrowed paragraph-direction classification

Refs [editing lag #2803](https://git.oklabs.uk/BeFeast/okilum/issues/2803).

`InputBaseState::paragraph_rtl_at` materialized the entire source row before
calling the first-strong Unicode bidi classifier. This also copied unchanged
long Markdown rows whose glyph layouts had already been reused. Plain code
files disable paragraph direction, so the earlier code-row controls did not
exercise this remaining copy.

The gpui-kit patch reads Rope characters directly and stops at the same first
L/R/AL character. The string helper delegates to that same iterator rule. Empty
or neutral-only paragraphs remain LTR; direction exemptions and disabled/code
paths still return before classification. There is no cached direction or
ASCII-only admission, so an edit to the first strong character is immediately
observable. A neutral-only row still requires a complete scan.

Per-editor opt-in counters record classification calls and source bytes prepared
for classification. They do not time frames or measure allocations/RSS. The
Reader regression uses two long Markdown rows with LTR/RTL direction and a
short edited row. It requires real layout reuse and direction calls, bounds
the prepared source to each row's short first-strong prefix, and retains exact
caret/hit geometry, Unicode/CRLF source, Undo/Redo and canonical save assertions.
Separate iterator controls compare the frozen old rule across chunk boundaries,
including directional controls, combining marks, emoji, Arabic and Hebrew, and
reject reads after the first strong character.

`scripts/ci/paragraph-direction-controls.py` restores the old full-row copy in
the generated vendor source, retaining only equivalent observational counters.
It requires the compiled Reader test to fail its named source-work assertion;
a compile error or zero tests cannot pass. It then restores the candidate and
requires the Reader and iterator controls to pass. The focused GitHub-hosted
workflow performs no installation or publication. A run result must be recorded
in the issue before claiming these controls passed.

The first hosted attempt (`45007812970`, head `90336039e`) exhausted the
3600-second control bound. Captured output had not been streamed, so its
compile/runtime stage cannot be determined from that log. Python termination
also bypassed its `finally`, leaving the old-copy source in the disposable
runner and causing the subsequent vendor verification to fail. That run is
neither negative-control nor candidate evidence.

Compilation now has the existing `shell-full` 840-second ceiling and reports
its exact test executable before a separate 120-second focused runtime phase.
The outer 3600-second bound is unchanged. Progress streams to the Actions log
and per-phase log artifacts. A byte-exact candidate backup and SHA-256 journal
are saved before the old-path mutation. An independent `always()` workflow
step restores and verifies them even if the control process was killed; this
does not turn the failed control step into success. Python orchestration tests
exercise the real supervisor against a TERM-resistant fake compiler, require
124 plus already-streamed progress, and verify independent restoration. They
also reject corrupt backups, zero tests and unrelated runtime failures. These
harness tests do not compile Rust or certify the product regression.

This is a source-work correction, not acceptance of the reporter issue or a
native latency, frame-budget or leak claim. Changed-row shaping, glyph copying,
Tree/Parser/Rope ownership and the other large-editor residuals remain open.
The patch changes only the pinned `longbridge/gpui-kit` input implementation;
it needs no core GPUI/tree-sitter API change. A general upstream proposal could
carry the borrowed-iterator rule and these controls after separate authorization.
