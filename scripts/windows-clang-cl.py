#!/usr/bin/env python3
"""Give libwebp's runtime-dispatched SSE4.1 units their upstream ISA flags.

Only the Linux-hosted Windows cross-build selects this adapter. libwebp-sys
0.9.6 puts every C source in one cc::Build, unlike upstream's separate SSE4.1
library in vendor/src/dsp/Makefile.am. Keep generic units at baseline x86_64.
"""
import os
from pathlib import Path
import sys


def compiler_args(args, environ):
    if (environ.get("CARGO_PKG_NAME") != "libwebp-sys"
            or environ.get("CARGO_PKG_VERSION") != "0.9.6"
            or environ.get("CARGO_CFG_TARGET_OS") != "windows"
            or environ.get("CARGO_CFG_TARGET_ARCH") != "x86_64"):
        return list(args)
    manifest = environ.get("CARGO_MANIFEST_DIR")
    if not manifest:
        return list(args)
    dsp = (Path(manifest) / "vendor/src/dsp").resolve()
    sources = [Path(arg).resolve() for arg in args if arg.endswith(".c")]
    if (len(sources) == 1 and sources[0].parent == dsp
            and sources[0].name.endswith("_sse41.c")):
        # SSE4.1 implies SSSE3. /clang: forwards the flag through clang-cl;
        # prepend it so cc-rs's '--' cannot turn it into a filename.
        return ["/clang:-msse4.1", *args]
    return list(args)


if __name__ == "__main__":
    os.execvp("clang-cl", ["clang-cl", *compiler_args(sys.argv[1:], os.environ)])
