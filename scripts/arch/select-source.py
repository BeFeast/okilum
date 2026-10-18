#!/usr/bin/env python3
"""Reject incompatible selected control scripts before checking out product source."""
import argparse
import re
import subprocess


# A historical source may change product/configuration files, but cannot replace
# the packaging consumer with an older dispatcher/compiler. Compare Git objects,
# never import or execute code from the selected revision to discover support.
PATHS = ('scripts/arch/', 'scripts/ci/github-linux.py',
         'scripts/ci/github-macos.py', 'scripts/ci/package-linux-binary.py')


def tooling(revision, root=None):
    raw = subprocess.check_output(['git', 'ls-tree', '-r', revision, '--', *PATHS], text=True, cwd=root)
    return dict(line.split('\t', 1)[::-1] for line in raw.splitlines())


def validate(source, *, root=None):
    if not re.fullmatch('[0-9a-f]{40}', source):
        raise ValueError('Expected exact full selected source SHA')
    resolved = subprocess.check_output(['git', 'rev-parse', '--verify', source + '^{commit}'], text=True, cwd=root).strip()
    if resolved != source:
        raise ValueError('Selected source differs from requested commit')
    trusted = tooling('HEAD', root)
    if not trusted or tooling(source, root) != trusted:
        raise ValueError('Selected source has unsupported Arch consumer tooling; '
                         'no legacy helper, dispatch or compiler will be executed')
    return source


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source')
    print(validate(parser.parse_args().source))
