# Native checks (`scripts/check-*-native.py`)

The native checks drive a real `okilum` binary under X11/Xvfb (CT141 or a
desktop session) and compare frames or logs.

## Build the app binary on its own

```sh
cargo build --release --locked -p okilum-shell --bin okilum
# examples, if a check needs one, in a second invocation:
cargo build --release --locked -p okilum-shell --example native_bidi737
```

Never build `--bin okilum` and an `--example` in one cargo invocation. Resolver
2 then compiles the app with okilum-shell's dev-dependency features, including
GPUI `test-support` (leak detection, test hooks), and the app behaves
differently: `check-lp-scroll-native.py` Source PageDown failed on every such
build and passed on every bin-only build of the same source (#1043).

## The guard

`okilum --build-info` prints `version`, `build` and `gpui-test-support: yes|no`;
the same fact is logged as `build_info` in `reader-diagnostic.log`. Every check
that takes the app binary calls `scripts/native_binary_guard.py` first: a binary
built with GPUI test-support is an **INVALID run** (exit 2), never a product
failure. A binary from before `--build-info` gets a warning and is not verified.
