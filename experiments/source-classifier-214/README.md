# Source classifier parser probes

These standalone probes use the existing comrak 0.47.0 dependency from a core
build; they do not edit product source or a native buffer. The source-position
probe prints authored slices beside AST values to expose normalization and
coordinate exceptions. The cost probe measures five raw parse calls for each
identified synthetic input of at most 64 KiB. It measures parsing only, with AST
counting outside the timed interval; it does not establish a deadline or native
input latency. Results depend on host, profile and linked dependency artifact.

Compile each probe using `rustc --edition=2021`, an explicit system linker if the
host's `cc` is a shell wrapper, the core build's dependency directory via
`-L dependency=...`, and the matching comrak rlib via `--extern comrak=...`.
Record compiler/host, rlib hash, source hashes and output with the implementation
receipt. No Cargo graph or vendored dependency needs modification.
