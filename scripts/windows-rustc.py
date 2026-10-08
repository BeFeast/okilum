#!/usr/bin/env python3
"""Give GPUI's runtime HLSL loader a portable packaged source directory.

Cargo's original crate source remains untouched. Only the env! value in the
Windows target library changes; Linux-hosted build scripts retain their real
manifest directory. Used by cross-build and native Windows PR packaging.
"""
import os
import sys

compiler, *args = sys.argv[1:]
if ('--crate-name' in args and args[args.index('--crate-name') + 1] == 'gpui_windows'
        and '--target' in args and args[args.index('--target') + 1] == 'x86_64-pc-windows-msvc'):
    os.environ['CARGO_MANIFEST_DIR'] = 'gpui-shaders'
cache = os.environ.get('TESSERA_SCCACHE')
if cache:
    os.execv(cache, [cache, compiler, *args])
os.execv(compiler, [compiler, *args])
