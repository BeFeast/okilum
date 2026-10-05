#!/usr/bin/env python3
"""Keep resource-file references relative to the original Cargo crate root.

embed-resource runs llvm-rc from the .rc file's directory. GPUI's manifest
reference is relative to its crate root, so supply that root as a search path.
"""
import os
import sys

args = sys.argv[1:]
if '/?' not in args:
    args = ['/I', os.environ['CARGO_MANIFEST_DIR'], *args]
os.execv('/usr/bin/llvm-rc', ['llvm-rc', *args])
