#!/usr/bin/env python3
"""Conservative native-check selection; input is git diff --name-only -z."""
import sys


def needs_macos(paths):
    # Only documentation is exempt. Unknown/new build inputs fail closed.
    return any(not (path.endswith('.md') and
                   (path.startswith('docs/') or '/' not in path)) for path in paths)


if __name__ == '__main__':
    paths = sys.stdin.buffer.read().decode('utf-8', errors='surrogateescape').split('\0')
    print('true' if needs_macos([path for path in paths if path]) else 'false')
