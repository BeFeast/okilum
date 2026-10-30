# Hosted gate evidence when the jobs API omits steps

The bridge still requires the exact source, unique invocation branch, workflow,
completed successful run, and the expected successful job. A successful run
summary alone is never sufficient.

On 30 October a bounded audit found empty `steps` on 7 of 17 recently successful
jobs. Known completed jobs were still missing steps after 15–32 minutes, while
other simultaneous jobs returned normal data. Main `4b16a3d0` and several PRs
failed closed despite successful suite logs. The observed first affected
completion was 17:37:12 UTC; that is not a proven onset time. Evidence is tracked
in #1815 and `qa-evidence/1815/steps-gap/`.

After the existing three evidence reads, **only** a single expected job with
`status=completed`, `conclusion=success`, `steps=[]`, and matching source/run
identity may use a job-log fallback. Nonempty pending/failed/skipped step
projections cannot be overridden. Missing/ambiguous jobs still fail.

Instrumented gate commands run through `scripts/ci/gate-step.py`. It emits
`OKILUM_GATE_V1 BEGIN` immediately before executing the actual command and
`OKILUM_GATE_V1 END` after its exit. Both carry exact source SHA, GitHub run ID,
run attempt, job name, and expected step name; END also carries the command's
exit code. A failure keeps its nonzero exit; interruption has no successful END.
The parser requires exactly one ordered matching pair with exit 0. Wrong
identities, duplicate markers, missing step/end, nonzero exit, invalid UTF-8,
and over-bound logs fail closed. Echoed commands or a bare “tests passed” line
do not prove a gate.

The fallback downloads one exact job log (32 MiB maximum, 30 s network timeout).
API credentials are sent only to api.github.com. A log-storage redirect is
restricted to HTTPS Azure/GitHub Actions storage, and a new request drops the
Authorization header. Logs are parsed in memory, not printed; success reports
the job ID and log SHA256. There is no new workflow dispatch or retry on this
path. A missing log remains unavailable/failure.

Initial instrumentation covers shared, macOS (hosted and depot), the aggregate
Linux gate (which still requires both full Linux checks and AppImage success),
and Windows package validation. Other lanes retain their existing steps-only
policy. Old unmarked logs cannot retrospectively satisfy the new fallback.
Shipping/notarization receipt guards and Stable publication are unchanged.

Regression coverage exercises normal API steps, empty steps plus valid log,
missing both evidence sources, logs missing the expected step, wrong identities,
nonzero/missing/duplicate/reversed markers, actual wrapper exits, and redirect
credential isolation. No new Python dependency is required.
