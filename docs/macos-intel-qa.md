# Intel macOS QA builds

The `macos-intel-qa` workflow cross-compiles on the existing Apple Silicon runner
for `x86_64-apple-darwin`. It runs manually or at 01:30 UTC, with no push trigger.
It uses a separate target cache and concurrency group. The single macOS runner
still serializes jobs; a running QA build occupies its slot until completion.
The hourly arm64 release workflow and publisher are unchanged.

The artifact contains a Developer ID signed `Tessera Intel QA.app`, its ZIP and
build time/executable size metrics. It is not notarized. Its separate bundle ID
and absence of `SUFeedURL` prevent the QA app from consuming the arm64 appcast.
It is never handed to the public release publisher.

The deployment target and `LSMinimumSystemVersion` are 12.0. Before signing,
every bundled Mach-O must contain an Intel slice with a macOS minimum no newer
than 12.0. Actual compatibility requires a launch on macOS 12; compilation and
metadata checks alone do not prove Metal or runtime API compatibility.

For native QA, extract outside `/Applications`, use a disposable vault, record
startup diagnostics, and close the test process without altering the user's
normal app or preferences. Report source/build, M4 build seconds, executable
bytes, macOS version and launch outcome. A failed launch is a blocker.
